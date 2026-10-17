use std::future::IntoFuture;
use std::time::Duration;

use anyhow::Context;
use kima_api::AppState;
use kima_core::config::Config;
use kima_core::secrets;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::from_env()?;
    // Upstream's `silent` has no EnvFilter equivalent; `off` is the same thing.
    let level = if config.log_level == "silent" {
        "off"
    } else {
        &config.log_level
    };
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(format!("{level},tower_http=info")))
        .init();

    let secrets = secrets::load_or_generate(
        &config.data_dir,
        config.session_secret.clone(),
        config.settings_encryption_key.clone(),
    )?;
    let pool = kima_db::connect(&config.database_url)
        .await
        .context("connecting to Postgres")?;
    kima_db::MIGRATOR
        .run(&pool)
        .await
        .context("running migrations")?;

    let library_root = kima_library::scan::effective_root(&pool, &config.music_path)
        .await
        .context("reading the library root from system settings")?;
    info!(root = %library_root.display(), "library root");
    let shutdown = CancellationToken::new();
    let state = AppState::with_shutdown(pool.clone(), &secrets.session_secret, shutdown.clone())
        .with_data_dir(config.data_dir.clone())
        .with_library_root(library_root.clone())
        .with_transcoder(kima_library::transcode::Transcoder::new(
            config.data_dir.join("transcode"),
            config.transcode_cache_mb * 1024 * 1024,
        ))
        .with_library_services(kima_api::LibraryServices::new(config.watch_mode));
    let library_ctx = kima_library::scan::LibraryCtx {
        pool: pool.clone(),
        root: library_root,
        events: state.events.clone(),
        data_dir: config.data_dir.clone(),
    };
    let mut registry = kima_server::jobs::Registry::new();
    registry.register_library(library_ctx.clone());
    // Before any worker starts: recover library jobs a dead process left running, then queue
    // the safety-net reconcile (spec §4.4).
    kima_server::jobs::prepare_library_queue(&pool)
        .await
        .context("preparing the library job queue")?;
    let workers = kima_server::jobs::spawn_all(&pool, registry, shutdown.clone());
    // Stopped by the shutdown token. With `KIMA_WATCH_MODE=off` this only records the status.
    let watcher = kima_library::watch::spawn(
        library_ctx,
        config.watch_mode,
        config.watch_poll_secs,
        state.library.ignore.clone(),
        state.library.watch_status.clone(),
        shutdown.clone(),
    );
    let app = kima_server::app(state, &config);
    let listener = TcpListener::bind(config.bind_addr)
        .await
        .with_context(|| format!("binding {}", config.bind_addr))?;
    info!(addr = %config.bind_addr, frontend = ?config.frontend_dir, "kima listening");
    let signalled = shutdown.clone();
    let server = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            // Ends SSE streams and anything else holding a connection open.
            signalled.cancel();
        })
        .into_future();
    tokio::pin!(server);
    let served = tokio::select! {
        res = &mut server => res,
        () = shutdown.cancelled() => {
            // Backstop: a connection that ignores shutdown must not keep the process alive.
            match tokio::time::timeout(SHUTDOWN_GRACE, &mut server).await {
                Ok(res) => res,
                Err(_) => {
                    warn!(grace = ?SHUTDOWN_GRACE, "connections still open; exiting anyway");
                    Ok(())
                }
            }
        }
    };
    // Runs whether serving ended by signal or by error, before the error is returned.
    shutdown.cancel();
    watcher.join().await;
    kima_server::jobs::drain(workers, SHUTDOWN_GRACE).await;
    served?;
    Ok(())
}

/// How long in-flight requests get to finish after a shutdown signal.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    info!("shutting down");
}
