//! Repo sources for the in-browser edition.
//!
//! Phase 0 stub — the two real sources land with the repo-browser UI:
//! - `DirectorySource`: the user picks a local folder via the File System
//!   Access API; a small JS walker (evaluated once, awaited as a
//!   `JsFuture`) snapshots directory listings + file bytes into an
//!   in-memory `MemoryFs` the engine can read.
//! - `DemoSource`: an embedded sample repo (objects serialized into the
//!   binary at compile time) so the browser UI works in browsers without
//!   the File System Access API.

#![allow(dead_code)]
