# Klappstuhl.me

Personal website and image hosting service built with Rust.

## Tech Stack

- **Backend:** Rust, [Axum](https://github.com/tokio-rs/axum), SQLite (via rusqlite), Tokio
- **Templates:** [Askama](https://github.com/djc/askama) (server-side rendering)
- **TLS:** Automatic Let's Encrypt via rustls-acme
- **API Docs:** OpenAPI 3.0 via utoipa + Scalar

## Requirements

Rust 1.74 or higher.

## Building

```
cargo build --release
```

The `static/` directory must be placed next to the compiled binary at runtime.

## Running

```
./klappstuhl_me
```

To create an admin account interactively:

```
./klappstuhl_me admin
```

## Configuration

Configuration is loaded from a JSON file. The location depends on the OS:

| OS      | Path                                                               |
|---------|--------------------------------------------------------------------|
| Linux   | `$XDG_CONFIG_HOME/klappstuhl_me/config.json`                      |
| macOS   | `$HOME/Library/Application Support/klappstuhl_me/config.json`     |
| Windows | `%AppData%\klappstuhl_me\config.json`                              |

A default config file is created automatically on first run. All available options are documented in [`src/config.rs`](src/config.rs).

## Data & Logs

**Database:**

| OS      | Path                                                          |
|---------|---------------------------------------------------------------|
| Linux   | `$XDG_DATA_HOME/klappstuhl_me/main.db`                       |
| macOS   | `$HOME/Library/Application Support/klappstuhl_me/main.db`    |
| Windows | `%AppData%\klappstuhl_me\main.db`                             |

**Logs:**

| OS      | Path                   |
|---------|------------------------|
| Linux   | `$XDG_STATE_HOME/klappstuhl_me/` |
| macOS   | `./logs/`              |
| Windows | `./logs/`              |

## License

AGPL-3.0 — see [LICENSE](LICENSE).
