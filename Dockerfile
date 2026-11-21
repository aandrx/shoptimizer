# syntax=docker/dockerfile:1

# D4: builder pinned to amd64 so any host produces the scorer's linux/amd64 binary.
# rust:1.97-bookworm matches the local rustc; .cargo/config.toml carries D5's flags.
FROM --platform=linux/amd64 rust:1.97-bookworm AS builder
WORKDIR /build

# Dependency-cache layer: only the manifests and compiler config, plus a placeholder
# main, so every dependency is compiled and cached before the real sources arrive.
# A manifest or lockfile change is the only thing that invalidates this layer.
COPY Cargo.toml Cargo.lock ./
COPY .cargo ./.cargo
RUN mkdir -p src \
 && printf 'fn main() {}\n' > src/main.rs \
 && cargo build --release --locked

# Real sources. Drop the placeholder tree and touch every copied file so cargo cannot
# mistake the cached dummy artifacts for an up-to-date build, even though Docker COPY
# preserves source mtimes. This is what makes the source rebuild reliable.
RUN rm -rf src
COPY src ./src
RUN find src -type f -exec touch {} + \
 && cargo build --release --locked

# D4: distroless runtime (glibc + libgcc, no shell), entrypoint at / as specified.
FROM --platform=linux/amd64 gcr.io/distroless/cc-debian12
COPY --from=builder /build/target/release/shoptimizer /shoptimizer
ENTRYPOINT ["/shoptimizer"]
