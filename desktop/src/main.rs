//! TPT Git Simple Pro — desktop shell (paid edition).
//!
//! A native host binary that opens the OS webview (WebView2 on Windows) and
//! serves the `trunk build --release` output of the appfront DOM bundle from
//! `dist/`. The same DOM bundle ships to the hub for the free WASM edition;
//! the Pro difference at runtime is that the bundle is built with
//! `--features pro` and the hosted page talks to this process over IPC:
//!
//! - `dialog.open` (framework built-in): the native folder picker — the one
//!   place a real repo path enters the app;
//! - `git.*` ([`git_host`]): snapshot/status/save/undo/discard/restore/pull/
//!   push, all executed natively with the engine + the git CLI, results
//!   delivered back to the page via the value-reply IPC bridge.
//!
//! `dist/` is embedded into the exe at compile time (see [`DIST`]), so the
//! shipped file is a single exe with no companion folder. Resolve order for
//! the bundle directory: `TPT_GIT_SIMPLE_DIST` env var, `./dist`, `dist`
//! next to the exe, then the embedded copy extracted to the OS temp dir.

use std::path::PathBuf;

use include_dir::{include_dir, Dir};
use serde_json::json;
use tpt_appfront_webview::{Acl, AppBuilder, Capability, ParamKind, ParamSpec, WindowConfig};

mod git_host;

const APP_ID: &str = "nz.co.tptsolutions.git-simple-pro";

/// The `trunk build --release` output, embedded at compile time. `build.ps1
/// -Pro` runs the trunk build before this crate, so `../dist` (relative to
/// this crate's `Cargo.toml`) always exists by the time this macro expands.
static DIST: Dir = include_dir!("$CARGO_MANIFEST_DIR/../dist");

fn main() {
    let dist_dir = resolve_dist_dir();
    if !dist_dir.join("index.html").exists() {
        eprintln!(
            "tpt-git-simple-pro: no index.html under {} and embedded-asset extraction failed — \
             this build is missing its web bundle",
            dist_dir.display()
        );
        std::process::exit(1);
    }

    let any_string = |name: &str, required: bool| ParamSpec {
        name: name.to_string(),
        required,
        kind: ParamKind::Any,
        default: None,
    };

    // Every page-reachable action, pinned to its parameter contract. File
    // contents are base64 strings; object payloads are Any.
    let git_cap = |action: &str, params: Vec<ParamSpec>| Capability {
        action: action.to_string(),
        params,
    };
    let acl = Acl {
        capabilities: vec![
            Capability {
                action: "dialog.open".to_string(),
                params: vec![any_string("title", false), any_string("dir", false)],
            },
            Capability {
                action: "open_external".to_string(),
                params: vec![ParamSpec {
                    name: "url".to_string(),
                    required: true,
                    kind: ParamKind::String,
                    default: None,
                }],
            },
            git_cap("git.snapshot", vec![any_string("path", true)]),
            git_cap("git.status", vec![any_string("path", true)]),
            git_cap(
                "git.save",
                vec![
                    any_string("path", true),
                    any_string("message", true),
                    any_string("name", true),
                    any_string("email", true),
                    any_string("files", true),
                    any_string("mergeParent", false),
                ],
            ),
            git_cap(
                "git.undo",
                vec![any_string("path", true), any_string("name", true), any_string("email", true)],
            ),
            git_cap(
                "git.discard",
                vec![
                    any_string("path", true),
                    any_string("name", true),
                    any_string("email", true),
                    any_string("files", true),
                ],
            ),
            git_cap("git.backups", vec![any_string("path", true)]),
            git_cap(
                "git.restore",
                vec![
                    any_string("path", true),
                    any_string("name", true),
                    any_string("email", true),
                    any_string("backupRef", true),
                ],
            ),
            git_cap(
                "git.pull",
                vec![any_string("path", true), any_string("name", true), any_string("email", true)],
            ),
            git_cap("git.push", vec![any_string("path", true)]),
        ],
    };

    let result = AppBuilder::new(APP_ID)
        .with_window(WindowConfig {
            id: "main".to_string(),
            title: "TPT Git Simple Pro".to_string(),
            width: 1280,
            height: 900,
            dist_dir,
        })
        .with_single_instance(true)
        .with_acl(acl)
        .run_with_value_handler(|action, params| {
            if action == "open_external" {
                let url = params.get("url").and_then(|v| v.as_str()).unwrap_or("");
                // Belt-and-braces on top of the ACL's string-type check: only
                // ever hand the OS shell a tptsolutions.co.nz URL, since this
                // is reachable from anything the hosted page's JS can run.
                if url.starts_with("https://tptsolutions.co.nz") {
                    let _ = open::that(url);
                }
                return Ok(json!({ "ok": true }));
            }
            // Errors carry plain-English text the UI shows as-is.
            git_host::handle(action, &params)
        });

    if let Err(error) = result {
        eprintln!("tpt-git-simple-pro: {error:#}");
        std::process::exit(1);
    }
}

/// Helper so the unmatched-action error keeps the full name (the `rest`
/// strip above only exists to make the intent explicit).
fn resolve_dist_dir() -> PathBuf {
    if let Ok(from_env) = std::env::var("TPT_GIT_SIMPLE_DIST") {
        return PathBuf::from(from_env);
    }
    let cwd_dist = PathBuf::from("dist");
    if cwd_dist.join("index.html").exists() {
        return cwd_dist;
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            let exe_dist = exe_dir.join("dist");
            if exe_dist.join("index.html").exists() {
                return exe_dist;
            }
        }
    }
    extract_embedded_dist()
}

/// Writes the embedded [`DIST`] contents to a stable temp directory (once —
/// subsequent launches reuse the extracted copy) and returns its path. This
/// is the path a customer's real double-click takes: no `dist/` folder
/// shipped or found, so the exe unpacks its own.
fn extract_embedded_dist() -> PathBuf {
    let out_dir = std::env::temp_dir().join(format!("{APP_ID}-dist"));
    if !out_dir.join("index.html").exists() {
        let _ = std::fs::create_dir_all(&out_dir);
        if let Err(error) = DIST.extract(&out_dir) {
            eprintln!("tpt-git-simple-pro: failed to extract embedded assets: {error:#}");
        }
    }
    out_dir
}
