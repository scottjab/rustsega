//! Shared test helpers: ROM discovery, frame hashing, golden files.

#[cfg(test)]
mod tests {
    /// Smoke test so `cargo nextest run --workspace` has something to run
    /// before any real code lands (ticket M0-00).
    #[test]
    fn workspace_builds() {
        assert_eq!(1 + 1, 2);
    }
}
