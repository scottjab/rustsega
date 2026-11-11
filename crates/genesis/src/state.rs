//! Save states: the type the API returns before the format exists.
//!
//! docs/ARCHITECTURE.md gives `Genesis` a `save_state`/`load_state` pair, and a
//! front end has to be able to call them - so they exist, they are honest about
//! doing nothing, and this is the error they answer with.
//! TODO(M6-07 #127): the versioned state format and the `serde` state of every
//! chip in it, which is what replaces this with something that can fail for a
//! interesting reason.

/// Why a save state could not be made or read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateError {
    /// This build has no save-state format at all.
    ///
    /// The variant every `Genesis::save_state` and `Genesis::load_state` call
    /// returns today, and the one a caller can meet until M6-07 (#127) ships
    /// the format; a truncated or foreign blob from a build that *does* have
    /// one needs its own variant, not this one.
    Unsupported,
}

impl std::fmt::Display for StateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported => write!(f, "save states are not implemented yet (M6-07 #127)"),
        }
    }
}

impl std::error::Error for StateError {}
