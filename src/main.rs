use std::{convert::Infallible, net::SocketAddr, str::FromStr, sync::Arc, time::Duration};

use anyhow::Context;
use axum::{
    extract::{DefaultBodyLimit, Request},
    middleware, Extension, ServiceExt,
};
use futures_util::StreamExt;
use hyper::body::Incoming;
use hyper_util::rt::{TokioExecutor, TokioIo};
use rustls_acme::AcmeConfig;
use rustls_acme::{caches::DirCache, is_tls_alpn_challenge};
use tokio_rustls::LazyConfigAcceptor;
use tower::{limit::GlobalConcurrencyLimitLayer, Layer, Service, ServiceExt as _};
use tower_http::{
    compression::CompressionLayer,
    normalize_path::NormalizePathLayer,
    services::{ServeDir, ServeFile},
};
use tracing::{error, info};
use tracing_appender::{non_blocking::WorkerGuard, rolling::Rotation};
use tracing_subscriber::{
    filter::{LevelFilter, Targets},
    fmt::format::FmtSpan,
    layer::SubscriberExt,
    util::SubscriberInitExt,
    Layer as _,
};

fn unwrap_infallible<T>(result: Result<T, Infallible>) -> T {
    match result {
        Ok(value) => value,
        Err(err) => match err {},
    }
}

fn setup_logging() -> anyhow::Result<(WorkerGuard, WorkerGuard)> {
    let rust_log_var = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string());
    let log_filter = Targets::from_str(&rust_log_var)?.with_target("bad_request", LevelFilter::OFF);
    let file_appender = tracing_appender::rolling::Builder::new()
        .max_log_files(60)
        .symlink("today.log")
        .rotation(Rotation::DAILY)
        .filename_suffix("log")
        .build(klappstuhl_me::utils::logs_directory())?;

    let bad_request_filter = Targets::new().with_target("bad_request", tracing::Level::INFO);
    let bad_request = tracing_appender::rolling::Builder::new()
        .max_log_files(5)
        .symlink("bad_requests.log")
        .rotation(Rotation::DAILY)
        .filename_prefix("bad_requests")
        .filename_suffix("log")
        .build(klappstuhl_me::utils::logs_directory())?;

    let (non_blocking_main, main_guard) = tracing_appender::non_blocking(file_appender);
    let (non_blocking_bad_req, bad_req_guard) = tracing_appender::non_blocking(bad_request);
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .json()
                .with_writer(non_blocking_main)
                .with_span_events(FmtSpan::CLOSE)
                .with_filter(log_filter),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .compact()
                .with_ansi(false)
                .with_level(false)
                .with_target(false)
                .with_writer(non_blocking_bad_req)
                .with_filter(bad_request_filter),
        )
        .init();
    Ok((main_guard, bad_req_guard))
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

async fn run_server(state: klappstuhl_me::AppState) -> anyhow::Result<()> {
    let config = state.config().clone();
    let _ = klappstuhl_me::CONFIG.set(config.clone());
    let addr = config.server.address();
    let secret_key = config.secret_key;

    let request_logger = state.requests.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(3600));
        loop {
            interval.tick().await;
            if !request_logger.cleanup() {
                break;
            }
        }
    });

    // Middleware order for request processing is bottom to top
    // and for response processing it's top to bottom
    let router = klappstuhl_me::routes::all()
        .nest_service("/favicon.ico", ServeFile::new("static/img/favicon.ico"))
        .nest_service("/site.webmanifest", ServeFile::new("static/site.webmanifest"))
        .nest_service("/robots.txt", ServeFile::new("static/robots.txt"))
        .nest_service("/static", ServeDir::new("static"))
        .layer(middleware::from_fn_with_state(state.clone(), klappstuhl_me::copy_api_token))
        .layer(klappstuhl_me::logging::HttpTrace::new(state.requests.clone()))
        .layer(middleware::from_fn(klappstuhl_me::flash::process_flash_messages))
        .layer(middleware::from_fn(klappstuhl_me::parse_cookies))
        .layer(Extension(secret_key))
        .layer(Extension(klappstuhl_me::cached::BodyCache::new(Duration::from_secs(120))))
        .layer(DefaultBodyLimit::max(klappstuhl_me::MAX_BODY_SIZE))
        .layer(tower_http::limit::RequestBodyLimitLayer::new(klappstuhl_me::MAX_BODY_SIZE))
        .layer(CompressionLayer::new())
        .layer(GlobalConcurrencyLimitLayer::new(512))
        .with_state(state);

    let app = NormalizePathLayer::trim_trailing_slash().layer(router);
    let mut service = ServiceExt::<Request>::into_make_service_with_connect_info::<SocketAddr>(app);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("Could not bind to {addr}"))?;

    if !config.production || addr.port() != 443 {
        axum::serve(listener, service)
            .with_graceful_shutdown(shutdown_signal())
            .await
            .context("Failed during server service")?;

        return Ok(());
    }

    // Production server stuff
    if addr.port() == 443 {
        let cache_dir = dirs::cache_dir()
            .map(|p| p.join(klappstuhl_me::PROGRAM_NAME).join("rustls_acme_cache"))
            .context("Could not find appropriate cache location for ACME")?;
        let mut state = AcmeConfig::new(config.domains)
            .cache(DirCache::new(cache_dir))
            .state();

        let supported_alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        let mut challenge_config = state.challenge_rustls_config();
        let mut default_config = state.default_rustls_config();
        if let Some(config) = Arc::get_mut(&mut challenge_config) {
            config.alpn_protocols.extend(supported_alpn_protocols.clone());
        }
        if let Some(config) = Arc::get_mut(&mut default_config) {
            config.alpn_protocols.extend(supported_alpn_protocols);
        }
        tokio::spawn(async move {
            loop {
                match state.next().await {
                    Some(Ok(ok)) => info!("ACME event: {:?}", ok),
                    Some(Err(err)) => error!("ACME error: {:?}", err),
                    None => break,
                }
            }
        });

        loop {
            let (tcp, addr) = match listener.accept().await {
                Ok(conn) => conn,
                Err(e) => {
                    // Connection errors can be ignored
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::ConnectionRefused
                            | std::io::ErrorKind::ConnectionAborted
                            | std::io::ErrorKind::ConnectionReset
                    ) {
                        continue;
                    }

                    // If we get any other type of error then just log it and sleep for a little bit
                    // and try again. According to hyper's old server implementation
                    // https://github.com/hyperium/hyper/blob/v0.14.27/src/server/tcp.rs#L184-L198
                    //
                    // They used to sleep if the file limit was reached, presumably to let other files
                    // close down.
                    error!("error during accept loop: {e}");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
            };
            let challenge_config = challenge_config.clone();
            let default_config = default_config.clone();
            let tower_service = unwrap_infallible(service.call(addr).await);

            tokio::spawn(async move {
                let start_handshake = match LazyConfigAcceptor::new(Default::default(), tcp).await {
                    Err(e) => {
                        eprintln!("failed to start handshake accept: {e:?}");
                        return;
                    }
                    Ok(s) => s,
                };

                let stream = if is_tls_alpn_challenge(&start_handshake.client_hello()) {
                    info!("Received TLS-ALPN-01 validation request");
                    start_handshake.into_stream(challenge_config).await
                } else {
                    start_handshake.into_stream(default_config).await
                };

                let socket = match stream {
                    Err(e) => {
                        eprintln!("failed to start handshake: {e:?}");
                        return;
                    }
                    Ok(stream) => TokioIo::new(stream),
                };
                let hyper_service = hyper::service::service_fn(move |request: Request<Incoming>| {
                    tower_service.clone().oneshot(request)
                });

                let serve = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                    .serve_connection(socket, hyper_service)
                    .await;
                if let Err(e) = serve {
                    eprintln!("failed to serve connection: {e:#}");
                }
            });
        }
    }

    axum::serve(listener, service)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("Failed during server service")?;
    Ok(())
}

const MIGRATIONS: [&str; 2] = [include_str!("../sql/0.sql"), include_str!("../sql/1.sql")];

fn init_db(connection: &mut rusqlite::Connection) -> rusqlite::Result<()> {
    let tx = connection.transaction()?;
    tx.execute_batch("PRAGMA foreign_keys=1;\nPRAGMA journal_mode=wal;")?;
    let version: usize = {
        let mut stmt = tx.prepare_cached("PRAGMA user_version;")?;
        stmt.query_row([], |r| r.get(0))?
    };
    for migration in MIGRATIONS.iter().skip(version) {
        tx.execute_batch(migration)?;
    }
    tx.commit()
}

async fn run(command: klappstuhl_me::Command) -> anyhow::Result<()> {
    let config = klappstuhl_me::Config::load()?;
    let database = klappstuhl_me::Database::file(&klappstuhl_me::database::directory()?)
        .with_init(init_db)
        .open()
        .await?;

    let state = klappstuhl_me::AppState::new(config, database).await;
    match command {
        klappstuhl_me::Command::Run => run_server(state).await,
        klappstuhl_me::Command::Admin => {
            let credentials = klappstuhl_me::cli::prompt_admin_account()?;
            let mut flags = klappstuhl_me::models::AccountFlags::default();
            flags.set_admin(true);
            state
                .database()
                .execute(
                    "INSERT INTO account(name, password, flags) VALUES (?, ?, ?)",
                    (credentials.username.clone(), credentials.password_hash, flags),
                )
                .await?;
            info!("successfully created account {}", credentials.username);
            Ok(())
        }
        _ => Err(anyhow::anyhow!("unknown command")),
    }
}

#[tokio::main]
async fn main() {
    let _guard = match setup_logging() {
        Ok(guard) => guard,
        Err(e) => {
            eprintln!("Error setting up logger:\n{e:?}");
            return;
        }
    };

    let command = klappstuhl_me::Command::parse();
    if let Err(e) = run(command).await {
        eprintln!("Error occurred during main execution:\n{e:?}");

        error!(error = %e,"error occurred during main execution");
        for e in e.chain().skip(1) {
            error!(cause = %e)
        }
    }
}