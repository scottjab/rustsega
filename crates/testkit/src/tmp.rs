//! Test-only temp directory helper, shared by this crate's own tests.
//!
//! Edition 2024 made `std::env::set_var` `unsafe` and the workspace forbids
//! `unsafe`, so tests here can't point the env-var-backed helpers at a scratch
//! directory. They take a base directory as an argument instead and drive it
//! through this type.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// A fresh temp directory that deletes itself when the test ends, so repeated
/// runs don't litter `/tmp`.
pub(crate) struct TempDir(PathBuf);

impl TempDir {
    /// `tag` only shows up in the directory name, to make a failure easy to
    /// trace back to the test that made it.
    pub(crate) fn new(tag: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "rustsega-testkit-{tag}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

impl std::ops::Deref for TempDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}
