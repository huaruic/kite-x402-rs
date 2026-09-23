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

    let router = router(app);

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

fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/{*path}", any(paid))
        .with_state(app)
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

#[cfg(test)]
mod tests {
    //! End-to-end tests for the one rule the whole template exists to enforce:
    //! verify -> upstream -> settle, and settle only when the upstream answered
    //! below 400. A fake facilitator and a fake upstream run in-process and
    //! count the calls they receive.

    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[derive(Default)]
    struct Calls {
        verify: AtomicUsize,
        settle: AtomicUsize,
        upstream: AtomicUsize,
        payment_valid: AtomicBool,
        settle_succeeds: AtomicBool,
    }

    impl Calls {
        fn counts(&self) -> (usize, usize, usize) {
            (
                self.verify.load(Ordering::SeqCst),
                self.upstream.load(Ordering::SeqCst),
                self.settle.load(Ordering::SeqCst),
            )
        }
    }

    async fn serve(router: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        format!("http://{addr}")
    }

    async fn fake_facilitator(calls: Arc<Calls>) -> String {
        let verify_calls = calls.clone();
        let settle_calls = calls.clone();
        serve(
            Router::new()
                .route(
                    "/verify",
                    axum::routing::post(move || {
                        let c = verify_calls.clone();
                        async move {
                            c.verify.fetch_add(1, Ordering::SeqCst);
                            axum::Json(serde_json::json!({
                                "isValid": c.payment_valid.load(Ordering::SeqCst),
                                "invalidReason": "insufficient_funds",
                                "payer": "0x857b",
                            }))
                        }
                    }),
                )
                .route(
                    "/settle",
                    axum::routing::post(move || {
                        let c = settle_calls.clone();
                        async move {
                            c.settle.fetch_add(1, Ordering::SeqCst);
                            axum::Json(serde_json::json!({
                                "success": c.settle_succeeds.load(Ordering::SeqCst),
                                "errorReason": "settle_rejected",
                                "transaction": "0xfeed",
                                "network": "eip155:2366",
                            }))
                        }
                    }),
                ),
        )
        .await
    }

    async fn fake_upstream(calls: Arc<Calls>) -> String {
        let ok_calls = calls.clone();
        let boom_calls = calls.clone();
        serve(
            Router::new()
                .route(
                    "/ok",
                    get(move || {
                        let c = ok_calls.clone();
                        async move {
                            c.upstream.fetch_add(1, Ordering::SeqCst);
                            "upstream body"
                        }
                    }),
                )
                .route(
                    "/boom",
                    get(move || {
                        let c = boom_calls.clone();
                        async move {
                            c.upstream.fetch_add(1, Ordering::SeqCst);
                            (StatusCode::INTERNAL_SERVER_ERROR, "upstream is down")
                        }
                    }),
                ),
        )
        .await
    }

    /// A wrapper wired to the two fakes, priced at $0.001 on Kite mainnet.
    async fn wrapper(calls: Arc<Calls>) -> String {
        let upstream = fake_upstream(calls.clone()).await;
        let facilitator = fake_facilitator(calls.clone()).await;
        let http = reqwest::Client::new();
        let chain = kite::KITE_MAINNET;
        let app = Arc::new(App {
            requirements: PaymentRequirements {
                scheme: "exact".into(),
                network: chain.network.into(),
                asset: chain.asset_address.into(),
                amount: kite::price_to_units("$0.001", &chain).unwrap().to_string(),
                pay_to: "0x209693Bc6afc0C5328bA36FaF03C514EF312287C".into(),
                max_timeout_seconds: 60,
                extra: Some(serde_json::json!({
                    "name": chain.eip712_name,
                    "version": chain.eip712_version,
                })),
            },
            chain,
            price_label: "$0.001".into(),
            description: "test".into(),
            upstream: reqwest::Url::parse(&upstream).unwrap(),
            upstream_auth_header: "Authorization".into(),
            upstream_auth_value: None,
            facilitator: Facilitator::new(facilitator, http.clone()),
            http,
        });
        serve(router(app)).await
    }

    fn signature() -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(
            serde_json::to_vec(&serde_json::json!({
                "x402Version": 2,
                "accepted": {"scheme": "exact", "network": "eip155:2366"},
                "payload": {"signature": "0xdeadbeef"},
            }))
            .unwrap(),
        )
    }

    #[tokio::test]
    async fn unpaid_request_is_challenged_and_never_reaches_the_upstream() {
        let calls = Arc::new(Calls::default());
        let base = wrapper(calls.clone()).await;

        let response = reqwest::get(format!("{base}/v1/ok")).await.unwrap();
        assert_eq!(response.status(), 402);
        assert!(response.headers().contains_key("payment-required"));
        // Nothing was verified, fetched or settled: the caller has not paid and
        // the upstream's rate limit was not spent on them.
        assert_eq!(calls.counts(), (0, 0, 0));
    }

    #[tokio::test]
    async fn rejected_payment_never_reaches_the_upstream() {
        let calls = Arc::new(Calls::default());
        calls.payment_valid.store(false, Ordering::SeqCst);
        let base = wrapper(calls.clone()).await;

        let response = reqwest::Client::new()
            .get(format!("{base}/v1/ok"))
            .header(HEADER_PAYMENT_SIGNATURE, signature())
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), 402);
        let challenge = response.headers().get("payment-required").unwrap();
        assert!(!challenge.is_empty());
        assert_eq!(calls.counts(), (1, 0, 0));
    }

    #[tokio::test]
    async fn a_failing_upstream_is_relayed_without_settling() {
        let calls = Arc::new(Calls::default());
        calls.payment_valid.store(true, Ordering::SeqCst);
        calls.settle_succeeds.store(true, Ordering::SeqCst);
        let base = wrapper(calls.clone()).await;

        let response = reqwest::Client::new()
            .get(format!("{base}/v1/boom"))
            .header(HEADER_PAYMENT_SIGNATURE, signature())
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), 500);
        assert!(response.headers().get("payment-response").is_none());
        assert_eq!(response.text().await.unwrap(), "upstream is down");
        // Verified and forwarded, but not charged. Reordering settle before the
        // upstream call is what this assertion exists to catch.
        assert_eq!(calls.counts(), (1, 1, 0));
    }

    #[tokio::test]
    async fn a_successful_upstream_settles_once_and_returns_the_receipt() {
        let calls = Arc::new(Calls::default());
        calls.payment_valid.store(true, Ordering::SeqCst);
        calls.settle_succeeds.store(true, Ordering::SeqCst);
        let base = wrapper(calls.clone()).await;

        let response = reqwest::Client::new()
            .get(format!("{base}/v1/ok"))
            .header(HEADER_PAYMENT_SIGNATURE, signature())
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), 200);
        let receipt = response
            .headers()
            .get("payment-response")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert_eq!(response.text().await.unwrap(), "upstream body");

        use base64::Engine as _;
        let settled: serde_json::Value = serde_json::from_slice(
            &base64::engine::general_purpose::STANDARD
                .decode(receipt)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(settled["success"], true);
        assert_eq!(settled["transaction"], "0xfeed");
        assert_eq!(calls.counts(), (1, 1, 1));
    }

    #[tokio::test]
    async fn a_failed_settlement_withholds_the_upstream_body() {
        let calls = Arc::new(Calls::default());
        calls.payment_valid.store(true, Ordering::SeqCst);
        calls.settle_succeeds.store(false, Ordering::SeqCst);
        let base = wrapper(calls.clone()).await;

        let response = reqwest::Client::new()
            .get(format!("{base}/v1/ok"))
            .header(HEADER_PAYMENT_SIGNATURE, signature())
            .send()
            .await
            .unwrap();

        // The money did not move, so the caller does not get what it did not
        // buy -- and the reason travels in PAYMENT-RESPONSE, per the spec.
        assert_eq!(response.status(), 402);
        assert!(response.headers().contains_key("payment-response"));
        assert!(!response.text().await.unwrap().contains("upstream body"));
        assert_eq!(calls.counts(), (1, 1, 1));
    }

    #[tokio::test]
    async fn a_malformed_payload_is_a_400_not_a_challenge() {
        let calls = Arc::new(Calls::default());
        let base = wrapper(calls.clone()).await;

        for bad in ["not base64!!", "e30="] {
            let response = reqwest::Client::new()
                .get(format!("{base}/v1/ok"))
                .header(HEADER_PAYMENT_SIGNATURE, bad)
                .send()
                .await
                .unwrap();
            // Retrying the same malformed header cannot succeed, so a 402
            // challenge would send the client into a loop.
            assert_eq!(response.status(), 400, "input: {bad}");
        }
        assert_eq!(calls.counts(), (0, 0, 0));
    }

    #[tokio::test]
    async fn healthz_is_free() {
        let calls = Arc::new(Calls::default());
        let base = wrapper(calls.clone()).await;
        let response = reqwest::get(format!("{base}/healthz")).await.unwrap();
        assert_eq!(response.status(), 200);
        let body: serde_json::Value = response.json().await.unwrap();
        assert_eq!(body["ok"], true);
        assert_eq!(body["network"], "eip155:2366");
        assert_eq!(body["asset"], "USDC.e");
        assert_eq!(calls.counts(), (0, 0, 0));
    }
}
