//! Free WASM edition UI (tpt-appfront-dom).
//!
//! Phase 0 pipeline proof: a hello-world shell mounted through the hub
//! contract. The repo-browser UI (file tree, history, diffs) grows into this
//! file in later phases, following the fea-lite split: a static shell
//! mounted once, dynamic sections re-rendered via `MountedRoot::render`
//! driven by signals.

use std::rc::Rc;

use tpt_appfront_core::{Signal, UITree};
use wasm_bindgen::prelude::*;

#[derive(Debug, Clone)]
enum Msg {
    /// Open the OS directory picker (File System Access API) and load the
    /// repo snapshot. Wired up with the real loading flow in a later phase.
    OpenRepo,
    /// Load the embedded demo repo so the browser UI works even where the
    /// File System Access API is unavailable.
    LoadDemo,
}

const BASE_CSS: &str = r#"
.gsr-app{--gsr-bg:#f7f8fa;--gsr-surface:#ffffff;--gsr-fg:#181d27;--gsr-muted:#667085;--gsr-border:#e3e7ee;--gsr-accent:#0e7c6b;--gsr-accent-2:#0a5f52;--gsr-accent-soft:#e6f5f2;--gsr-accent-fg:#ffffff;max-width:1100px;margin:0 auto;color:var(--gsr-fg);font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,Helvetica,Arial,sans-serif;font-size:14.5px;line-height:1.55;padding:0 4px 32px}
.gsr-app *{box-sizing:border-box}
.gsr-header{display:flex;align-items:flex-start;gap:14px;padding:22px 2px 18px;border-bottom:1px solid var(--gsr-border);margin-bottom:22px}
.gsr-brand{flex:0 0 auto;width:42px;height:42px;border-radius:11px;background:linear-gradient(135deg,var(--gsr-accent),var(--gsr-accent-2));display:flex;align-items:center;justify-content:center;color:var(--gsr-accent-fg);font-weight:800;font-size:.82rem}
.gsr-title-row{display:flex;align-items:center;gap:10px;flex-wrap:wrap}
.gsr-chip{display:inline-flex;align-items:center;padding:3px 10px;border-radius:999px;font-size:.66rem;font-weight:800;letter-spacing:.05em;text-transform:uppercase;background:var(--gsr-accent-soft);color:var(--gsr-accent)}
.gsr-sub{color:var(--gsr-muted);margin:5px 0 0;font-size:.88rem}
.gsr-btn{padding:9px 16px;border-radius:9px;border:1px solid var(--gsr-border);background:var(--gsr-surface);color:var(--gsr-fg);font:inherit;font-weight:600;cursor:pointer}
.gsr-primary{background:linear-gradient(135deg,var(--gsr-accent),var(--gsr-accent-2));border-color:transparent;color:var(--gsr-accent-fg)}
"#;

/// Mounts the whole free-edition app into `container` (the hub's container
/// div, or the standalone `#tpt-appfront-root` marker).
pub fn mount_app(container: &web_sys::Element) -> Result<(), JsValue> {
    let document = web_sys::window()
        .expect("no window")
        .document()
        .expect("no document");

    upsert_style(&document, "git-simple-styles", BASE_CSS);
    // `.gsr-app` on the mount root itself: the theme custom properties all
    // live here, so the CSS above depends on this class being present.
    let existing_class = container.get_attribute("class").unwrap_or_default();
    if !existing_class.split_whitespace().any(|c| c == "gsr-app") {
        let combined = if existing_class.is_empty() {
            "gsr-app".to_string()
        } else {
            format!("{existing_class} gsr-app")
        };
        let _ = container.set_attribute("class", &combined);
    }

    let status: Signal<String> = Signal::new(String::new());

    let dispatch: Rc<dyn Fn(Msg)> = {
        let status = status.clone();
        Rc::new(move |msg| match msg {
            Msg::OpenRepo => status.set("Directory picker: wired up in a later phase.".into()),
            Msg::LoadDemo => status.set("Demo repo: wired up in a later phase.".into()),
        })
    };

    let ui: UITree<Msg> = shell_tree();
    let mut root = tpt_appfront_dom::mount(container, &ui, dispatch)?;
    let _status = status;
    let _ = &mut root;
    Ok(())
}

/// The mounted-once shell.
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
                group.text("Browse any git repository — its files, history, and changes — without learning git. Plain-language actions, guardrails, and safe undo in the desktop edition.")
                    .class("gsr-sub");
            })
            .class("gsr-title-group");
        })
        .class("gsr-header");

        c.container(|actions| {
            actions.button("Open a repository").class("gsr-btn gsr-primary").on_click(Msg::OpenRepo);
            actions.button("Try the demo repository").class("gsr-btn").on_click(Msg::LoadDemo);
        });
    })
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
