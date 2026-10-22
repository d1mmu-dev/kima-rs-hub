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

    // Before anything starts: a bad expression must fail startup, not the first fire.
    let sync_cron = config
        .library_sync_cron
        .as_deref()
        .map(|expr| {
            kima_jobs::schedule::Cron::parse(expr).map_err(|e| {
                anyhow::anyhow!(
                    "invalid KIMA_LIBRARY_SYNC_CRON {expr:?}: expected 5 cron fields or `off` ({e})"
                )
            })
        })
        .transpose()?;

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
    // One handle for the HTTP side, the job worker and the watcher: switching the library to
    // an organized copy changes it for all three.
    let library_root = kima_library::scan::LibraryRoot::new(library_root);
    let cipher = kima_core::cipher::Cipher::from_secret(&secrets.settings_encryption_key);
    let kv = kima_db::settings_kv::load(&pool, &cipher)
        .await
        .context("reading the key-value settings")?;
    let identify =
        kima_core::identify::IdentifySwitch::new(config.identify_env_off, kv.auto_enrich_metadata);
    // The one outbound HTTP client (spec §5): pauses survive restarts.
    let net = kima_net::Net::new(
        kima_net::NetConfig::production(
            env!("CARGO_PKG_VERSION"),
            &config.contact,
            &config.musicbrainz_base_url,
        ),
        pool.clone(),
        identify.clone(),
    );
    net.load_pauses()
        .await
        .context("reading the source pauses")?;
    net.set_concurrency(kv.enrichment_concurrency);
    // Fingerprinting needs ffmpeg; AcoustID can be switched off by env (spec §9.4).
    let ffmpeg = kima_ident::chromaprint::ffmpeg_available().await;
    if !ffmpeg {
        warn!("ffmpeg not found: fingerprinting is unavailable");
    }
    let fpcalc = ffmpeg && !config.acoustid_disabled;
    let state = AppState::with_shutdown(pool.clone(), &secrets.session_secret, shutdown.clone())
        .with_cipher(cipher)
        .with_identify(identify)
        .with_net(std::sync::Arc::new(net))
        .with_data_dir(config.data_dir.clone())
        .with_shared_library_root(library_root.clone())
        .with_organize_target(config.organize_target.clone())
        .with_public_url(config.public_url.clone())
        .with_fpcalc(fpcalc)
        .with_fanart_env_key(config.fanart_api_key.clone())
        .with_artwork_cache_mb(config.artwork_cache_mb)
        .with_transcoder(kima_library::transcode::Transcoder::new(
            config.data_dir.join("transcode"),
            config.transcode_cache_mb * 1024 * 1024,
        ))
        .with_library_services(
            kima_api::LibraryServices::new(config.watch_mode).with_sync_cron(sync_cron.clone()),
        );
    let library_ctx = kima_library::scan::LibraryCtx {
        pool: pool.clone(),
        root: library_root,
        events: state.events.clone(),
        data_dir: config.data_dir.clone(),
        ignore: state.library.ignore.clone(),
    };
    // Built from the same pieces the HTTP side holds (spec §10.6). `fingerprinting` is the
    // `fpcalc` feature (ffmpeg present, AcoustID not disabled, spec §9.4).
    let ident_ctx = kima_ident::IdentCtx {
        pool: pool.clone(),
        net: state.net.clone(),
        cipher: state.cipher.clone(),
        events: state.events.clone(),
        library: library_ctx.clone(),
        settings: state.settings.clone(),
        config: kima_ident::IdentConfig {
            fingerprinting: state.fpcalc,
            fingerprint_workers: config.fingerprint_workers,
            acoustid_disabled: config.acoustid_disabled,
            acoustid_env_key: config.acoustid_client_key.clone(),
            fanart_env_key: state.fanart_env_key.clone(),
        },
        last_cycle: Default::default(),
    };
    info!(config = ?ident_ctx.config, "identification");
    let mut registry = kima_server::jobs::Registry::new();
    registry.register_library(library_ctx.clone());
    registry.register_enrich(ident_ctx);
    // Before any worker starts: recover library jobs a dead process left running, then queue
    // the safety-net reconcile (spec §4.4).
    kima_server::jobs::prepare_library_queue(&pool)
        .await
        .context("preparing the library job queue")?;
    // Likewise for the coordinator: requeue its cycle, reset `enriching` artists, kick.
    kima_ident::coordinator::prepare(&pool)
        .await
        .context("preparing the enrichment queue")?;
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
    // Always runs: the safety-net reconcile when enabled (D-091), the cache prune and the
    // enrichment backstop kick. Stopped by the shutdown token.
    let scheduler = kima_server::schedule_entries(&config, sync_cron)
        .into_iter()
        .fold(
            kima_jobs::schedule::Scheduler::new(),
            |s, (name, cron, f)| s.add(name, cron, f),
        )
        .spawn(pool.clone(), shutdown.clone());
    let app = kima_server::app(state, &config);
    let listener = TcpListener::bind(config.bind_addr)
        .await
        .with_context(|| format!("binding {}", config.bind_addr))?;
    info!(addr = %config.bind_addr, frontend = ?config.frontend_dir, "kima listening");
    let signalled = shutdown.clone();
    let server = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
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
    let _ = scheduler.await;
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
