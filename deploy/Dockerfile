# syntax=docker/dockerfile:1.7

# ---- builder ----
FROM rust:1.85-slim-bookworm AS builder
WORKDIR /src
RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config libssl-dev ca-certificates git \
    && rm -rf /var/lib/apt/lists/*
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --workspace \
    && mkdir -p /out \
    && cp target/release/ar-gateway /out/ \
    && cp target/release/auto_review /out/

# ---- linter stage (Milestone 1 minimal set) ----
FROM debian:bookworm-slim AS linters
RUN apt-get update && apt-get install -y --no-install-recommends \
        curl ca-certificates shellcheck nodejs npm python3-pip \
    && rm -rf /var/lib/apt/lists/* \
    && pip3 install --break-system-packages ruff \
    && npm install -g markdownlint-cli eslint \
    && curl -sSfL https://github.com/hadolint/hadolint/releases/latest/download/hadolint-Linux-x86_64 \
        -o /usr/local/bin/hadolint \
    && chmod +x /usr/local/bin/hadolint

# ---- runtime ----
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates git tini \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -r -u 10001 -m -d /home/auto auto
COPY --from=linters /usr/local/bin/hadolint /usr/local/bin/
COPY --from=linters /usr/bin/shellcheck /usr/bin/
COPY --from=linters /usr/local/bin/ruff /usr/local/bin/
COPY --from=linters /usr/bin/markdownlint /usr/bin/
COPY --from=linters /usr/bin/eslint /usr/bin/
COPY --from=builder /out/ar-gateway /usr/local/bin/
COPY --from=builder /out/auto_review /usr/local/bin/
USER auto
ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["/usr/local/bin/ar-gateway"]
