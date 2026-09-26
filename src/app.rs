//! Free WASM edition UI (tpt-appfront-dom), view-only repo browser.
//!
//! Rendering architecture (the fea-lite/cutlist-optimizer proven split):
//! - the **shell is mounted once** (header, sidebar, footer);
//! - the **main column re-renders** via `MountedRoot::render` from an
//!   effect watching the load/tab/selection signals — every state change
//!   rebuilds one declarative `UITree`;
//! - the engine reads through a `MemoryFs` snapshot: the directory walk
//!   happens once, on open, then everything (log, trees, blobs, diffs) is
//!   synchronous and local.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;

use tpt_appfront_core::{create_effect, ContainerBuilder, Signal, UITree};
use tpt_git_engine::diff::{diff_blobs, FileDiff};
use tpt_git_engine::fs::MemoryFs;
use tpt_git_engine::repo::{LogEntry, Repo};
use tpt_git_engine::Oid;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

use crate::repo_source::{self, Snapshot};

#[derive(Debug, Clone)]
enum Msg {
    /// Open the OS directory picker (File System Access API).
    OpenRepo,
    /// Load the embedded demo repo.
    LoadDemo,
    /// Switch the content tab.
    Tab(&'static str),
    /// Select a commit in history (same click again = deselect).
    SelectCommit(String),
    /// Collapse/expand a directory in the file tree.
    ToggleDir(String),
    /// Open a file (from tree or diff list); same click = close.
    OpenFile(String),
    /// Open a tptsolutions.co.nz page in a new tab.
    VisitSite(String),
}

#[derive(Clone)]
enum Load {
    Idle,
    Loading,
    Failed(String),
    Ready(Rc<Loaded>),
}

struct Loaded {
    name: String,
    total_bytes: usize,
    skipped: Vec<String>,
    truncated: bool,
    branch: Option<String>,
    head: Option<Oid>,
    /// Newest first.
    log: Vec<LogEntry>,
    repo: Rc<RefCell<Repo<MemoryFs>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileSummary {
    path: String,
    added: usize,
    removed: usize,
    binary: bool,
}

const TAB_HISTORY: &str = "history";
const TAB_FILES: &str = "files";
const SITE_URL: &str = "https://tptsolutions.co.nz/tools/git-simple";

const BASE_CSS: &str = r#"
.gsr-app{--gsr-bg:#f7f8fa;--gsr-surface:#ffffff;--gsr-panel:#f2f4f7;--gsr-fg:#181d27;--gsr-muted:#667085;--gsr-border:#e3e7ee;--gsr-accent:#0e7c6b;--gsr-accent-2:#0a5f52;--gsr-accent-soft:#e6f5f2;--gsr-accent-fg:#ffffff;--gsr-err:#b3261e;--gsr-err-bg:#fbeeec;--gsr-add:#0a7d43;--gsr-add-bg:#e8f6ee;--gsr-del:#b3261e;--gsr-del-bg:#fbeeec;--gsr-shadow:0 1px 2px rgba(16,24,40,.04),0 1px 3px rgba(16,24,40,.06);max-width:1180px;margin:0 auto;color:var(--gsr-fg);font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,Helvetica,Arial,sans-serif;font-size:14.5px;line-height:1.55;padding:0 4px 32px}
@media(prefers-color-scheme:dark){.gsr-app{--gsr-bg:#0b0f16;--gsr-surface:#141a24;--gsr-panel:#161d2a;--gsr-fg:#e8ebf0;--gsr-muted:#98a2b3;--gsr-border:#262f3d;--gsr-accent:#34b39a;--gsr-accent-2:#4fd6ba;--gsr-accent-soft:#122824;--gsr-accent-fg:#04140f;--gsr-err:#f2b8b5;--gsr-err-bg:#2c1a1c;--gsr-add:#5ad195;--gsr-add-bg:#12291c;--gsr-del:#f2b8b5;--gsr-del-bg:#2c1a1c;--gsr-shadow:0 1px 2px rgba(0,0,0,.3)}}
.gsr-app *{box-sizing:border-box}
.gsr-header{display:flex;align-items:flex-start;gap:14px;padding:22px 2px 18px;border-bottom:1px solid var(--gsr-border);margin-bottom:20px}
.gsr-brand{flex:0 0 auto;width:42px;height:42px;border-radius:11px;background:linear-gradient(135deg,var(--gsr-accent),var(--gsr-accent-2));display:flex;align-items:center;justify-content:center;color:var(--gsr-accent-fg);font-weight:800;font-size:.82rem}
.gsr-title-group{flex:1;min-width:0}
.gsr-title-row{display:flex;align-items:center;gap:10px;flex-wrap:wrap}
.gsr-chip{display:inline-flex;align-items:center;padding:3px 10px;border-radius:999px;font-size:.66rem;font-weight:800;letter-spacing:.05em;text-transform:uppercase;background:var(--gsr-accent-soft);color:var(--gsr-accent)}
.gsr-sub{color:var(--gsr-muted);margin:5px 0 0;font-size:.88rem}
.gsr-shell{display:grid;grid-template-columns:minmax(280px,340px) 1fr;gap:20px;align-items:start}
@media(max-width:900px){.gsr-shell{grid-template-columns:1fr}}
.gsr-sidebar{display:flex;flex-direction:column;gap:14px;position:sticky;top:14px}
@media(max-width:900px){.gsr-sidebar{position:static}}
.gsr-main{min-width:0;display:flex;flex-direction:column;gap:14px}
.gsr-card{background:var(--gsr-surface);border:1px solid var(--gsr-border);border-radius:14px;padding:15px 17px;box-shadow:var(--gsr-shadow)}
.gsr-card h3{margin:0 0 8px;font-size:1rem;font-weight:700}
.gsr-hint{color:var(--gsr-muted);font-size:.82rem;margin:0 0 10px}
.gsr-btn{padding:9px 16px;border-radius:9px;border:1px solid var(--gsr-border);background:var(--gsr-surface);color:var(--gsr-fg);font:inherit;font-weight:600;cursor:pointer;transition:border-color .12s,color .12s}
.gsr-btn:hover{border-color:var(--gsr-accent);color:var(--gsr-accent)}
.gsr-primary{background:linear-gradient(135deg,var(--gsr-accent),var(--gsr-accent-2));border-color:transparent;color:var(--gsr-accent-fg)}
.gsr-primary:hover{color:var(--gsr-accent-fg);filter:brightness(1.07)}
.gsr-actions{display:flex;gap:8px;flex-wrap:wrap}
.gsr-actions .gsr-btn{flex:1 1 auto}
.gsr-meta{display:grid;grid-template-columns:auto 1fr;gap:4px 12px;font-size:.86rem;margin:0}
.gsr-meta .k{color:var(--gsr-muted)}
.gsr-meta .v{margin:0;font-weight:600;word-break:break-all}
.gsr-upsell{border-radius:14px;padding:15px 17px;background:linear-gradient(135deg,var(--gsr-accent-soft),transparent);border:1px dashed var(--gsr-accent);font-size:.88rem}
.gsr-upsell h3{margin:0 0 6px}
.gsr-upsell ul{margin:0 0 10px;padding-left:18px}
.gsr-upsell li{margin:2px 0}
.gsr-linkish{color:var(--gsr-accent);font-weight:700;text-decoration:none;cursor:pointer}
.gsr-linkish:hover{text-decoration:underline}
.gsr-tabs{display:flex;gap:6px}
.gsr-tab{padding:8px 16px;border-radius:9px 9px 0 0;border:1px solid transparent;border-bottom:none;background:transparent;color:var(--gsr-muted);font:inherit;font-weight:700;cursor:pointer}
.gsr-tab:hover{color:var(--gsr-fg)}
.gsr-tab.on{background:var(--gsr-surface);border-color:var(--gsr-border);color:var(--gsr-fg)}
.gsr-empty{padding:34px 20px;text-align:center;color:var(--gsr-muted)}
.gsr-empty .gsr-actions{max-width:380px;margin:14px auto 0}
.gsr-error{margin:0;padding:10px 12px;background:var(--gsr-err-bg);border:1px solid var(--gsr-err);border-radius:10px;color:var(--gsr-err);font-size:.88rem}
.gsr-commit{display:flex;flex-direction:column;gap:2px;padding:9px 12px;border-radius:10px;cursor:pointer;border:1px solid transparent;border:none;background:none;color:var(--gsr-fg);font:inherit;width:100%;text-align:left}
.gsr-commit:hover{background:var(--gsr-panel)}
.gsr-commit.on{background:var(--gsr-accent-soft);border-color:var(--gsr-accent)}
.gsr-commit-top{display:flex;align-items:baseline;gap:10px;flex-wrap:wrap}
.gsr-commit-msg{font-weight:600;flex:1;min-width:200px}
.gsr-commit-oid{font-family:ui-monospace,SFMono-Regular,Menlo,monospace;font-size:.76rem;color:var(--gsr-muted)}
.gsr-commit-meta{font-size:.78rem;color:var(--gsr-muted)}
.gsr-filelist{display:flex;flex-direction:column;gap:2px;margin-top:8px}
.gsr-file{display:flex;gap:10px;align-items:baseline;padding:6px 10px;border-radius:8px;cursor:pointer;font-size:.86rem;border:1px solid transparent;border:none;background:none;color:var(--gsr-fg);font:inherit;width:100%;text-align:left}
.gsr-file:hover{background:var(--gsr-panel)}
.gsr-file.on{background:var(--gsr-accent-soft);border-color:var(--gsr-accent)}
.gsr-file-path{flex:1;min-width:0;word-break:break-all;font-family:ui-monospace,SFMono-Regular,Menlo,monospace;font-size:.8rem}
.gsr-delta-add{color:var(--gsr-add);font-weight:700;font-variant-numeric:tabular-nums}
.gsr-delta-del{color:var(--gsr-del);font-weight:700;font-variant-numeric:tabular-nums}
.gsr-delta-none{color:var(--gsr-muted)}
.gsr-tree{display:flex;flex-direction:column;font-family:ui-monospace,SFMono-Regular,Menlo,monospace;font-size:.82rem}
.gsr-tree-row{display:flex;align-items:center;gap:8px;padding:4px 8px;border-radius:6px;cursor:pointer;border:none;background:none;color:var(--gsr-fg);font:inherit;font-family:inherit;font-size:inherit;text-align:left}
.gsr-tree-row:hover{background:var(--gsr-panel)}
.gsr-tree-dir{color:var(--gsr-accent);font-weight:700}
.gsr-viewer{margin-top:10px;border-radius:10px;overflow:hidden;border:1px solid var(--gsr-border)}
.gsr-viewer-head{display:flex;align-items:center;justify-content:space-between;gap:10px;padding:8px 12px;background:var(--gsr-panel);border-bottom:1px solid var(--gsr-border);font-size:.8rem;color:var(--gsr-muted);word-break:break-all}
.gsr-close{flex:0 0 auto;padding:2px 10px;font-size:.74rem;border-radius:999px;border:1px solid var(--gsr-border);background:transparent;color:var(--gsr-muted);cursor:pointer}
.gsr-close:hover{color:var(--gsr-err);border-color:var(--gsr-err)}
.gsr-scroll{max-height:480px;overflow:auto}
.gsr-diff{width:100%;border-collapse:collapse;font-family:ui-monospace,SFMono-Regular,Menlo,monospace;font-size:.78rem}
.gsr-diff .row{padding:1px 8px;white-space:pre-wrap;word-break:break-all}
.gsr-diff .add{background:var(--gsr-add-bg);color:var(--gsr-add)}
.gsr-diff .del{background:var(--gsr-del-bg);color:var(--gsr-del)}
.gsr-diff .ctx{color:var(--gsr-muted)}
.gsr-diff .hunk{background:var(--gsr-panel);color:var(--gsr-accent);font-weight:700;padding:4px 8px}
.gsr-text{display:block;padding:10px 12px;font-family:ui-monospace,SFMono-Regular,Menlo,monospace;font-size:.8rem;white-space:pre-wrap;word-break:break-all;margin:0;max-height:420px;overflow:auto}
.gsr-foot{margin-top:26px;padding-top:14px;border-top:1px solid var(--gsr-border);color:var(--gsr-muted);font-size:.8rem;display:flex;gap:4px 16px;flex-wrap:wrap;align-items:center}
"#;

/// Mounts the whole free-edition app into `container` (the hub's container
/// div, or the standalone `#tpt-appfront-root` marker).
pub fn mount_app(container: &web_sys::Element) -> Result<(), JsValue> {
    let document = web_sys::window()
        .expect("no window")
        .document()
        .expect("no document");

    upsert_style(&document, "git-simple-styles", BASE_CSS);
    // `.gsr-app` on the mount root itself: every theme custom property
    // lives here, so the CSS above depends on this class.
    let existing_class = container.get_attribute("class").unwrap_or_default();
    if !existing_class.split_whitespace().any(|c| c == "gsr-app") {
        let combined = if existing_class.is_empty() {
            "gsr-app".to_string()
        } else {
            format!("{existing_class} gsr-app")
        };
        let _ = container.set_attribute("class", &combined);
    }

    let load: Signal<Load> = Signal::new(Load::Idle);
    let tab: Signal<&'static str> = Signal::new(TAB_HISTORY);
    let selected_commit: Signal<Option<String>> = Signal::new(None);
    let selected_file: Signal<Option<String>> = Signal::new(None);
    let collapsed: Signal<BTreeSet<String>> = Signal::new(BTreeSet::new());

    let dispatch: Rc<dyn Fn(Msg)> = {
        let load = load.clone();
        let tab = tab.clone();
        let selected_commit = selected_commit.clone();
        let selected_file = selected_file.clone();
        let collapsed = collapsed.clone();
        Rc::new(move |msg| match msg {
            Msg::OpenRepo => {
                let load = load.clone();
                load.set(Load::Loading);
                wasm_bindgen_futures::spawn_local(async move {
                    match repo_source::open_directory_snapshot().await {
                        Ok(snapshot) => load.set(load_snapshot(snapshot)),
                        Err(e) => {
                            if is_user_abort(&e) {
                                load.set(Load::Idle);
                            } else {
                                load.set(Load::Failed(js_error_text(&e)));
                            }
                        }
                    }
                });
            }
            Msg::LoadDemo => match repo_source::demo_snapshot() {
                Ok(snapshot) => load.set(load_snapshot(snapshot)),
                Err(e) => load.set(Load::Failed(js_error_text(&e))),
            },
            Msg::Tab(t) => {
                tab.set(t);
                selected_file.set(None);
            }
            Msg::SelectCommit(oid) => {
                // Same-commit click deselects; switching commits closes the
                // open file (it belongs to the old diff).
                let current = selected_commit.get();
                selected_commit.set(if current.as_deref() == Some(oid.as_str()) {
                    None
                } else {
                    Some(oid)
                });
                selected_file.set(None);
            }
            Msg::ToggleDir(dir) => {
                let mut set = collapsed.get();
                if set.contains(&dir) {
                    set.remove(&dir);
                } else {
                    set.insert(dir);
                }
                collapsed.set(set);
            }
            Msg::OpenFile(path) => {
                let current = selected_file.get();
                selected_file.set(if current.as_deref() == Some(path.as_str()) {
                    None
                } else {
                    Some(path)
                });
            }
            Msg::VisitSite(url) => visit_site(&url),
        })
    };

    // The static shell, mounted once.
    let ui: UITree<Msg> = shell_tree();
    let mut shell = tpt_appfront_dom::mount(container, &ui, dispatch.clone())?;

    // The dynamic main column lives inside the shell's grid; find it by
    // its class and mount the re-rendering content root there.
    let main_el = container
        .query_selector(".gsr-main")
        .ok()
        .flatten()
        .ok_or_else(|| JsValue::from_str("shell missing .gsr-main"))?;
    let footer: UITree<Msg> = footer_tree();
    let mut footer_root = tpt_appfront_dom::mount(container, &footer, dispatch.clone())?;

    let content_root: Rc<RefCell<Option<tpt_appfront_dom::MountedRoot<Msg>>>> =
        Rc::new(RefCell::new(None));

    {
        let main_el = main_el.clone();
        let content_root = content_root.clone();
        let dispatch = dispatch.clone();
        let load = load.clone();
        let tab = tab.clone();
        let selected_commit = selected_commit.clone();
        let selected_file = selected_file.clone();
        let collapsed = collapsed.clone();
        let handle = create_effect(move || {
            let tree = content_ui(
                &load.get(),
                tab.get(),
                selected_commit.get(),
                selected_file.get(),
                collapsed.get(),
            );
            // The content section changes *shape* between states (idle card
            // vs facts + tabs + lists), which in-place reconciliation isn't
            // for — so each state change unmounts the old section and
            // mounts a fresh tree. Lists here are small (<= 60 rows), so a
            // full rebuild is cheap and always correct.
            if let Some(old) = content_root.borrow_mut().take() {
                old.unmount();
            }
            let mounted = tpt_appfront_dom::mount(&main_el, &tree, dispatch.clone());
            if let Ok(root) = mounted {
                *content_root.borrow_mut() = Some(root);
            }
        });
        // The app lives as long as the page; the handle is dropped by the
        // page teardown, not by us.
        std::mem::forget(handle);
    }
    let _ = &mut shell;
    let _ = &mut footer_root;
    Ok(())
}

/// Turns a raw snapshot into engine state. Errors here mean "not a git
/// repository", which the UI shows as a friendly message.
fn load_snapshot(snapshot: Snapshot) -> Load {
    let mut fs = MemoryFs::new();
    for (path, bytes) in &snapshot.files {
        fs.add(path, bytes.clone());
    }
    let mut repo = match Repo::open(fs) {
        Ok(repo) => repo,
        Err(e) => {
            return Load::Failed(format!(
                "\"{}\" doesn't look like a git repository ({e}). Pick the folder that contains the .git folder.",
                snapshot.name
            ))
        }
    };
    let head = match repo.head() {
        Ok(head) => head,
        Err(e) => return Load::Failed(format!("Couldn't read the repository: {e}")),
    };
    let log = match head.commit {
        Some(start) => repo.log_from(start, 500).unwrap_or_default(),
        None => Vec::new(),
    };
    let total_bytes = snapshot.total_bytes();
    Load::Ready(Rc::new(Loaded {
        name: snapshot.name,
        total_bytes,
        skipped: snapshot.skipped,
        truncated: snapshot.truncated,
        branch: head.branch,
        head: head.commit,
        log,
        repo: Rc::new(RefCell::new(repo)),
    }))
}

fn is_user_abort(e: &JsValue) -> bool {
    js_error_text(e).to_lowercase().contains("abort")
}

fn js_error_text(e: &JsValue) -> String {
    if let Some(err) = e.dyn_ref::<js_sys::Error>() {
        err.to_string().into()
    } else {
        format!("{e:?}")
    }
}

fn visit_site(url: &str) {
    let window = web_sys::window().expect("no window");
    let _ = window.open_with_url_and_target(url, "_blank");
}

/// The mounted-once shell: header, sidebar, main-column placeholder.
fn shell_tree() -> UITree<Msg> {
    UITree::container(|c| {
        c.container(|header| {
            header.text("GS").class("gsr-brand");
            header.container(|group| {
                group.container(|row| {
                    row.heading(2, "TPT Git Simple");
                    row.text("Free preview").class("gsr-chip");
                })
                .class("gsr-title-row");
                group.text("Browse any git repository — its files and history — without learning git. No install, nothing to sign up for.")
                    .class("gsr-sub");
            })
            .class("gsr-title-group");
        })
        .class("gsr-header");

        c.container(|shell| {
            shell.container(|sidebar| {
                sidebar.container(|actions| {
                    actions
                        .button("Open a repository")
                        .class("gsr-btn gsr-primary")
                        .on_click(Msg::OpenRepo);
                    actions.button("Try the demo").class("gsr-btn").on_click(Msg::LoadDemo);
                })
                .class("gsr-actions");

                #[cfg(feature = "pro")]
                pro_badge_card(sidebar);
                #[cfg(not(feature = "pro"))]
                upsell_card(sidebar);
            })
            .class("gsr-sidebar");
            shell.container(|_main| {}).class("gsr-main");
        })
        .class("gsr-shell");
    })
}

/// The dynamic main column: idle prompt, error, or the repo browser.
fn content_ui(
    load: &Load,
    tab: &'static str,
    selected_commit: Option<String>,
    selected_file: Option<String>,
    collapsed: BTreeSet<String>,
) -> UITree<Msg> {
    UITree::container(|c| match load {
        Load::Idle => {
            c.container(|card| {
                card.heading(3, "Start by opening a repository");
                card.text("Pick the folder of a project that uses git — you'll see its files, its history, and what changed at each step. Everything stays on your machine; the folder is only ever read.")
                    .class("gsr-hint");
                card.container(|actions| {
                    actions
                        .button("Open a repository")
                        .class("gsr-btn gsr-primary")
                        .on_click(Msg::OpenRepo);
                    actions
                        .button("Try the demo repository")
                        .class("gsr-btn")
                        .on_click(Msg::LoadDemo);
                })
                .class("gsr-actions");
            })
            .class("gsr-card gsr-empty");
        }
        Load::Loading => {
            c.container(|card| {
                card.heading(3, "Reading the repository…");
                card.text("Copying the folder into memory so nothing on your disk can be changed.")
                    .class("gsr-hint");
            })
            .class("gsr-card gsr-empty");
        }
        Load::Failed(message) => {
            c.container(|card| {
                card.heading(3, "Couldn't open that folder");
                card.container(|p| {
                    p.text(message.clone());
                })
                .class("gsr-error");
            })
            .class("gsr-card");
        }
        Load::Ready(data) => {
            repo_facts(c, data);
            c.container(|tabs| {
                tabs.button("History")
                    .class(if tab == TAB_HISTORY { "gsr-tab on" } else { "gsr-tab" })
                    .on_click(Msg::Tab(TAB_HISTORY));
                tabs.button("Files")
                    .class(if tab == TAB_FILES { "gsr-tab on" } else { "gsr-tab" })
                    .on_click(Msg::Tab(TAB_FILES));
            })
            .class("gsr-tabs");

            if tab == TAB_FILES {
                render_files(c, data, selected_file, collapsed);
            } else {
                render_history(c, data, selected_commit, selected_file);
            }
        }
    })
}

/// One-line repository summary card.
fn repo_facts(c: &mut ContainerBuilder<Msg>, data: &Rc<Loaded>) {
    c.container(|card| {
        card.heading(3, &data.name);
        card.container(|meta| {
            meta.container(|row| {
                row.text("Branch").class("k");
                row.text(data.branch.clone().unwrap_or_else(|| "(detached)".into())).class("v");
            })
            .class("gsr-meta");
            meta.container(|row| {
                row.text("Saved versions").class("k");
                row.text(data.log.len().to_string()).class("v");
            })
            .class("gsr-meta");
            meta.container(|row| {
                row.text("Loaded size").class("k");
                row.text(format_bytes(data.total_bytes)).class("v");
            })
            .class("gsr-meta");
        })
        .class("");
        if !data.skipped.is_empty() {
            card.text(format!(
                "{} large file{} not loaded: {}",
                data.skipped.len(),
                plural(data.skipped.len()),
                data.skipped.iter().take(3).cloned().collect::<Vec<_>>().join(", ")
            ))
            .class("gsr-hint");
        }
        if data.truncated {
            card.text("The folder is very large — only part of it was loaded.")
                .class("gsr-hint");
        }
    })
    .class("gsr-card");
}

/// History tab: commit list; a selected commit shows its changed files and,
/// when a file is chosen, the line-level diff.
fn render_history(
    c: &mut ContainerBuilder<Msg>,
    data: &Rc<Loaded>,
    selected_commit: Option<String>,
    selected_file: Option<String>,
) {
    if data.log.is_empty() {
        c.container(|card| {
            card.heading(3, "No saved versions yet");
            card.text("This repository has no commits — once something is saved, every version will appear here.")
                .class("gsr-hint");
        })
        .class("gsr-card");
        return;
    }

    c.container(|card| {
        card.heading(3, "History");
        card.text(format!(
            "{} saved versions, newest first. Select one to see what changed.",
            data.log.len()
        ))
        .class("gsr-hint");
        for entry in data.log.iter().take(60) {
            let hex = entry.oid.to_hex();
            let on = selected_commit.as_deref() == Some(hex.as_str());
            card.container(|row| {
                row.container(|top| {
                    top.text(first_line(&entry.message)).class("gsr-commit-msg");
                    top.text(entry.oid.short()).class("gsr-commit-oid");
                })
                .class("gsr-commit-top");
                row.text(format!("{} · {}", entry.author_name, format_time(entry.time)))
                    .class("gsr-commit-meta");
            })
            .class(if on { "gsr-commit on" } else { "gsr-commit" })
            .on_click(Msg::SelectCommit(hex));
        }
        if data.log.len() > 60 {
            card.text(format!("… and {} older versions.", data.log.len() - 60))
                .class("gsr-hint");
        }
    })
    .class("gsr-card");

    if let Some(hex) = selected_commit {
        if let Some(oid) = Oid::from_hex(&hex) {
            render_commit_diff(c, data, oid, selected_file);
        }
    }
}

/// The changed-file summary + diff viewer for one selected commit.
fn render_commit_diff(
    c: &mut ContainerBuilder<Msg>,
    data: &Rc<Loaded>,
    commit: Oid,
    selected_file: Option<String>,
) {
    let Some(summary) = commit_summary(data, commit) else {
        return;
    };

    c.container(|card| {
        let entry = data.log.iter().find(|e| e.oid == commit);
        card.heading(
            3,
            entry
                .map(|e| first_line(&e.message))
                .unwrap_or_else(|| "Selected version".to_string())
                .as_str(),
        );
        if summary.is_empty() {
            card.text("This saved version has no file changes (it may be a merge).")
                .class("gsr-hint");
        } else {
            let (added, removed): (usize, usize) = summary
                .iter()
                .map(|s| (s.added, s.removed))
                .fold((0, 0), |(a, b), (x, y)| (a + x, b + y));
            card.text(format!(
                "{} file{} changed — {added} line{} added, {removed} removed.",
                summary.len(),
                plural(summary.len()),
                plural(added),
            ))
            .class("gsr-hint");
            card.container(|list| {
                for s in summary.iter() {
                    let on = selected_file.as_deref() == Some(s.path.as_str());
                    list.container(|row| {
                        row.text(s.path.clone()).class("gsr-file-path");
                        if s.binary {
                            row.text("changed").class("gsr-delta-none");
                        } else {
                            row.text(format!("+{}", s.added)).class("gsr-delta-add");
                            row.text(format!("\u{2212}{}", s.removed)).class("gsr-delta-del");
                        }
                    })
                    .class(if on { "gsr-file on" } else { "gsr-file" })
                    .on_click(Msg::OpenFile(s.path.clone()));
                }
            })
            .class("gsr-filelist");
        }
    })
    .class("gsr-card");

    if let Some(path) = selected_file {
        if let Some(diff) = file_diff(data, commit, &path) {
            c.container(|card| {
                card.container(|head| {
                    head.text(path.clone()).class("gsr-commit-msg");
                    head.button("Close").class("gsr-close").on_click(Msg::OpenFile(String::new()));
                })
                .class("gsr-viewer-head");
                if diff.binary {
                    card.text("This file changed, but it isn't text (an image or other binary file), so there's no line-by-line view.")
                        .class("gsr-hint");
                } else if diff.hunks.is_empty() {
                    card.text("No content changes.").class("gsr-hint");
                } else {
                    card.container(|table| {
                        for hunk in &diff.hunks {
                            table
                                .container(|r| {
                                    r.text(format!("lines around {} / {}", hunk.old_start, hunk.new_start));
                                })
                                .class("hunk");
                            for (marker, line) in &hunk.lines {
                                let class = match marker {
                                    '+' => "add",
                                    '-' => "del",
                                    _ => "ctx",
                                };
                                table
                                    .container(|r| {
                                        r.text(format!("{marker} {line}"));
                                    })
                                    .class(class);
                            }
                        }
                    })
                    .class("gsr-diff gsr-scroll");
                }
            })
            .class("gsr-card");
        }
    }
}

/// Files tab: the repository tree at the latest saved version.
fn render_files(
    c: &mut ContainerBuilder<Msg>,
    data: &Rc<Loaded>,
    selected_file: Option<String>,
    collapsed: BTreeSet<String>,
) {
    let Some(oid) = data.head else {
        c.container(|card| {
            card.heading(3, "No files yet");
            card.text("This repository has no commits.").class("gsr-hint");
        })
        .class("gsr-card");
        return;
    };

    let files = {
        let mut repo = data.repo.borrow_mut();
        let tree = match repo.commit(&oid) {
            Ok(commit) => commit.tree,
            Err(_) => {
                c.container(|card| {
                    card.text("Couldn't read the repository.").class("gsr-error");
                })
                .class("gsr-card");
                return;
            }
        };
        repo.list_tree_recursive(&tree)
    };
    let files = match files {
        Ok(files) => files,
        Err(_) => {
            c.container(|card| {
                card.text("Couldn't read the file list.").class("gsr-error");
            })
            .class("gsr-card");
            return;
        }
    };

    c.container(|card| {
        card.heading(3, "Files");
        card.text(format!(
            "{} files in the latest saved version. Select a file to read it.",
            files.len()
        ))
        .class("gsr-hint");
        card.container(|tree| render_tree_level(tree, &files, "", 0, &collapsed))
            .class("gsr-tree");
    })
    .class("gsr-card");

    if let Some(path) = &selected_file {
        let bytes = data.repo.borrow_mut().read_file_at(oid, path);
        c.container(|card| {
            card.container(|head| {
                head.text(path.clone()).class("gsr-commit-msg");
                head.button("Close").class("gsr-close").on_click(Msg::OpenFile(String::new()));
            })
            .class("gsr-viewer-head");
            match bytes {
                Ok(bytes) => {
                    if bytes.contains(&0) {
                        card.text("Binary file (not text) — contents not shown.")
                            .class("gsr-hint");
                    } else {
                        let text = String::from_utf8_lossy(&bytes);
                        let preview: Vec<&str> = text.lines().take(400).collect();
                        card.container(|pre| {
                            pre.text(preview.join("\n"));
                        })
                        .class("gsr-text");
                    }
                }
                Err(_) => {
                    card.text("Couldn't read this file.").class("gsr-error");
                }
            }
        })
        .class("gsr-card");
    }
}

/// Flattened tree rendering; directories are open unless collapsed.
fn render_tree_level(
    b: &mut ContainerBuilder<Msg>,
    files: &[(String, String, Oid)],
    prefix: &str,
    depth: usize,
    collapsed: &BTreeSet<String>,
) {
    let mut dirs: Vec<String> = Vec::new();
    for (path, _, _) in files {
        let rest = path.strip_prefix(prefix).unwrap_or(path);
        if let Some(i) = rest.find('/') {
            let dir = format!("{}{}/", prefix, &rest[..i]);
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
    }
    dirs.sort();
    for dir in dirs {
        let is_open = !collapsed.contains(&dir);
        let label = dir
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or(&dir)
            .to_string();
        b.button(format!(
            "{}{} {}/",
            "  ".repeat(depth),
            if is_open { "▾" } else { "▸" },
            label
        ))
        .class("gsr-tree-row gsr-tree-dir")
        .on_click(Msg::ToggleDir(dir.clone()));
        if is_open {
            render_tree_level(b, files, &dir, depth + 1, collapsed);
        }
    }
    for (path, _mode, _oid) in files {
        let rest = path.strip_prefix(prefix).unwrap_or(path);
        if rest.contains('/') {
            continue;
        }
        b.button(format!("{}{}", "  ".repeat(depth + 1), rest))
            .class("gsr-tree-row")
            .on_click(Msg::OpenFile(path.clone()));
    }
}

/// Per-path (+/-) summary for a commit vs its first parent.
fn commit_summary(data: &Rc<Loaded>, commit: Oid) -> Option<Vec<FileSummary>> {
    let mut repo = data.repo.borrow_mut();
    let commit_obj = repo.commit(&commit).ok()?;
    let parent = commit_obj.parents.first().copied();
    let new_map = repo.tree_map(commit).ok()?;
    let old_map = match parent {
        Some(p) => repo.tree_map(p).ok()?,
        None => std::collections::HashMap::new(),
    };
    let mut paths: Vec<&String> = new_map.keys().chain(old_map.keys()).collect();
    paths.sort();
    paths.dedup();

    let mut out = Vec::new();
    for path in paths {
        let old = old_map.get(path).map(|(_, oid)| *oid);
        let new = new_map.get(path).map(|(_, oid)| *oid);
        if old == new {
            continue;
        }
        match (old, new) {
            (Some(old), Some(new)) => {
                let bytes_old = repo.blob(&old).unwrap_or_default();
                let bytes_new = repo.blob(&new).unwrap_or_default();
                if bytes_old.contains(&0) || bytes_new.contains(&0) {
                    out.push(FileSummary { path: path.clone(), added: 0, removed: 0, binary: true });
                } else {
                    let (added, removed, _) =
                        tpt_git_engine::diff::count_changes(&bytes_old, &bytes_new);
                    out.push(FileSummary { path: path.clone(), added, removed, binary: false });
                }
            }
            (None, Some(new)) => {
                let bytes = repo.blob(&new).unwrap_or_default();
                let added = if bytes.contains(&0) { 0 } else { line_count(&bytes) };
                out.push(FileSummary { path: path.clone(), added, removed: 0, binary: bytes.contains(&0) });
            }
            (Some(old), None) => {
                let bytes = repo.blob(&old).unwrap_or_default();
                let removed = if bytes.contains(&0) { 0 } else { line_count(&bytes) };
                out.push(FileSummary { path: path.clone(), added: 0, removed, binary: bytes.contains(&0) });
            }
            (None, None) => {}
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Some(out)
}

fn line_count(bytes: &[u8]) -> usize {
    String::from_utf8_lossy(bytes).lines().count()
}

/// Full diff of one file at a commit vs its parent.
fn file_diff(data: &Rc<Loaded>, commit: Oid, path: &str) -> Option<FileDiff> {
    let mut repo = data.repo.borrow_mut();
    let commit_obj = repo.commit(&commit).ok()?;
    let parent = commit_obj.parents.first().copied();
    let old = parent.and_then(|p| repo.read_file_at(p, path).ok());
    let new = repo.read_file_at(commit, path).ok();
    drop(repo);
    Some(diff_blobs(&old.unwrap_or_default(), &new.unwrap_or_default()))
}

/// (pro) Sidebar chip marking the desktop build.
#[cfg(feature = "pro")]
fn pro_badge_card(c: &mut ContainerBuilder<Msg>) {
    c.container(|card| {
        card.heading(3, "Desktop edition");
        card.text("All changes happen locally with guardrails: confirmations before anything destructive, a backup before any discard, and undo for every save.")
            .class("gsr-hint");
    })
    .class("gsr-upsell");
}

/// The free-edition upsell (sidebar).
fn upsell_card(c: &mut ContainerBuilder<Msg>) {
    c.container(|card| {
        card.heading(3, "Want to save changes too?");
        card.text("This free edition is view-only. The desktop edition can write, safely:")
            .class("gsr-hint");
        card.container(|ul| {
            ul.text("• Save your changes in plain language");
            ul.text("• Get the latest version with one click");
            ul.text("• Safe undo for every action");
            ul.text("• Guided help when versions clash");
            ul.text("• $15 once — yours to keep, no subscription");
        });
        card.button("See the desktop edition \u{2192}")
            .class("gsr-linkish")
            .on_click(Msg::VisitSite(SITE_URL.to_string()));
    })
    .class("gsr-upsell");
}

fn footer_tree() -> UITree<Msg> {
    UITree::container(|c| {
        c.container(|foot| {
            foot.text(format!(
                "TPT Git Simple {} — free view-only edition. Your folder is never modified.",
                tpt_git_engine::VERSION
            ));
            foot.button("Desktop edition")
                .class("gsr-linkish")
                .on_click(Msg::VisitSite(SITE_URL.to_string()));
        })
        .class("gsr-foot");
    })
}

fn first_line(message: &str) -> String {
    message.lines().next().unwrap_or("").to_string()
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn format_bytes(n: usize) -> String {
    if n >= 1024 * 1024 {
        format!("{:.1} MB", n as f64 / (1024.0 * 1024.0))
    } else if n >= 1024 {
        format!("{:.0} KB", n as f64 / 1024.0)
    } else {
        format!("{n} B")
    }
}

/// Short UTC timestamp for commit rows (engine times are Unix UTC).
fn format_time(unix: i64) -> String {
    let days = unix.div_euclid(86_400);
    let secs = unix.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        secs / 3600,
        (secs % 3600) / 60
    )
}

/// Howard Hinnant's days-from-civil inverse (UTC).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn upsert_style(document: &web_sys::Document, id: &str, css: &str) {
    if document.get_element_by_id(id).is_some() {
        return;
    }
    if let Ok(el) = document.create_element("style") {
        let _ = el.set_attribute("id", id);
        let _ = el.set_text_content(Some(css));
        if let Some(head) = document.head() {
            let _ = head.append_child(&el);
        }
    }
}
