<div align="center">

<img src="/static/img/logo.png" height="100" alt="klappstuhl_me logo" style="margin-top: 1rem; border-radius: 0.25rem;">

# Klappstuhl.me

Personal website, image host, and pastebin — built in Rust.

[![Rust](https://img.shields.io/badge/Rust-1.74%2B-CE422B?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Axum](https://img.shields.io/badge/Axum-web-000000?logo=rust&logoColor=white)](https://github.com/tokio-rs/axum)
[![SQLite](https://img.shields.io/badge/SQLite-bundled-003B57?logo=sqlite&logoColor=white)](https://www.sqlite.org/)
[![API docs](https://img.shields.io/badge/API-Scalar%20reference-3178C6?logo=openapiinitiative&logoColor=white)](https://klappstuhl.me/api/docs)
[![License](https://img.shields.io/badge/License-AGPL--3.0-blue.svg)](LICENSE)

</div>

## Table of Contents

- [Documentation](#documentation)
- [Tech stack](#tech-stack)
- [Setup](docs/setup.md) — [quick start (Docker)](docs/setup.md#quick-start-docker), [configuration](docs/setup.md#configuration), [building from source](docs/setup.md#building-from-source)
- [Features](docs/features.md) — [insights](docs/features.md#insights), [account](docs/features.md#account), [Discord login & the Percy dashboard](docs/features.md#discord-login--the-percy-dashboard), [data & log paths](docs/features.md#data--log-paths)
- [License](#license)

## Documentation

The **public API** (images, links, pastes, media — effects, conversion,
metadata, color-palette extraction — render — code screenshots, QR codes, and
SVG charts — account/usage introspection, web/unfurl, scan, admin)
is documented by its live, interactive **Scalar** reference at
[`/api/docs`](https://klappstuhl.me/api/docs), generated from the OpenAPI schema in
`src/site/api/`. Add or change an endpoint and the page updates automatically; the
raw schema is at [`/api/openapi.json`](https://klappstuhl.me/api/openapi.json).

Keys are **scoped** (`images:*`, `links:*`, `pastes:*`) and minted on your
[account page](https://klappstuhl.me/account). Errors follow the 
(`{ message, code, errors }`) shape; rate limits send `Retry-After` + `X-RateLimit-*`.

Everything else lives in [`docs/`](docs):

- **[Setup](docs/setup.md)** — running it under Docker, every `config.json` key, and building from source.
- **[Features](docs/features.md)** — the account shell, the site insights, Discord login and the Percy dashboard, and where data and logs land on disk.

## Tech stack

- **Backend** — Rust, [Axum](https://github.com/tokio-rs/axum), Tokio, SQLite (rusqlite, bundled).
- **Templates** — [Askama](https://github.com/djc/askama) (compile-time server-side rendering).
- **Storage** — two SQLite files in the data dir: `main.db` (accounts, images, links, pastes,
  metrics, admin state) and `requests.db` (HTTP access log). Periodic `VACUUM INTO` backups land in `<data>/backups/`.

## License

AGPL-3.0 — see [LICENSE](LICENSE).
