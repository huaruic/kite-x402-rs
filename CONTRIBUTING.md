# Contributing

Thanks for taking a look. Issues and pull requests are both welcome.

## Getting set up

Requires Rust 1.88 or newer (the floor is pinned in `Cargo.toml`).

```bash
git clone https://github.com/huaruic/kite-x402-rs
cd kite-x402-rs
cargo test
```

The tests need no network access and no wallet: the facilitator and the upstream
are faked in-process.

## The checks CI runs

Run these before opening a pull request; they are the whole gate.

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
docker build -t kite-x402-axum .        # optional locally, always run in CI
```

Warnings are errors. If clippy is wrong about your code, `#[allow(...)]` it with
a comment saying why, rather than loosening the lint globally.

## What to keep stable

This template deliberately mirrors the TypeScript and Go templates in
[`gokite-ai/kite-x402-services`](https://github.com/gokite-ai/kite-x402-services).
Three things are contract, not preference:

1. **The environment variables.** Same names, same defaults, same required set.
   A service should be able to move between templates without a config change.
2. **The route shape.** `/healthz` free, everything under `/v1/*` paid and
   proxied with the `/v1` prefix stripped.
3. **The settlement order.** `verify → upstream → settle`, and settle only when
   the upstream answered below 400.

A change to any of these needs a reason in the pull request body, because it
breaks parity with the official templates.

`src/kite.rs` holds chain constants the facilitator validates signatures
against. Changing an address or an EIP-712 domain there makes every signature
fail, so treat edits to it as protocol changes.

## Tests

Non-trivial logic should leave a runnable check behind. The money path and the
settlement ordering especially: those tests assert exact
`(verify, upstream, settle)` call counts, so a reordering fails the build
instead of silently charging someone for a request that errored.

A useful sanity check on a new test is to break the code it covers on purpose
and confirm the test fails.

## Commit messages

[Conventional Commits](https://www.conventionalcommits.org), matching the
upstream repository's convention:

```
feat(x402): support the upto scheme
fix(proxy): forward the query string on POST
test(kite): cover pieUSD 18-decimal rounding
chore(ci): pin the toolchain floor
docs(readme): document PUBLIC_BASE_URL
```

Explain *why* in the body when the change is not obvious from the diff. One
logical change per commit.

## Reporting bugs

Open an issue with the template. For anything with a security impact, use
[SECURITY.md](SECURITY.md) instead of a public issue.
