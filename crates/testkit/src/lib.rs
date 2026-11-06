//! Shared test helpers: ROM discovery, frame hashing, golden files.
//!
//! Discovery is the part every crate needs before it can run any test that
//! reads data off disk: one place to look for public test suites and for the
//! owner's commercial ROMs, and one way to *skip* (never fail) when the data
//! is absent, so CI without commercial ROMs stays green. See
//! `docs/TESTING.md` ("External test data").

use std::path::{Path, PathBuf};

#[cfg(test)]
mod tmp;

pub mod golden;
pub use golden::{Golden, assert_golden, dump_png, frame_hash};

/// Public test suites and homebrew test ROMs, fetched by
/// `scripts/fetch-test-data.sh`.
const TEST_ROM_DIR_ENV: &str = "RUSTSEGA_TEST_ROM_DIR";

/// Commercial ROMs, local only. Files use the `.md` extension but are raw
/// binary ROMs.
const ROM_DIR_ENV: &str = "RUSTSEGA_ROM_DIR";

/// Path of the repo root, resolved from this crate's own manifest dir so it
/// does not depend on the cwd. Tests run with the crate dir as cwd.
pub fn workspace_root() -> PathBuf {
    // testkit lives at crates/testkit, so the root is two levels up.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

/// Directory holding public test data: `$RUSTSEGA_TEST_ROM_DIR`, else
/// `<workspace_root>/test-roms`.
pub fn test_rom_dir() -> PathBuf {
    match std::env::var_os(TEST_ROM_DIR_ENV) {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => workspace_root().join("test-roms"),
    }
}

/// Directory holding the owner's commercial ROMs: `$RUSTSEGA_ROM_DIR`, else
/// `$HOME/games/roms/genesis`.
///
/// With neither var set there is no useful guess, so the fallback is the
/// relative `games/roms/genesis`, which does not exist in practice: callers
/// get `None` and skip.
pub fn rom_dir() -> PathBuf {
    match std::env::var_os(ROM_DIR_ENV) {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => match std::env::var_os("HOME") {
            Some(home) => PathBuf::from(home).join("games/roms/genesis"),
            None => PathBuf::from("games/roms/genesis"),
        },
    }
}

/// Absolute path to `rel` under `base`, or `None` if it isn't a readable file.
///
/// The shared guts of every lookup here: taking the base dir as an argument
/// keeps the logic testable, because overriding the env vars would need
/// `std::env::set_var`, which is `unsafe` in edition 2024.
fn existing_file_in(base: &Path, rel: &str) -> Option<PathBuf> {
    let path = base.join(rel);
    if path.is_file() { Some(path) } else { None }
}

/// First file directly inside `base` whose name starts with `prefix`, taking
/// the files in sorted name order so the result is stable.
///
/// Directories are skipped: a folder called e.g. `Sonic (USA).md.backup` must
/// not be mistaken for a ROM.
fn find_file_in(base: &Path, prefix: &str) -> Option<PathBuf> {
    let mut names = Vec::new();
    let entries = std::fs::read_dir(base).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = entry.file_name();
        if name.to_string_lossy().starts_with(prefix) {
            names.push(name.to_string_lossy().into_owned());
        }
    }
    names.sort();
    names.first().map(|name| base.join(name))
}

/// Public test ROM by path relative to [`test_rom_dir`], e.g.
/// `"680x0/68000/v1/ADDQ.json"`. `None` means the data is not present.
pub fn test_rom(rel: &str) -> Option<PathBuf> {
    test_rom_in(&test_rom_dir(), rel)
}

/// [`test_rom`] against an explicit base dir. Public so tests can cover it
/// without touching the process-global environment.
pub fn test_rom_in(base: &Path, rel: &str) -> Option<PathBuf> {
    existing_file_in(base, rel)
}

/// Commercial ROM by exact file name, e.g.
/// `"Sonic The Hedgehog (USA, Europe).md"`. `None` when absent.
pub fn commercial_rom(file_name: &str) -> Option<PathBuf> {
    commercial_rom_in(&rom_dir(), file_name)
}

/// [`commercial_rom`] against an explicit base dir.
pub fn commercial_rom_in(base: &Path, file_name: &str) -> Option<PathBuf> {
    existing_file_in(base, file_name)
}

/// Commercial ROM found by name prefix, e.g. `"Sonic The Hedgehog (USA"`. Use
/// this when the region/revision suffix in the file name is not known. `None`
/// when no file matches.
pub fn find_commercial_rom(prefix: &str) -> Option<PathBuf> {
    find_file_in(&rom_dir(), prefix)
}

/// [`find_commercial_rom`] against an explicit base dir.
pub fn find_commercial_rom_in(base: &Path, prefix: &str) -> Option<PathBuf> {
    find_file_in(base, prefix)
}

/// Unwrap a data lookup, or return early from the calling test with a
/// `SKIPPED:` line when the data is missing.
///
/// Missing data is a skip, not a failure: CI has the public suites but never
/// the commercial ROMs.
///
/// The expansion is a bare `return;`, so use it in a function that returns
/// `()`. That is every plain `#[test] fn`; a `Result`-returning test has to
/// match on the `Option` itself.
///
/// ```no_run
/// # use testkit::require_data;
/// # fn it() {
/// let rom = require_data!(testkit::commercial_rom("Sonic The Hedgehog (USA, Europe).md"));
/// let _ = rom;
/// # }
/// ```
#[macro_export]
macro_rules! require_data {
    ($opt:expr) => {
        match $opt {
            Some(value) => value,
            None => {
                eprintln!("SKIPPED: missing test data: {}", stringify!($opt));
                return;
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmp::TempDir;
    use std::fs;
    use std::path::Path;

    /// Smoke test so `cargo nextest run --workspace` has something to run
    /// before any real code lands (ticket M0-00).
    #[test]
    fn workspace_builds() {
        assert_eq!(1 + 1, 2);
    }

    fn write_stub(path: &Path, bytes: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dirs");
        }
        // Placeholder bytes only: never real ROM data.
        fs::write(path, bytes).expect("write stub file");
    }

    #[test]
    fn workspace_root_points_at_the_repo_root() {
        let root = workspace_root();
        assert!(
            root.join("Cargo.toml").is_file(),
            "no workspace Cargo.toml at {}",
            root.display()
        );
        assert!(
            root.join("crates/testkit").is_dir(),
            "crates/testkit missing under {}",
            root.display()
        );
    }

    #[test]
    fn test_rom_dir_prefers_env_var() {
        match std::env::var_os("RUSTSEGA_TEST_ROM_DIR") {
            Some(v) => assert_eq!(test_rom_dir(), PathBuf::from(v)),
            None => assert!(test_rom_dir().ends_with("test-roms")),
        }
    }

    #[test]
    fn rom_dir_prefers_env_var() {
        match std::env::var_os("RUSTSEGA_ROM_DIR") {
            Some(v) => assert_eq!(rom_dir(), PathBuf::from(v)),
            None => assert!(rom_dir().ends_with("games/roms/genesis")),
        }
    }

    #[test]
    fn test_rom_in_finds_existing_file() {
        let dir = TempDir::new("test-rom");
        write_stub(&dir.join("a.bin"), b"stub");
        assert_eq!(test_rom_in(&dir, "a.bin"), Some(dir.join("a.bin")));
    }

    #[test]
    fn test_rom_in_resolves_nested_paths() {
        let dir = TempDir::new("test-rom-nested");
        write_stub(&dir.join("680x0/68000/v1/ADDQ.json"), b"stub");
        assert_eq!(
            test_rom_in(&dir, "680x0/68000/v1/ADDQ.json"),
            Some(dir.join("680x0/68000/v1/ADDQ.json"))
        );
    }

    #[test]
    fn test_rom_missing_returns_none() {
        let dir = TempDir::new("test-rom-missing");
        assert_eq!(test_rom_in(&dir, "definitely-not-here.bin"), None);
        assert!(test_rom("definitely-not-here.bin").is_none());
    }

    #[test]
    fn commercial_rom_in_finds_existing_file() {
        let dir = TempDir::new("rom");
        let name = "Sonic The Hedgehog (USA, Europe).md";
        write_stub(&dir.join(name), b"stub");
        assert_eq!(commercial_rom_in(&dir, name), Some(dir.join(name)));
    }

    #[test]
    fn commercial_rom_missing_returns_none() {
        let dir = TempDir::new("rom-missing");
        assert_eq!(commercial_rom_in(&dir, "Nope Game (USA).md"), None);
        assert!(commercial_rom("Nope Game (USA).md").is_none());
    }

    #[test]
    fn find_commercial_rom_by_substring() {
        let dir = TempDir::new("rom-find");
        let sonic = "Sonic The Hedgehog (USA, Europe).md";
        write_stub(&dir.join(sonic), b"stub");
        write_stub(&dir.join("Other.md"), b"stub");
        assert_eq!(
            find_commercial_rom_in(&dir, "Sonic The Hedgehog (USA"),
            Some(dir.join(sonic))
        );
    }

    #[test]
    fn find_commercial_rom_returns_first_in_sorted_order() {
        let dir = TempDir::new("rom-find-sorted");
        write_stub(&dir.join("Altered Beast (USA).md"), b"stub");
        write_stub(&dir.join("Alien Storm (USA).md"), b"stub");
        write_stub(&dir.join("Other.md"), b"stub");
        let found = find_commercial_rom_in(&dir, "Al");
        assert_eq!(found, Some(dir.join("Alien Storm (USA).md")));
    }

    #[test]
    fn find_commercial_rom_ignores_directories() {
        let dir = TempDir::new("rom-find-dir");
        fs::create_dir_all(dir.join("Sonic subdir")).expect("create dir");
        assert_eq!(find_commercial_rom_in(&dir, "Sonic"), None);
    }

    #[test]
    fn find_commercial_rom_missing_returns_none() {
        let dir = TempDir::new("rom-find-missing");
        write_stub(&dir.join("Other.md"), b"stub");
        assert_eq!(find_commercial_rom_in(&dir, "Sonic"), None);
    }

    /// Records what `require_data!` handed back. `take` returns `()` because
    /// that is what the macro's bare `return;` requires, which is exactly how
    /// a `#[test] fn` uses it.
    #[derive(Default)]
    struct Probe {
        got: Option<PathBuf>,
    }

    impl Probe {
        fn take(&mut self, opt: Option<PathBuf>) {
            self.got = Some(require_data!(opt));
        }
    }

    #[test]
    fn require_data_yields_value_when_present() {
        let dir = TempDir::new("require-present");
        write_stub(&dir.join("a.bin"), b"stub");
        let mut probe = Probe::default();
        probe.take(test_rom_in(&dir, "a.bin"));
        assert_eq!(probe.got, Some(dir.join("a.bin")));
    }

    #[test]
    fn require_data_returns_early_when_missing() {
        let mut probe = Probe::default();
        probe.take(None);
        assert_eq!(probe.got, None);
    }
}
