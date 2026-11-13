//! Where the server gets the bytes it serves.
//!
//! The router only ever sees the [`AssetSource`] trait, which is what lets
//! `crates/server/tests/static_files.rs` drive it from a `HashMap` and lets the
//! same router serve either the embedded frontend or a placeholder (#155).

use std::borrow::Cow;
use std::sync::Arc;

/// One frontend file, ready to be written into a response.
#[derive(Clone)]
pub struct Asset {
    /// The file bytes. A `Cow` so the embedded source can hand out the
    /// `&'static [u8]` that `rust-embed` put in the binary instead of copying
    /// it on every request.
    pub bytes: Cow<'static, [u8]>,
    /// MIME type with no parameters, e.g. `application/wasm`. The router appends
    /// `; charset=utf-8` to the text types that need one.
    pub mime: String,
}

/// The static files a [`crate::router`] serves.
pub trait AssetSource: Send + Sync + 'static {
    /// The file stored under `path`, or `None` when there is none.
    ///
    /// `path` is a key with no leading `/` (`index.html`, `app-abc123.wasm`),
    /// normalized by the router, which rejects `.` and `..` components before
    /// asking. Implementations may join it to a directory, so that rejection is
    /// what keeps them out of the filesystem.
    fn get(&self, path: &str) -> Option<Asset>;
}

/// The frontend compiled into the binary from `crates/web/dist`.
///
/// Needs `just build`, which runs trunk before it turns on `embed-web`: with the
/// feature off the binary serves [`PlaceholderAssets`], so clippy, nextest and
/// the wasm builds never need `crates/web/dist` to exist.
#[cfg(feature = "embed-web")]
pub struct EmbeddedAssets;

/// `rust-embed` resolves a relative `folder` against this crate's
/// `CARGO_MANIFEST_DIR`, so this is `crates/web/dist`.
///
/// Without the `debug-embed` feature it re-reads the folder at runtime in debug
/// builds and embeds the bytes in release ones. That is fine here because the
/// only build that turns this on is `cargo build --release`.
#[cfg(feature = "embed-web")]
#[derive(rust_embed::RustEmbed)]
#[folder = "../web/dist"]
struct Frontend;

#[cfg(feature = "embed-web")]
impl AssetSource for EmbeddedAssets {
    fn get(&self, path: &str) -> Option<Asset> {
        let file = Frontend::get(path)?;
        Some(Asset {
            bytes: file.data,
            mime: file.metadata.mimetype().to_owned(),
        })
    }
}

/// The frontend of a binary that was built without it: one page explaining how
/// to get the real one.
pub struct PlaceholderAssets;

/// What `GET /` returns without `embed-web`. Kept to one page and no assets, so
/// there is nothing for a placeholder binary to get wrong.
const PLACEHOLDER_INDEX: &str = concat!(
    "<!doctype html>\n",
    "<meta charset=utf-8>\n",
    "<title>rustsega</title>\n",
    "<h1>rustsega</h1>\n",
    "<p>This binary was built without the frontend embedded.</p>\n",
    "<p>Run <code>nix develop -c just build</code> for the single binary that serves the app.</p>\n",
);

impl AssetSource for PlaceholderAssets {
    fn get(&self, path: &str) -> Option<Asset> {
        match path {
            "index.html" => Some(Asset {
                bytes: Cow::Borrowed(PLACEHOLDER_INDEX.as_bytes()),
                mime: "text/html".to_owned(),
            }),
            _ => None,
        }
    }
}

/// The assets this binary can serve: the embedded frontend with `embed-web`,
/// the placeholder page without it.
pub fn builtin() -> Arc<dyn AssetSource> {
    #[cfg(feature = "embed-web")]
    {
        Arc::new(EmbeddedAssets)
    }
    #[cfg(not(feature = "embed-web"))]
    {
        Arc::new(PlaceholderAssets)
    }
}
