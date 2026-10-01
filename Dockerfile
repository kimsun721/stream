FROM rust:1.97-slim-bookworm AS builder

# aws-lc-sys, pulled in by rustls, builds from C. It links statically, so the
# runtime image needs nothing.
RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake perl clang \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Compile dependencies against a stub first so editing sources does not rebuild
# aws-lc-sys, which dominates a cold build.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src \
    && echo 'fn main() {}' > src/main.rs \
    && cargo build --release --locked \
    && rm -rf src

COPY static ./static
COPY src ./src
RUN touch src/main.rs && cargo build --release --locked

FROM debian:bookworm-slim AS runtime

RUN useradd --system --create-home --uid 10001 stream
USER stream
WORKDIR /app

COPY --from=builder /build/target/release/stream /usr/local/bin/stream
COPY config.toml.example ./

# Every key defaults, so the image starts with nothing mounted. Mount your own
# over /app/config.toml to override, plus certs if [server.tls] is set.
#
#   40000/udp  media, must be published as UDP
#   8443       SDP and WHIP, reached by clients
#   8080       room CRUD, reached only by a trusted backend
EXPOSE 40000/udp 8443 8080

CMD ["stream"]
