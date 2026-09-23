# kite-x402-rs

[![CI](https://github.com/huaruic/kite-x402-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/huaruic/kite-x402-rs/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.88%2B-orange.svg)](https://www.rust-lang.org)

Turn any HTTP API into a service that AI agents can call and pay for per
request, using the [x402](https://www.x402.org) protocol with settlement on the
[Kite](https://gokite.ai) chain.

You bring the API. This gives you a wrapper that answers `402 Payment Required`,
verifies and settles the payment through the Kite facilitator, and proxies paid
requests to your upstream. You deploy it; Kite Passport agents can then discover
and pay for it.

```
agent (Kite Passport)                 this wrapper                     your upstream API
────────────────────                 ─────────────                    ─────────────────
GET /v1/forecast ───────────────────► 402 + PAYMENT-REQUIRED
                                       (network, asset, amount, payTo)
sign EIP-3009 authorization
GET /v1/forecast
  PAYMENT-SIGNATURE: … ─────────────► facilitator /verify ✓
                                       GET /forecast ─────────────────► 200 JSON
                                       facilitator /settle ✓ (on-chain)
◄──────────────────────────────────── 200 JSON + PAYMENT-RESPONSE (tx hash)
```

No smart contracts, no wallet code, no gas, **no private key**. The buyer signs
a token authorization; the facilitator broadcasts it and pays the gas; USDC.e
lands in your wallet. The wrapper is a reverse proxy with a payment gate.

## Why this exists

The official [`gokite-ai/kite-x402-services`](https://github.com/gokite-ai/kite-x402-services)
repository ships wrapper templates for TypeScript/Express and Go/Gin, both built
on an official x402 SDK. **There is no x402 SDK for Rust** — [`coinbase/x402`](https://github.com/coinbase/x402)
publishes `typescript`, `go`, `python` and `java` only.

So `src/x402.rs` implements the v2 wire protocol directly against the
specification. That is the substance of this project; the rest is a proxy.

## Quick start

Requires Rust 1.88 or newer.

```bash
git clone https://github.com/huaruic/kite-x402-rs
cd kite-x402-rs
cp .env.example .env          # fill in PAY_TO, UPSTREAM_URL, PRICE_USD
set -a && source .env && set +a
cargo run
```

In another shell:

```bash
curl -i "localhost:8080/v1/forecast?latitude=52.52&longitude=13.41&current=temperature_2m"
```

You get `HTTP/1.1 402 Payment Required` and a base64 `PAYMENT-REQUIRED` header.
Decode it:

```bash
curl -sD - -o /dev/null "localhost:8080/v1/forecast" \
  | grep -i '^payment-required:' | sed 's/^[^:]*: *//' | tr -d '\r' \
  | base64 -d | python3 -m json.tool
```

```json
{
  "x402Version": 2,
  "error": "PAYMENT-SIGNATURE header is required",
  "resource": {
    "url": "https://localhost:8080/v1/forecast",
    "description": "Open-Meteo forecast behind x402 on Kite",
    "mimeType": "application/json"
  },
  "accepts": [
    {
      "scheme": "exact",
      "network": "eip155:2366",
      "asset": "0x7aB6f3ed87C42eF0aDb67Ed95090f8bF5240149e",
      "amount": "1000",
      "payTo": "0xYourKiteWallet",
      "maxTimeoutSeconds": 60,
      "extra": { "name": "Bridged USDC (Kite AI)", "version": "2" }
    }
  ]
}
```

That is the whole contract: every path under `/v1/` is paid and proxied to
`UPSTREAM_URL` with the `/v1` prefix stripped. `/healthz` is free.

## Configuration

All configuration is environment variables — the same set the official
TypeScript and Go templates use, so a service can move between them unchanged.

| Variable | Required | Default | Description |
|---|:---:|---|---|
| `PAY_TO` | **yes** | — | Kite wallet that receives payments. `kpass wallet address`, or any EOA you control. |
| `UPSTREAM_URL` | **yes** | — | The API being wrapped. `/v1/<path>` proxies to `UPSTREAM_URL/<path>`. |
| `KITE_NETWORK` | no | `mainnet` | `mainnet` (USDC.e) or `testnet` (pieUSD). |
| `PRICE_USD` | no | `0.001` | Price per call in USD. At most 6 decimals on mainnet. |
| `UPSTREAM_AUTH_HEADER` | no | `Authorization` | Header used to inject the upstream credential. |
| `UPSTREAM_AUTH_VALUE` | no | — | The credential itself. Never reaches the caller. |
| `SERVICE_DESCRIPTION` | no | `Paid API wrapped for the Kite network` | Shown to buyers in the 402 challenge. |
| `PORT` | no | `8080` | Listen port. Bound on `0.0.0.0`. |
| `PUBLIC_BASE_URL` | no | `https://<Host header>` | Pins the advertised resource URL when a proxy rewrites `Host`. |
| `FACILITATOR_URL` | no | `https://facilitator.pieverse.io/v2` | Keep the `/v2`; the SDKs append `/verify` and `/settle`. |

The service **exits at startup** if `PAY_TO` or `UPSTREAM_URL` is missing, or if
`PRICE_USD` cannot be represented by the configured asset. A service that
answers `/healthz` but fails every purchase is the worst failure mode here.

### Kite network reference

| | Mainnet | Testnet |
|---|---|---|
| CAIP-2 network | `eip155:2366` | `eip155:2368` |
| RPC | `https://rpc.gokite.ai` | `https://rpc-testnet.gokite.ai` |
| Settlement asset | USDC.e `0x7aB6…149e` (6 decimals) | pieUSD `0x3812…621A` (18 decimals) |
| EIP-712 domain | `Bridged USDC (Kite AI)` v2 | `pieUSD` v1 |

These are encoded in [`src/kite.rs`](src/kite.rs); you should not need to edit
that file.

## Behaviour

| Situation | Response | Buyer charged? |
|---|---|:---:|
| No `PAYMENT-SIGNATURE` | 402 + `PAYMENT-REQUIRED` | no |
| Malformed or non-v2 payload | 400 | no |
| Facilitator rejects the payment | 402 + `PAYMENT-REQUIRED` | no |
| Facilitator unreachable | 502 | no |
| Upstream unreachable | 502 | no |
| Upstream answered ≥ 400 | upstream's own status and body | no |
| Upstream answered < 400, settle failed | 402 + `PAYMENT-RESPONSE` | no |
| Upstream answered < 400, settle succeeded | upstream's body + `PAYMENT-RESPONSE` | **yes** |

The ordering — **verify → upstream → settle** — is the rule this project exists
to get right. Charging before the upstream has answered is the most common
complaint agents have about paid APIs, and it is enforced by tests rather than
by convention (see [Testing](#testing)).

## How it works

Three base64-encoded JSON headers carry the whole protocol:

| Header | Direction | Payload |
|---|---|---|
| `PAYMENT-REQUIRED` | server → client | what to sign, on a 402 |
| `PAYMENT-SIGNATURE` | client → server | the signed EIP-3009 authorization |
| `PAYMENT-RESPONSE` | server → client | settlement result and transaction hash |

The facilitator is called twice, both times with
`{ "x402Version": 2, "paymentPayload": …, "paymentRequirements": … }`:

- `POST /verify` — does this signature authorise what we are about to charge?
- `POST /settle` — broadcast it. Only ever called after the upstream succeeded.

Reference: [`specs/transports-v2/http.md`](https://github.com/coinbase/x402/blob/main/specs/transports-v2/http.md),
[`specs/x402-specification-v2.md`](https://github.com/coinbase/x402/blob/main/specs/x402-specification-v2.md).

### Security model

**The client cannot name its own price.** A payment payload echoes back an
`accepted` block describing what the client thinks it owes. That block is never
used to decide the charge. The `paymentRequirements` sent to the facilitator are
always the ones this service built at startup from its own configuration; the
client's payload is forwarded verbatim only because the signature covers exactly
what the client sent.

**The upstream credential never reaches the caller.** It is injected by the
proxy after the inbound headers are filtered, and `PAYMENT-SIGNATURE` is
stripped before the request goes upstream.

**No key material lives here.** The wrapper never signs anything and never
touches the chain, so there is no private key to leak.

See [SECURITY.md](SECURITY.md) for reporting a vulnerability.

## Testing

```bash
cargo test
```

14 tests, no external network access required:

- **Price parsing** — both assets' decimals, and the rounding, precision and
  overflow cases that would otherwise settle the wrong amount. `$0.001` is
  `1000` units of USDC.e but `1000000000000000` units of pieUSD, and neither
  fits a float.
- **Header codec** — the camelCase wire names, and the version check that
  rejects a v1 payload instead of forwarding it to the facilitator.
- **End-to-end ordering** — an in-process fake facilitator and fake upstream
  count the calls they receive, so each test asserts an exact
  `(verify, upstream, settle)` triple rather than just a status code.

The ordering tests have teeth: deleting the upstream-status check makes
`a_failing_upstream_is_relayed_without_settling` fail, because a 500 from the
upstream must produce `(1, 1, 0)` — verified, forwarded, **not charged**.

CI additionally builds the container image, starts it, and asserts that
`/healthz` answers and that an unpaid `/v1` request returns a 402 whose decoded
challenge carries the right network and amount.

## Deployment

The service must be reachable over public **HTTPS**: Kite Passport calls it from
its own servers, so localhost, plain HTTP and browser-gated tunnels do not work.

### Container

```bash
docker build -t kite-x402-axum .
docker run -p 8080:8080 \
  -e PAY_TO=0xYourKiteWallet \
  -e UPSTREAM_URL=https://api.open-meteo.com \
  -e PRICE_USD=0.001 \
  kite-x402-axum
```

### Fly.io

```bash
fly launch --internal-port 8080 --no-deploy
fly secrets set PAY_TO=0x… UPSTREAM_URL=https://api.example.com PRICE_USD=0.001
fly deploy
```

### Render

New → Web Service → Docker, then set the same variables in the dashboard. Leave
`PORT` alone; Render injects it and the service honours it.

### Binary

```bash
cargo build --release      # ./target/release/kite-x402-axum
```

> Set `UPSTREAM_AUTH_VALUE` as a runtime secret, never as a Docker build
> argument — a build argument is baked into an image layer.

## Verifying with a Kite Passport agent

Install the `kpass` CLI (see the [Kite Passport docs](https://docs.gokite.ai)),
and test on testnet first, where a sandbox agent pays with free pieUSD:

```bash
kpass login init --email you@example.com
kpass login verify --login-id <id> --code <otp>

kpass sandbox on
kpass wallet address
kpass faucet drop --recipient <that-address> --token pieUSD

kpass agent register --type service-tester
kpass agent session create \
  --task-summary "test my x402 wrapper" \
  --max-amount-per-tx 0.05 --max-total-amount 1 --assets pieUSD --ttl 1h

kpass agent session execute --method GET --url "https://your-host/v1/forecast?…"
```

A successful run prints HTTP 200, the upstream body, and the settlement
transaction hash.

> **Note:** Kite Passport only pays hosts admitted to its executable catalog.
> An otherwise-correct service returns `host_not_in_executable_catalog` until
> the host is allowlisted, which is a request to the Kite team rather than a
> configuration fix.

## Project layout

```
src/main.rs    configuration, routes, the payment gate and the proxy. Edit this.
src/kite.rs    Kite network constants and the $0.001-style price parser.
src/x402.rs    the x402 v2 protocol: headers, types, facilitator client.
Dockerfile     multi-stage build; CI builds and exercises it on every push.
```

`kite.rs` and `x402.rs` are the parts you inherit; `main.rs` is the part you
adapt to your upstream.

## Relationship to the official repository

This is an independent implementation intended to become the `rust-axum`
template in [`gokite-ai/kite-x402-services`](https://github.com/gokite-ai/kite-x402-services).
It follows that repository's contract deliberately: the same environment
variables, the same `/v1/*` paid prefix, the same settle-on-success rule, and a
402 challenge whose fields match the documented shape.

## Roadmap

- [ ] Extract `x402.rs` and `kite.rs` into a `kite-x402` library crate so other
      Rust frameworks (actix-web, warp, tower) can depend on the protocol
      without the axum binary, and publish it to crates.io.
- [ ] A verified end-to-end payment against the live facilitator on testnet.
- [ ] Upstream the template, including a `template-rust` CI job.

## Contributing

Issues and pull requests are welcome — see [CONTRIBUTING.md](CONTRIBUTING.md)
for the conventions and the checks CI runs.

## License

[Apache-2.0](LICENSE), matching the upstream Kite repository.
