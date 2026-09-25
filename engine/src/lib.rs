//! TPT Git Simple engine — Phase 0 placeholder.
//!
//! Phase 1 replaces this with the real adapter: refs/HEAD discovery, loose +
//! packed object reading, commit-graph log, tree listing, line diffs, and
//! (pro) object writing with reflog-backed safe undo — all over a `GitFs`
//! abstraction with no UI or host dependencies.

#![allow(dead_code)]

/// Phase 0 smoke symbol so an empty crate is never shipped.
pub const ENGINE_PLACEHOLDER: &str = "tpt-git-engine";
