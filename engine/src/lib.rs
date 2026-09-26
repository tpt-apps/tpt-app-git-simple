//! TPT Git Simple engine — a small, readable git implementation covering
//! exactly what the app needs: browse (refs, log, trees, blobs, diffs),
//! save (plain-language staging → blob/tree/commit writing), get-latest
//! (3-way merge with plain-English conflict guidance), and safe undo
//! (reflog + backup refs).
//!
//! ## Why not libgit2 / tpt-gitcastle's code
//!
//! `tpt-gitcastle` is a TypeScript/Electron app, so its code can't be
//! wrapped in a Rust/WASM crate, and libgit2 doesn't build for
//! `wasm32-unknown-unknown`. This engine is pure Rust with no host
//! dependencies, written against an abstract [`fs::GitFs`] filesystem so:
//!
//! - **native** (`std`-backed [`fs::StdFs`]): host-unit-testable against
//!   real repos created with the git CLI, and usable directly by the Pro
//!   desktop exe;
//! - **wasm32** (browser app feeds a [`fs::MemoryFs`] snapshot): powers the
//!   free in-browser repo browser.
//!
//! ## Scope
//!
//! - Reads: refs (loose + `packed-refs`), HEAD, loose and packed objects
//!   (idx v2, ofs/ref deltas), commit log, recursive tree listing, blob
//!   content, line diffs (Myers with prefix/suffix trimming and a work
//!   budget), worktree-vs-HEAD status.
//! - `pro`: object writing, staged commits, branch/HEAD ref updates with
//!   reflog, backup refs for destructive actions, undo-last-save, 3-way
//!   merge (blob-level, diff3-style) with conflict-marker state and
//!   plain-English guidance.
//! - Explicitly out of scope: the index file (staging is explicit file
//!   lists), `.gitignore` parsing, commit-graph files, grafted/shallow
//!   history, remote network protocols (the Pro desktop shell delegates
//!   fetch/push to the git CLI), submodules beyond tree entries.

pub mod diff;
pub mod error;
pub mod fs;
pub mod objects;
pub mod oid;
pub mod pack;
pub mod repo;
pub mod status;
pub mod zlib;

#[cfg(feature = "pro")]
pub mod commit;
#[cfg(feature = "pro")]
pub mod conflict;
#[cfg(feature = "pro")]
pub mod merge;
#[cfg(feature = "pro")]
pub mod undo;

pub use error::GitError;
pub use oid::Oid;
pub use repo::Repo;

/// The engine version, surfaced in the UI footer.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
