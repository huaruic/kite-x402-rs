# Build and run the wrapper as a container, for hosts that want one (Fly, Cloud
# Run) or have no native Rust runtime (Render). The service reads PORT, so it
# works unchanged on platforms that inject their own.

FROM rust:1.98-bookworm AS build
# aws-lc-sys, the default rustls crypto provider, builds C through the cmake
# crate. Without these the TLS dependency fails to compile.
RUN apt-get update \
 && apt-get install -y --no-install-recommends cmake clang \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

FROM debian:bookworm-slim
# rustls verifies the facilitator's and the upstream's certificates against the
# system trust store, which the slim image does not ship.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --no-create-home kite
COPY --from=build /src/target/release/kite-x402-axum /usr/local/bin/
USER kite
ENV PORT=8080
EXPOSE 8080
CMD ["kite-x402-axum"]
