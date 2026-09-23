# Rust + Axum template

A ready-to-deploy reverse proxy that charges x402 payments on the Kite chain
for every request under `/v1/*` and forwards paid requests to `UPSTREAM_URL`.

```bash
cp .env.example .env        # fill PAY_TO, UPSTREAM_URL, PRICE_USD
set -a && source .env && set +a
cargo run
curl -i "localhost:8080/v1/forecast?latitude=52.52&longitude=13.41&current=temperature_2m"
# HTTP/1.1 402 Payment Required + PAYMENT-REQUIRED header
```

Files:

- `src/main.rs` — configuration, routes, proxy. Edit this.
- `src/kite.rs` — Kite network constants and the `$0.001`-style price parser. Leave as is.
- `src/x402.rs` — the x402 v2 protocol itself. Leave as is.

The wrapper verifies the signature before your upstream is called and settles
only when the upstream responded with a status below 400, so a failed upstream
call never charges the buyer.

## Why this template carries `x402.rs`

The TypeScript and Go templates get the protocol from an SDK (`@x402/core`,
`github.com/coinbase/x402/go`). There is no Rust x402 SDK, so `src/x402.rs`
implements the wire protocol directly against the spec — the three base64 JSON
headers and the facilitator's `/verify` and `/settle` calls:

| Header | Direction | Payload |
|---|---|---|
| `PAYMENT-REQUIRED` | server → client | what to sign, on a 402 |
| `PAYMENT-SIGNATURE` | client → server | the signed authorization |
| `PAYMENT-RESPONSE` | server → client | settlement result and tx hash |

No signing or chain access happens here: the payer signs an EIP-3009
authorization, and the facilitator verifies it, broadcasts it and pays the gas.
That is why the template needs no crypto dependency and no private key.

Reference: [`specs/transports-v2/http.md`](https://github.com/coinbase/x402/blob/main/specs/transports-v2/http.md)
and [`specs/x402-specification-v2.md`](https://github.com/coinbase/x402/blob/main/specs/x402-specification-v2.md).

## Request handling

```
GET /healthz              free, returns { ok, network, asset, price }
ANY /v1/<path>            paid, proxied to UPSTREAM_URL/<path> with the query string
```

| Situation | Response | Charged? |
|---|---|---|
| No `PAYMENT-SIGNATURE` | 402 + `PAYMENT-REQUIRED` | no |
| Malformed or non-v2 payload | 400 | no |
| Facilitator rejects the payment | 402 + `PAYMENT-REQUIRED` | no |
| Facilitator unreachable | 502 | no |
| Upstream unreachable | 502 | no |
| Upstream answered ≥ 400 | upstream's own status and body | no |
| Upstream answered < 400, settle failed | 402 + `PAYMENT-RESPONSE` | no |
| Upstream answered < 400, settle succeeded | upstream's body + `PAYMENT-RESPONSE` | yes |

The client's payload is forwarded to the facilitator verbatim, but the payment
requirements sent alongside it are always the ones this service built at
startup from its own configuration. A client cannot name its own price by
editing the `accepted` block it echoes back.

## Configuration

Same environment variables as the TypeScript and Go templates; see
[`.env.example`](.env.example). `PAY_TO` and `UPSTREAM_URL` are required and the
service exits at startup without them, as it does on a price the configured
asset cannot represent — a service that answers `/healthz` but fails every
purchase is the worst failure mode here.

`PUBLIC_BASE_URL` is the one addition: it pins the resource URL advertised in
the 402 when a proxy in front of the service rewrites `Host`.

## Tests

```bash
cargo test
```

Covers the price parser (both assets' decimals, and the rounding and overflow
cases that would settle the wrong amount) and the header codec (the camelCase
wire names, and the version check on inbound payloads).

## Deploy

Any host that serves public **https** — Fly, Render, Cloud Run, a VPS. Kite
Passport calls the service from its servers, so localhost, http and
browser-gated tunnels do not work.

```bash
cargo build --release   # ./target/release/kite-x402-rust-axum
```

See the [upstream repository README](https://github.com/gokite-ai/kite-x402-services)
for the full flow and how to test with a Kite Passport agent.

## License

Apache-2.0, matching the upstream repository. See [LICENSE](LICENSE).
