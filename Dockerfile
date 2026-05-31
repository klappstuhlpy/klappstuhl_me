# ─────────────────────────────────────────────────────────────────────────────
# Stage 1 – builder
#
# Uses the official Rust image.  git is required because two Cargo dependencies
# are pulled directly from GitHub repositories.
# ─────────────────────────────────────────────────────────────────────────────
FROM rust:1.88-slim-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    git \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# ── Dependency layer cache ────────────────────────────────────────────────────
# Copy only the manifests first and compile a stub binary so that all
# dependency crates are compiled and cached in a separate layer.  The real
# source is copied afterwards; only our own crate needs to be recompiled on
# source changes, not the entire dependency tree.
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src && echo 'fn main() {}' > src/main.rs \
 && cargo build --release \
 && rm -rf src target/release/deps/klappstuhl_me* target/release/klappstuhl_me*

# ── Real build ────────────────────────────────────────────────────────────────
COPY . .
RUN cargo build --release


# ─────────────────────────────────────────────────────────────────────────────
# Stage 2 – runtime
#
# Minimal Debian image.  Only what is strictly needed at runtime:
#   • ca-certificates  – validates the ACME certificate chain (Let's Encrypt)
#   • docker-ce-cli    – the /services admin page shells out to `docker`
#   • chromium         – the render screenshot / Markdown→PDF endpoints
#   • ffmpeg           – the video/HEIC transcode endpoint
#
# chromium pulls in its own shared-library and font dependencies (libnss3,
# fonts, …) via apt, so the headless browser actually starts inside the
# container.  fonts-liberation is added explicitly so rendered pages have a
# sane default font.  The app resolves both binaries off PATH (no config
# needed) and already launches Chromium with --no-sandbox, which is required
# when running as root inside a container.
# ─────────────────────────────────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    docker.io \
    curl \
    nginx \
    caddy \
    ufw \
    chromium \
    ffmpeg \
    fonts-liberation \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Binary (SQLite is bundled in, no separate .so needed)
COPY --from=builder /build/target/release/klappstuhl_me ./klappstuhl_me

# Static assets served at runtime by tower_http from ./static/
COPY static/ ./static/

# ── Persistent data layout ────────────────────────────────────────────────────
# All mutable data is rooted under /data via XDG environment variables:
#
#   /data/config/klappstuhl_me/config.json          ← application config
#   /data/data/klappstuhl_me/main.db                ← SQLite database
#   /data/state/klappstuhl_me/                      ← rolling log files
#   /data/cache/klappstuhl_me/rustls_acme_cache/    ← ACME / TLS cert cache
#
ENV XDG_CONFIG_HOME=/data/config \
    XDG_DATA_HOME=/data/data \
    XDG_STATE_HOME=/data/state \
    XDG_CACHE_HOME=/data/cache

# /data is declared as a volume so Docker (or Compose) can mount it.
VOLUME ["/data"]

# Default port (non-production).  Port 443 is also used in production mode.
EXPOSE 9510
EXPOSE 443

CMD ["./klappstuhl_me", "run"]
