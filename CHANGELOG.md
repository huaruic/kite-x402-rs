# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Planned

- Extract `x402.rs` and `kite.rs` into a `kite-x402` library crate so other Rust
  frameworks can depend on the protocol without the axum binary, and publish to
  crates.io.
- A verified end-to-end payment against the live facilitator on Kite testnet.
- Upstream the template into `gokite-ai/kite-x402-services`, including a
  `template-rust` CI job.

## [0.1.0]

Initial release: a Rust + Axum wrapper that puts an x402 payment gate in front
of any HTTP API, settled on the Kite chain.

### Added

- x402 v2 protocol over HTTP, implemented against the specification because no
  Rust SDK exists: the `PAYMENT-REQUIRED`, `PAYMENT-SIGNATURE` and
  `PAYMENT-RESPONSE` base64 JSON headers, and the facilitator's `/verify` and
  `/settle` calls.
- Kite mainnet and testnet constants, and exact decimal-string conversion from a
  `$0.001`-style price to integer token units for both USDC.e (6 decimals) and
  pieUSD (18 decimals).
- Reverse proxy for `/v1/*` with the `/v1` prefix stripped, hop-by-hop headers
  filtered, `PAYMENT-SIGNATURE` withheld from the upstream, and the upstream
  credential injected server-side.
- Free `/healthz` reporting network, asset and configured price.
- Startup validation: the service exits rather than accepting a configuration
  that would fail every purchase.
- 14 tests, including end-to-end coverage of the `verify → upstream → settle`
  ordering via an in-process fake facilitator and upstream that assert exact
  call counts.
- Multi-stage `Dockerfile`, exercised in CI by starting the container and
  driving the 402 challenge.

### Security

- Payment requirements sent to the facilitator are always built from local
  configuration, never from the `accepted` block a client echoes back, so a
  client cannot choose its own price.

[Unreleased]: https://github.com/huaruic/kite-x402-rs/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/huaruic/kite-x402-rs/releases/tag/v0.1.0
