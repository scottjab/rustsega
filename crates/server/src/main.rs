//! The single binary: an axum server serving the frontend embedded by
//! `embed-web` (M8-01, #155), configured from the command line and the
//! environment (M8-02, #156).

use std::sync::Arc;

use clap::Parser as _;
use server::AppState;
use server::assets;
use server::cli::{self, Args};
use server::library::LibrarySlot;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Exits here, with clap's format and status 2, on a flag it cannot parse.
    let args = Args::parse();
    tracing_subscriber::fmt().with_max_level(args.log).init();

    // Before the socket, because a wrong `--rom-dir` is a mistake in the command
    // line: it should read as one on the terminal, not as a server whose ROM
    // library happens to be empty.
    let addr = args.addr();
    let rom_dir = cli::check_rom_dir(args.rom_dir).unwrap_or_else(|error| error.exit());

    // Scanned before the socket, and a scan that fails is as much a wrong command
    // line as a directory that is not there. [`LibrarySlot::scan`] logs how many
    // ROMs it found, which is the line that says what this server will offer.
    let library = match rom_dir {
        Some(dir) => LibrarySlot::scan(dir)?,
        None => LibrarySlot::empty(),
    };

    #[cfg(not(feature = "embed-web"))]
    tracing::warn!("built without `embed-web`: serving the placeholder page, use `just build`");

    let state = Arc::new(AppState::new(assets::builtin()).with_library(library));
    // `LibrarySlot` is one `Arc`ed scan, so this is a handle on the library the
    // router is serving rather than a copy of it. It is taken because the
    // exposure line below can only be written once the socket says which address
    // it actually took, and by then `state` has been moved into the router.
    let served = state.library.clone();
    let app = server::router(state);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    // The address the socket ended up with, not the one that was asked for: with
    // `--port 0` those differ, and a startup line naming port 0 is no use to
    // whoever has to connect. `crates/server/tests/running_binary.rs` reads this
    // line to find the server it started.
    let listening = listener.local_addr()?;
    tracing::info!("rustsega listening on http://{listening}");

    // A ROM library behind a reachable address is a public ROM library (#415).
    // There is no authentication anywhere in M8 - TLS and access control are
    // explicitly the reverse proxy's job - and an id is `sha1(relative path)`,
    // which is not a secret, so `POST /api/roms/rescan` recomputes the whole
    // listing for anyone who can reach the port.
    //
    // So it says so, in one line, naming the address and how many files are on the
    // way out. It does not refuse: `rustsega --bind 0.0.0.0 --port 9000` is an
    // acceptance criterion of #156, and the decision on #415 is that whether to
    // expose the collection belongs to whoever runs this, who is being told rather
    // than overruled. `crates/server/tests/running_binary.rs` holds the other half
    // of that promise: a gate here is a decision someone has to make by editing
    // those tests.
    if !listening.ip().is_loopback()
        && let Some(library) = served.get()
    {
        tracing::warn!(
            "ROM library is served to {listening} with no authentication: {} files are listable \
             and downloadable by anyone who can reach this address, and `POST \
             /api/roms/rescan` recomputes the list for them. Serve it only on a network you \
             trust, or behind a proxy that authenticates.",
            library.entries().len()
        );
    }

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
