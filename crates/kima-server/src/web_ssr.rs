//! The embedded vertigo SSR server for `kima-web` (M9 spec §3, §6.1, §12).
//!
//! `vertigo_cli::serve::run` runs in this process on a loopback port; wasmtime executes the
//! `kima-web` wasm for every server-rendered page and [`crate::web_proxy`] forwards the
//! `kima-web` routes to it.
//!
//! **Liveness: crash and restart (owner decision).** Once the SSR server is up, its task
//! ending for any reason other than a requested shutdown (a panic, an error such as a failed
//! bind, or a quiet return) is fatal: [`WebSsr::failed`] resolves and `main` exits non-zero,
//! so the container's restart policy brings the whole process back. There is no mode that
//! keeps serving the other routes while `kima-web` answers 5xx.

use std::future::Future;
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use vertigo_cli::CommonOpts;
use vertigo_cli::serve::{MountConfigBuilder, ServeOpts, ServeOptsInner};

use crate::web_proxy::WebProxy;

/// How long the first start may take: wasmtime compiles the wasm before the port opens.
const READY_TIMEOUT: Duration = Duration::from_secs(180);

/// How long a task that returned (without failing) waits for a shutdown request to explain it.
/// vertigo's server also stops on SIGTERM/Ctrl-C by itself, a moment before or after ours.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// A running embedded SSR server and the watch over its task.
pub struct WebSsr {
    /// The allow-list proxy to put in front of `kima-server`'s routes.
    pub proxy: Arc<WebProxy>,
    watch: JoinHandle<Result<(), String>>,
}

impl WebSsr {
    /// Validates the build, claims `127.0.0.1:port`, starts the server and waits until it
    /// accepts connections. Any failure here is a startup failure.
    ///
    /// `build_dir` is the output of `vertigo build` (`index.json`, the `.wasm`, `wasm_run.*.js`).
    /// vertigo serves it under its own path as given, so a relative directory is relative to the
    /// working directory and an absolute one appears in the public asset URLs.
    /// `fetch_base` is the origin relative fetches made during rendering resolve against
    /// (`kima-server` itself, so `/api/...` reaches the real API).
    pub async fn start(
        build_dir: &Path,
        port: u16,
        fetch_base: String,
        shutdown: CancellationToken,
    ) -> anyhow::Result<Self> {
        let dest_dir = build_dir
            .to_str()
            .ok_or_else(|| anyhow!("KIMA_WEB_BUILD_DIR is not valid UTF-8"))?
            .to_owned();
        // Reads `index.json`; the file names it holds are the only assets the proxy lets through.
        let mount = MountConfigBuilder::new("/", dest_dir.clone())
            .build()
            .map_err(|e| {
                anyhow!("{dest_dir} is not a `vertigo build` output (no usable index.json): {e:?}")
            })?;
        let assets = vec![mount.get_wasm_http_path(), mount.get_run_js_http_path()];

        // vertigo would wait 20 s for a busy port and only then fail; say so at once instead.
        drop(
            TcpListener::bind((Ipv4Addr::LOCALHOST, port))
                .with_context(|| format!("the embedded SSR port 127.0.0.1:{port} is not free"))?,
        );

        let opts = ServeOpts {
            inner: ServeOptsInner {
                host: "127.0.0.1".into(),
                port,
                proxy: vec![],
                mount_point: "/".into(),
                env: vec![],
                wasm_preload: false,
                disable_hydration: false,
                ssr_fetch_base: Some(fetch_base),
                pretty_html: false,
                disable_compression: false,
                threads: Some(2),
            },
            common: CommonOpts {
                dest_dir,
                log_local_time: None,
            },
        };
        let watch = tokio::spawn(supervise(
            vertigo_cli::serve::run(opts, None),
            shutdown,
            SHUTDOWN_GRACE,
        ));

        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let deadline = tokio::time::Instant::now() + READY_TIMEOUT;
        loop {
            if watch.is_finished() {
                let why = match watch.await {
                    Ok(Err(why)) => why,
                    Ok(Ok(())) => "it stopped".into(),
                    Err(e) => format!("its watcher failed: {e}"),
                };
                bail!("the embedded SSR server did not start: {why}");
            }
            if tokio::net::TcpStream::connect(addr).await.is_ok() {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                watch.abort();
                bail!(
                    "the embedded SSR server was not listening on {addr} after {READY_TIMEOUT:?}"
                );
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        tracing::info!(%addr, "embedded kima-web SSR is up");
        Ok(Self {
            proxy: Arc::new(WebProxy::new(port, assets)),
            watch,
        })
    }

    /// A `WebSsr` over any task, supervised as the real one is. For tests of the liveness policy.
    pub fn from_task<F, E>(proxy: Arc<WebProxy>, task: F, shutdown: CancellationToken) -> Self
    where
        F: Future<Output = Result<(), E>> + Send + 'static,
        E: std::fmt::Debug + Send + 'static,
    {
        Self {
            proxy,
            watch: tokio::spawn(supervise(task, shutdown, SHUTDOWN_GRACE)),
        }
    }

    /// Resolves with the reason when the SSR server has died. Stays pending while it runs and
    /// after a requested shutdown, so it can sit in a `select!` next to the HTTP server.
    pub async fn failed(&mut self) -> anyhow::Error {
        match (&mut self.watch).await {
            Ok(Err(why)) => anyhow!("the embedded SSR server failed: {why}"),
            Ok(Ok(())) => std::future::pending().await,
            Err(e) => anyhow!("the embedded SSR watcher failed: {e}"),
        }
    }
}

/// Runs `task` and says how it ended. `Ok(())` only for a task that ended after `shutdown` was
/// requested (within `grace`); a panic, an error and a return nobody asked for are all `Err`.
pub async fn supervise<F, E>(
    task: F,
    shutdown: CancellationToken,
    grace: Duration,
) -> Result<(), String>
where
    F: Future<Output = Result<(), E>> + Send + 'static,
    E: std::fmt::Debug + Send + 'static,
{
    match tokio::spawn(task).await {
        Err(e) if e.is_panic() => Err("the task panicked".into()),
        Err(e) => Err(format!("the task was cancelled: {e}")),
        Ok(Err(e)) => Err(format!("the task returned an error: {e:?}")),
        Ok(Ok(())) => match tokio::time::timeout(grace, shutdown.cancelled()).await {
            Ok(()) => Ok(()),
            Err(_) => Err("the task returned although no shutdown was requested".into()),
        },
    }
}

/// Drives the HTTP `server` until it ends, and ends it when `shutdown` is requested or the SSR
/// server dies. A dead SSR server is the crash-and-restart case: `shutdown` is requested, the
/// server gets `grace` to drain, and the result is the error (so the process exits non-zero).
/// A server that ignores the shutdown request for longer than `grace` is abandoned.
pub async fn serve<S>(
    server: &mut S,
    ssr: Option<&mut WebSsr>,
    shutdown: &CancellationToken,
    grace: Duration,
) -> anyhow::Result<()>
where
    S: Future<Output = std::io::Result<()>> + Unpin,
{
    let ssr_died = async {
        match ssr {
            Some(ssr) => ssr.failed().await,
            None => std::future::pending().await,
        }
    };
    tokio::select! {
        res = &mut *server => Ok(res?),
        () = shutdown.cancelled() => {
            // Backstop: a connection that ignores shutdown must not keep the process alive.
            match tokio::time::timeout(grace, &mut *server).await {
                Ok(res) => Ok(res?),
                Err(_) => {
                    tracing::warn!(?grace, "connections still open; exiting anyway");
                    Ok(())
                }
            }
        }
        why = ssr_died => {
            tracing::error!(error = %why, "exiting so the supervisor can restart kima");
            shutdown.cancel();
            let _ = tokio::time::timeout(grace, &mut *server).await;
            Err(why)
        }
    }
}
