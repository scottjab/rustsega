//! The single binary: an axum server serving the frontend embedded by
//! `embed-web` (M8-01, #155).

use server::assets;

/// Where the server listens. Address and port flags are M8-02 (#156), so this is
/// loopback and fixed for now: the binary is for a person, not for a host.
const BIND: &str = "127.0.0.1:8080";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().init();

    #[cfg(not(feature = "embed-web"))]
    tracing::warn!("built without `embed-web`: serving the placeholder page, use `just build`");

    let app = server::router(assets::builtin());
    let listener = tokio::net::TcpListener::bind(BIND).await?;
    tracing::info!("rustsega on http://{BIND}");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

/// Resolves when the process is asked to stop: Ctrl-C, or SIGTERM from a
/// supervisor or `kill`.
async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            // Failing to subscribe to a signal there is no reason to shut down
            // now: keep waiting for Ctrl-C instead.
            Err(error) => tracing::warn!(%error, "no SIGTERM listener"),
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        result = ctrl_c => {
            if let Err(error) = result {
                tracing::warn!(%error, "Ctrl-C listener failed");
            }
        }
        () = terminate => {}
    }
    tracing::info!("stopping");
}
