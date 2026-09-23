//! The x402 v2 wire protocol, over HTTP.
//!
//! TypeScript and Go get this from `@x402/core` and `github.com/coinbase/x402/go`.
//! There is no Rust SDK, so the protocol is implemented here against the spec:
//! `specs/transports-v2/http.md` and `specs/x402-specification-v2.md` in
//! <https://github.com/coinbase/x402>.
//!
//! Three base64-encoded JSON headers carry everything:
//!
//! | Header              | Direction       | Payload              |
//! |---------------------|-----------------|----------------------|
//! | `PAYMENT-REQUIRED`  | server -> client | [`PaymentRequired`]  |
//! | `PAYMENT-SIGNATURE` | client -> server | client payment payload |
//! | `PAYMENT-RESPONSE`  | server -> client | [`SettleResponse`]   |

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use serde::{Deserialize, Serialize};

/// The only protocol version this template implements.
pub const X402_VERSION: i64 = 2;

pub const HEADER_PAYMENT_REQUIRED: &str = "PAYMENT-REQUIRED";
pub const HEADER_PAYMENT_SIGNATURE: &str = "PAYMENT-SIGNATURE";
pub const HEADER_PAYMENT_RESPONSE: &str = "PAYMENT-RESPONSE";

/// What the server demands for one resource.
///
/// This struct is only ever built by us, from our own configuration, and is the
/// copy sent to the facilitator. The `accepted` block a client echoes back in
/// its payload is never used to decide what it owes -- a client that could pick
/// its own `amount` could pay one unit for everything.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaymentRequirements {
    pub scheme: String,
    pub network: String,
    pub asset: String,
    /// Integer token units, as a decimal string.
    pub amount: String,
    #[serde(rename = "payTo")]
    pub pay_to: String,
    #[serde(rename = "maxTimeoutSeconds")]
    pub max_timeout_seconds: u32,
    /// EIP-712 domain the payer must sign over (`name`, `version`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<serde_json::Value>,
}

/// The resource a payment buys, echoed back to the client for context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceInfo {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

/// Body of the `PAYMENT-REQUIRED` header on a 402.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaymentRequired {
    #[serde(rename = "x402Version")]
    pub x402_version: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource: Option<ResourceInfo>,
    pub accepts: Vec<PaymentRequirements>,
}

impl PaymentRequired {
    pub fn new(
        error: impl Into<String>,
        resource: ResourceInfo,
        accepts: PaymentRequirements,
    ) -> Self {
        Self {
            x402_version: X402_VERSION,
            error: Some(error.into()),
            resource: Some(resource),
            accepts: vec![accepts],
        }
    }
}

/// Facilitator `/verify` result.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct VerifyResponse {
    #[serde(rename = "isValid")]
    pub is_valid: bool,
    #[serde(rename = "invalidReason")]
    pub invalid_reason: Option<String>,
    #[serde(rename = "invalidMessage")]
    pub invalid_message: Option<String>,
    pub payer: Option<String>,
}

/// Facilitator `/settle` result, and the body of the `PAYMENT-RESPONSE` header.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SettleResponse {
    pub success: bool,
    #[serde(rename = "errorReason", skip_serializing_if = "Option::is_none")]
    pub error_reason: Option<String>,
    #[serde(rename = "errorMessage", skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payer: Option<String>,
    pub transaction: String,
    pub network: String,
}

impl VerifyResponse {
    /// Why the facilitator rejected the payment, in one line for the client.
    pub fn rejection(&self) -> String {
        match (&self.invalid_reason, &self.invalid_message) {
            (Some(r), Some(m)) => format!("{r}: {m}"),
            (Some(r), None) => r.clone(),
            (None, Some(m)) => m.clone(),
            (None, None) => "payment rejected by facilitator".to_string(),
        }
    }
}

impl SettleResponse {
    pub fn failure(&self) -> String {
        match (&self.error_reason, &self.error_message) {
            (Some(r), Some(m)) => format!("{r}: {m}"),
            (Some(r), None) => r.clone(),
            (None, Some(m)) => m.clone(),
            (None, None) => "settlement failed".to_string(),
        }
    }
}

/// Serializes `value` the way the three x402 headers are carried: JSON, base64.
pub fn encode_header<T: Serialize>(value: &T) -> Result<String, serde_json::Error> {
    Ok(B64.encode(serde_json::to_vec(value)?))
}

/// Anything wrong with a client's `PAYMENT-SIGNATURE`.
#[derive(Debug)]
pub enum PayloadError {
    NotBase64,
    NotJson(serde_json::Error),
    /// Present but not the version this server speaks.
    UnsupportedVersion(i64),
}

impl std::fmt::Display for PayloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotBase64 => write!(f, "{HEADER_PAYMENT_SIGNATURE} is not valid base64"),
            Self::NotJson(e) => write!(f, "{HEADER_PAYMENT_SIGNATURE} is not valid JSON: {e}"),
            Self::UnsupportedVersion(v) => {
                write!(
                    f,
                    "unsupported x402Version {v}, this service speaks {X402_VERSION}"
                )
            }
        }
    }
}

/// Decodes a client's `PAYMENT-SIGNATURE` header.
///
/// The payload is kept as an opaque `Value` and forwarded to the facilitator
/// byte-for-byte. Re-serializing it through typed structs would drop scheme
/// fields and extensions this template does not model, and the signature covers
/// what the client actually sent.
pub fn decode_payment_signature(raw: &str) -> Result<serde_json::Value, PayloadError> {
    let bytes = B64
        .decode(raw.trim())
        .map_err(|_| PayloadError::NotBase64)?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(PayloadError::NotJson)?;
    match value.get("x402Version").and_then(serde_json::Value::as_i64) {
        Some(X402_VERSION) => Ok(value),
        Some(other) => Err(PayloadError::UnsupportedVersion(other)),
        None => Err(PayloadError::UnsupportedVersion(0)),
    }
}

/// Talks to the facilitator that verifies signatures and broadcasts settlement.
pub struct Facilitator {
    base_url: String,
    http: reqwest::Client,
}

#[derive(Debug)]
pub struct FacilitatorError(pub String);

impl std::fmt::Display for FacilitatorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Facilitator {
    pub fn new(base_url: impl Into<String>, http: reqwest::Client) -> Self {
        // Trailing slashes would produce //verify, which some gateways 404 on.
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            http,
        }
    }

    /// Does this payment authorize what we are about to charge?
    pub async fn verify(
        &self,
        payload: &serde_json::Value,
        requirements: &PaymentRequirements,
    ) -> Result<VerifyResponse, FacilitatorError> {
        self.call("verify", payload, requirements).await
    }

    /// Broadcast the transfer. Only ever called after the upstream succeeded.
    pub async fn settle(
        &self,
        payload: &serde_json::Value,
        requirements: &PaymentRequirements,
    ) -> Result<SettleResponse, FacilitatorError> {
        self.call("settle", payload, requirements).await
    }

    async fn call<T: for<'de> Deserialize<'de>>(
        &self,
        endpoint: &str,
        payload: &serde_json::Value,
        requirements: &PaymentRequirements,
    ) -> Result<T, FacilitatorError> {
        let url = format!("{}/{endpoint}", self.base_url);
        let body = serde_json::json!({
            "x402Version": X402_VERSION,
            "paymentPayload": payload,
            "paymentRequirements": requirements,
        });

        let response =
            self.http.post(&url).json(&body).send().await.map_err(|e| {
                FacilitatorError(format!("facilitator {endpoint} unreachable: {e}"))
            })?;

        let status = response.status();
        let text = response.text().await.map_err(|e| {
            FacilitatorError(format!("facilitator {endpoint} body unreadable: {e}"))
        })?;

        // A non-200 still carries a structured reason often enough to be worth
        // parsing: "insufficient_funds" beats "facilitator settle failed (400)".
        if !status.is_success() {
            let reason = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| {
                    v.get("invalidReason")
                        .or_else(|| v.get("errorReason"))
                        .and_then(|r| r.as_str().map(str::to_string))
                })
                .unwrap_or_else(|| excerpt(&text));
            return Err(FacilitatorError(format!(
                "facilitator {endpoint} failed ({}): {reason}",
                status.as_u16()
            )));
        }

        serde_json::from_str(&text).map_err(|e| {
            FacilitatorError(format!(
                "facilitator {endpoint} returned unreadable JSON ({e}): {}",
                excerpt(&text)
            ))
        })
    }
}

fn excerpt(body: &str) -> String {
    let compact = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.is_empty() {
        return "<empty response>".to_string();
    }
    if compact.chars().count() <= 200 {
        return compact;
    }
    compact.chars().take(197).collect::<String>() + "..."
}

#[cfg(test)]
mod tests {
    use super::*;

    fn requirements() -> PaymentRequirements {
        PaymentRequirements {
            scheme: "exact".into(),
            network: "eip155:2366".into(),
            asset: "0x7aB6f3ed87C42eF0aDb67Ed95090f8bF5240149e".into(),
            amount: "1000".into(),
            pay_to: "0x209693Bc6afc0C5328bA36FaF03C514EF312287C".into(),
            max_timeout_seconds: 60,
            extra: Some(serde_json::json!({"name": "Bridged USDC (Kite AI)", "version": "2"})),
        }
    }

    #[test]
    fn payment_required_header_matches_the_spec_shape() {
        let challenge = PaymentRequired::new(
            "PAYMENT-SIGNATURE header is required",
            ResourceInfo {
                url: "https://svc.example.com/v1/forecast".into(),
                description: Some("Paid API".into()),
                mime_type: Some("application/json".into()),
            },
            requirements(),
        );
        let decoded: serde_json::Value =
            serde_json::from_slice(&B64.decode(encode_header(&challenge).unwrap()).unwrap())
                .unwrap();

        assert_eq!(decoded["x402Version"], 2);
        assert_eq!(decoded["accepts"][0]["scheme"], "exact");
        assert_eq!(decoded["accepts"][0]["network"], "eip155:2366");
        assert_eq!(decoded["accepts"][0]["amount"], "1000");
        // camelCase on the wire, snake_case in Rust: a rename regression here
        // makes every client reject the challenge.
        assert_eq!(decoded["accepts"][0]["payTo"], requirements().pay_to);
        assert_eq!(decoded["accepts"][0]["maxTimeoutSeconds"], 60);
        assert_eq!(decoded["accepts"][0]["extra"]["version"], "2");
    }

    #[test]
    fn payment_signature_round_trips_and_is_version_checked() {
        let payload = serde_json::json!({
            "x402Version": 2,
            "accepted": {"scheme": "exact"},
            "payload": {"signature": "0xdeadbeef"},
        });
        let encoded = B64.encode(serde_json::to_vec(&payload).unwrap());
        assert_eq!(decode_payment_signature(&encoded).unwrap(), payload);

        // v1 payloads are a different wire shape; failing loudly beats
        // forwarding them to the facilitator and guessing at the error.
        let v1 = B64.encode(br#"{"x402Version":1}"#);
        assert!(matches!(
            decode_payment_signature(&v1),
            Err(PayloadError::UnsupportedVersion(1))
        ));
        assert!(matches!(
            decode_payment_signature("not base64!!"),
            Err(PayloadError::NotBase64)
        ));
        assert!(matches!(
            decode_payment_signature(&B64.encode(b"{")),
            Err(PayloadError::NotJson(_))
        ));
        assert!(matches!(
            decode_payment_signature(&B64.encode(b"{}")),
            Err(PayloadError::UnsupportedVersion(0))
        ));
    }

    #[test]
    fn settle_response_serializes_for_the_payment_response_header() {
        let settled = SettleResponse {
            success: true,
            transaction: "0xabc".into(),
            network: "eip155:2366".into(),
            payer: Some("0x857b".into()),
            ..Default::default()
        };
        let decoded: serde_json::Value =
            serde_json::from_slice(&B64.decode(encode_header(&settled).unwrap()).unwrap()).unwrap();
        assert_eq!(decoded["success"], true);
        assert_eq!(decoded["transaction"], "0xabc");
        assert!(decoded.get("errorReason").is_none());
    }
}
