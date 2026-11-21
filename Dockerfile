FROM rust:1.97-bookworm AS builder
WORKDIR /app
# Dependency-cache layer: build the dependencies against a dummy main.
COPY Cargo.toml Cargo.lock ./
COPY .cargo .cargo
RUN mkdir src && echo 'fn main() {}' > src/main.rs \
    && cargo build --release --locked \
    && rm -rf src target/release/shoptimizer target/release/deps/shoptimizer* target/release/.fingerprint/shoptimizer*
COPY src src
RUN cargo build --release --locked

FROM gcr.io/distroless/cc-debian12
COPY --from=builder /app/target/release/shoptimizer /shoptimizer
ENTRYPOINT ["/shoptimizer"]
