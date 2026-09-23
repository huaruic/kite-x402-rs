## What this changes

## Why

<!-- If the diff does not make the reason obvious, put it here. -->

## Checks

- [ ] `cargo fmt --check`
- [ ] `cargo clippy --all-targets -- -D warnings`
- [ ] `cargo test`
- [ ] New or changed non-trivial logic leaves a runnable test behind

## Parity with the official templates

- [ ] Environment variables unchanged, or the change is justified above
- [ ] `/healthz` free and `/v1/*` paid, prefix stripped on proxy
- [ ] Settlement still happens only after the upstream answers below 400

<!-- Tick "unchanged" boxes freely; they exist so a reviewer can see at a glance
     that the contract with gokite-ai/kite-x402-services still holds. -->
