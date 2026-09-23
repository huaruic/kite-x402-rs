//! Kite x402 service template (Rust + Axum).
//!
//! Wraps an existing HTTP API behind x402 payments settled on the Kite chain.
//! Requests to `/v1/*` return HTTP 402 until the caller attaches a valid
//! `PAYMENT-SIGNATURE`; the payment is verified by the facilitator, the request
//! is proxied to `UPSTREAM_URL`, and the payment is settled only if the upstream
//! answered with a non-error status.

mod kite;
mod x402;

use std::sync::Arc;

use axum::{
    Router,
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, HeaderName, Method, StatusCode, Uri, header},
    response::{IntoResponse, Response},
    routing::{any, get},
};

use kite::KiteChain;
use x402::{
    Facilitator, HEADER_PAYMENT_REQUIRED, HEADER_PAYMENT_RESPONSE, HEADER_PAYMENT_SIGNATURE,
    PaymentRequired, PaymentRequirements, ResourceInfo, SettleResponse,
};

/// Headers that describe one hop and must not be forwarded, plus the two that
/// would leak or corrupt the proxied exchange:
///
/// - `payment-signature` is ours to consume; the upstream must never see it.
/// - `accept-encoding` is dropped so the upstream answers in identity encoding.
///   The body is passed through verbatim, so forwarding a compressed response
///   under a stripped `content-encoding` would hand the client bytes it cannot
///   decode.
const STRIP_FROM_REQUEST: &[&str] = &[
    "connection",
    "keep-alive",
    "transfer-encoding",
    "te",
    "trailer",
    "upgrade",
    "host",
    "content-length",
    "payment-signature",
    "accept-encoding",
];

const STRIP_FROM_RESPONSE: &[&str] = &[
    "connection",
    "keep-alive",
    "transfer-encoding",
    "te",
    "trailer",
    "upgrade",
    "content-length",
];

struct App {
    chain: KiteChain,
    /// Built once at startup from our own configuration. Never from the client.
    requirements: PaymentRequirements,
    price_label: String,
    description: String,
    upstream: reqwest::Url,
    upstream_auth_header: String,
    upstream_auth_value: Option<String>,
    facilitator: Facilitator,
    http: reqwest::Client,
}

fn env(key: &str, fallback: &str) -> String {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| fallback.to_string())
}

fn build_app() -> Result<App, String> {
    let pay_to = env("PAY_TO", "");
    if pay_to.is_empty() {
        return Err("PAY_TO is required: the Kite wallet address that receives payments".into());
    }
    let chain = kite::chain_by_name(&env("KITE_NETWORK", "mainnet"))?;

    let upstream_raw = env("UPSTREAM_URL", "");
    let upstream = reqwest::Url::parse(&upstream_raw)
        .map_err(|_| "UPSTREAM_URL is required, e.g. https://api.example.com".to_string())?;
    if !matches!(upstream.scheme(), "http" | "https") || !upstream.has_host() {
        return Err("UPSTREAM_URL is required, e.g. https://api.example.com".into());
    }

    let price_raw = env("PRICE_USD", "0.001");
    let price_label = if price_raw.starts_with('$') {
        price_raw.clone()
    } else {
        format!("${price_raw}")
    };
    // Fail at startup, not on the first paid request: a service that answers
    // /healthz but 500s every purchase is the worst failure mode here.
    let amount = kite::price_to_units(&price_label, &chain)?;

    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| format!("could not build HTTP client: {e}"))?;

    Ok(App {
        requirements: PaymentRequirements {
            scheme: "exact".into(),
            network: chain.network.into(),
            asset: chain.asset_address.into(),
            amount: amount.to_string(),
            pay_to,
            max_timeout_seconds: 60,
            extra: Some(serde_json::json!({
                "name": chain.eip712_name,
                "version": chain.eip712_version,
            })),
        },
        chain,
        price_label,
        description: env(
            "SERVICE_DESCRIPTION",
            "Paid API wrapped for the Kite network",
        ),
        upstream,
        upstream_auth_header: env("UPSTREAM_AUTH_HEADER", "Authorization"),
        upstream_auth_value: std::env::var("UPSTREAM_AUTH_VALUE")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty()),
        facilitator: Facilitator::new(env("FACILITATOR_URL", kite::FACILITATOR_URL), http.clone()),
        http,
    })
}

#[tokio::main]
async fn main() {
    let app = match build_app() {
        Ok(app) => Arc::new(app),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };

    let port = env("PORT", "8080");
    let addr = format!("0.0.0.0:{port}");
    println!(
        "kite x402 service on :{port} -> {} (network {}, {} per call to {})",
        app.upstream, app.chain.network, app.price_label, app.requirements.pay_to
    );

    let router = Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/{*path}", any(paid))
        .with_state(app);

    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("could not bind {addr}: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = axum::serve(listener, router).await {
        eprintln!("server error: {e}");
        std::process::exit(1);
    }
}

/// Free, so load balancers and the catalog can probe the service.
async fn healthz(State(app): State<Arc<App>>) -> Response {
    axum::Json(serde_json::json!({
        "ok": true,
        "network": app.chain.network,
        "asset": app.chain.asset_symbol,
        "price": app.price_label,
    }))
    .into_response()
}

/// Every method under `/v1/` is paid and proxied.
///
/// Ordering is the one rule this whole template exists to get right:
/// verify -> upstream -> settle, and settle only when the upstream succeeded.
/// Charging before the upstream answers is the number one complaint agents have
/// about paid APIs.
async fn paid(
    State(app): State<Arc<App>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let resource = resource_info(&app, &headers, &uri);

    let Some(raw) = headers.get(HEADER_PAYMENT_SIGNATURE) else {
        return challenge(&app, resource, "PAYMENT-SIGNATURE header is required");
    };
    let Ok(raw) = raw.to_str() else {
        return challenge(
            &app,
            resource,
            "PAYMENT-SIGNATURE header is not valid ASCII",
        );
    };

    let payload = match x402::decode_payment_signature(raw) {
        Ok(p) => p,
        // Malformed input from the caller, not a payment problem: 400, and no
        // 402 challenge, because retrying the same bad header will not help.
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };

    match app.facilitator.verify(&payload, &app.requirements).await {
        Err(e) => return (StatusCode::BAD_GATEWAY, e.to_string()).into_response(),
        Ok(v) if !v.is_valid => return challenge(&app, resource, v.rejection()),
        Ok(_) => {}
    }

    let upstream = match forward(&app, &method, &uri, &headers, body).await {
        Ok(r) => r,
        // 502 is >= 400, so nothing is settled and the caller keeps its money.
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                axum::Json(serde_json::json!({"error": "upstream unreachable", "detail": e})),
            )
                .into_response();
        }
    };

    // The upstream refused. Hand the caller its answer, unpaid.
    if upstream.status.is_client_error() || upstream.status.is_server_error() {
        return upstream.into_response(None);
    }

    let settled = match app.facilitator.settle(&payload, &app.requirements).await {
        Ok(s) => s,
        Err(e) => return settlement_failed(SettleResponse::default(), e.to_string()),
    };
    if !settled.success {
        let reason = settled.failure();
        return settlement_failed(settled, reason);
    }

    upstream.into_response(Some(&settled))
}

/// 402 with the `PAYMENT-REQUIRED` challenge that tells a client what to sign.
fn challenge(app: &App, resource: ResourceInfo, error: impl Into<String>) -> Response {
    let required = PaymentRequired::new(error, resource, app.requirements.clone());
    let mut response = (StatusCode::PAYMENT_REQUIRED, axum::Json(&required)).into_response();
    match x402::encode_header(&required) {
        Ok(encoded) => match encoded.parse() {
            Ok(value) => {
                response
                    .headers_mut()
                    .insert(HEADER_PAYMENT_REQUIRED, value);
            }
            Err(_) => return internal("could not encode PAYMENT-REQUIRED header"),
        },
        Err(e) => return internal(&format!("could not build PAYMENT-REQUIRED header: {e}")),
    }
    response
}

/// The upstream already answered, but the money did not move, so the caller does
/// not get the body. The spec carries the reason in `PAYMENT-RESPONSE` on a 402.
fn settlement_failed(mut settled: SettleResponse, reason: String) -> Response {
    settled.success = false;
    if settled.error_reason.is_none() {
        settled.error_reason = Some(reason.clone());
    }
    let mut response = (
        StatusCode::PAYMENT_REQUIRED,
        axum::Json(serde_json::json!({"error": "settlement failed", "detail": reason})),
    )
        .into_response();
    if let Ok(encoded) = x402::encode_header(&settled)
        && let Ok(value) = encoded.parse()
    {
        response
            .headers_mut()
            .insert(HEADER_PAYMENT_RESPONSE, value);
    }
    response
}

fn internal(message: &str) -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, message.to_string()).into_response()
}

fn resource_info(app: &App, headers: &HeaderMap, uri: &Uri) -> ResourceInfo {
    // Kite Passport reaches the service over public https, so that is the URL a
    // client signs over. PUBLIC_BASE_URL covers proxies that rewrite Host.
    let base = match env("PUBLIC_BASE_URL", "") {
        b if !b.is_empty() => b.trim_end_matches('/').to_string(),
        _ => {
            let host = headers
                .get(header::HOST)
                .and_then(|h| h.to_str().ok())
                .unwrap_or("localhost");
            format!("https://{host}")
        }
    };
    ResourceInfo {
        url: format!("{base}{}", uri.path_and_query().map_or("", |p| p.as_str())),
        description: Some(app.description.clone()),
        mime_type: Some("application/json".into()),
    }
}

/// What came back from the API we are wrapping.
struct UpstreamResponse {
    status: StatusCode,
    headers: HeaderMap,
    body: Bytes,
}

impl UpstreamResponse {
    fn into_response(self, settled: Option<&SettleResponse>) -> Response {
        let mut response = Response::builder().status(self.status);
        for (name, value) in self.headers.iter() {
            if STRIP_FROM_RESPONSE.contains(&name.as_str()) {
                continue;
            }
            response = response.header(name, value);
        }
        let mut response = match response.body(Body::from(self.body)) {
            Ok(r) => r,
            Err(e) => return internal(&format!("could not relay upstream response: {e}")),
        };
        if let Some(settled) = settled
            && let Ok(encoded) = x402::encode_header(settled)
            && let Ok(value) = encoded.parse()
        {
            response
                .headers_mut()
                .insert(HEADER_PAYMENT_RESPONSE, value);
        }
        response
    }
}

/// Proxies a paid request to the wrapped API.
///
/// `/v1/forecast?x=1` reaches the upstream as `<UPSTREAM_URL>/forecast?x=1`. The
/// upstream credential is injected here and never reaches the caller.
async fn forward(
    app: &App,
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    body: Bytes,
) -> Result<UpstreamResponse, String> {
    let path = uri.path().strip_prefix("/v1").unwrap_or(uri.path());
    let mut target = app
        .upstream
        .join(path.trim_start_matches('/'))
        .map_err(|e| format!("could not build upstream URL: {e}"))?;
    target.set_query(uri.query());

    let mut forwarded = reqwest::header::HeaderMap::new();
    for (name, value) in headers.iter() {
        if STRIP_FROM_REQUEST.contains(&name.as_str()) {
            continue;
        }
        forwarded.insert(name.clone(), value.clone());
    }
    if let Some(credential) = &app.upstream_auth_value {
        let name = HeaderName::from_bytes(app.upstream_auth_header.as_bytes()).map_err(|_| {
            format!(
                "UPSTREAM_AUTH_HEADER {:?} is not a header name",
                app.upstream_auth_header
            )
        })?;
        let value = credential
            .parse()
            .map_err(|_| "UPSTREAM_AUTH_VALUE is not a valid header value".to_string())?;
        forwarded.insert(name, value);
    }

    let mut request = app.http.request(method.clone(), target).headers(forwarded);
    if !body.is_empty() {
        request = request.body(body);
    }

    let response = request.send().await.map_err(|e| e.to_string())?;
    let status = StatusCode::from_u16(response.status().as_u16())
        .map_err(|e| format!("upstream returned an unusable status: {e}"))?;
    let headers = response.headers().clone();
    let body = response.bytes().await.map_err(|e| e.to_string())?;
    Ok(UpstreamResponse {
        status,
        headers,
        body,
    })
}
