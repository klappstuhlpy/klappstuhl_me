# ─────────────────────────────────────────────────────────────────────────────
# Stage 1 – builder
#
# Uses the official Rust image.  git is required because two Cargo dependencies
# are pulled directly from GitHub repositories.
# ─────────────────────────────────────────────────────────────────────────────
FROM rust:1.88-slim-bookworm AS builder

# mold is a fast drop-in linker; .cargo/config.toml points the linux target at
# it via rustflags, which cuts a big chunk off the final link step.
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    git \
    mold \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# ── Build with persistent cargo + target cache ────────────────────────────────
# BuildKit cache mounts persist the cargo registry/git checkouts and the
# `target/` dir *across* builds. This restores Cargo's incremental compilation:
# a `git pull` that touches a few files recompiles only those, instead of the
# whole crate from scratch every deploy. Because `target/` is a cache mount
# (not part of the image layer), the finished binary must be copied out before
# the RUN ends, otherwise it won't exist in the next stage.
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/build/target \
    cargo build --release \
 && cp target/release/klappstuhl_me /build/klappstuhl_me


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

# Binary (SQLite is bundled in, no separate .so needed). Copied out of the
# build stage's cache-mounted target/ to /build/klappstuhl_me in the builder.
COPY --from=builder /build/klappstuhl_me ./klappstuhl_me

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
