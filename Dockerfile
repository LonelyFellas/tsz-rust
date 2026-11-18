# syntax=docker/dockerfile:1

FROM rust:1.98.0-bookworm AS build

WORKDIR /src

ENV SQLX_OFFLINE=true

# The application includes a git dependency and aws-lc-sys. Keep the toolchain
# and native build tools inside the builder image so the runtime stays small.
RUN apt-get update \
    && apt-get install --no-install-recommends -y \
        build-essential \
        clang \
        cmake \
        git \
        libssl-dev \
        pkg-config \
        perl \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock rust-toolchain.toml build.rs ./
COPY .sqlx ./.sqlx
COPY src ./src
COPY data ./data
COPY migrations ./migrations
COPY ops/speech-voice-catalog ./ops/speech-voice-catalog

RUN cargo build --locked --release

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install --no-install-recommends -y \
        ca-certificates \
        curl \
        libgcc-s1 \
        libstdc++6 \
        tini \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --create-home --home-dir /nonexistent --shell /usr/sbin/nologin tsz

COPY --from=build /src/target/release/tsz-rust /usr/local/bin/tsz-rust

USER tsz
WORKDIR /app
EXPOSE 8383

HEALTHCHECK --interval=10s --timeout=3s --start-period=20s --retries=6 \
    CMD curl --fail --silent http://127.0.0.1:8383/readyz >/dev/null || exit 1

ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["/usr/local/bin/tsz-rust"]
