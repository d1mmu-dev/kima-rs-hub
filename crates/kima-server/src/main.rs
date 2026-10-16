use anyhow::Context;
use kima_api::AppState;
use kima_core::config::Config;
use kima_core::secrets;
use tokio::net::TcpListener;
use tracing::info;
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

    let app = kima_server::app(AppState::new(pool, &secrets.session_secret), &config);
    let listener = TcpListener::bind(config.bind_addr)
        .await
        .with_context(|| format!("binding {}", config.bind_addr))?;
    info!(addr = %config.bind_addr, frontend = ?config.frontend_dir, "kima listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

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
