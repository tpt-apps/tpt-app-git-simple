//! Pro desktop bridge (compiled only with `--features pro`).
//!
//! In the desktop shell the page talks to the native process over the
//! `window.__appfront` IPC bridge: `post(action, params, requestId)`
//! returns a promise the host resolves with the command result. Every git
//! operation that needs the real filesystem or network goes through here;
//! results are JSON (file contents base64).

#![allow(dead_code)]

use serde::Deserialize;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

/// Whether we're running inside the desktop shell (the bridge is injected
/// before page scripts run, exactly there).
pub fn is_desktop() -> bool {
    let Some(window) = web_sys::window() else {
        return false;
    };
    js_sys::Reflect::has(&window.into(), &JsValue::from_str("__appfront")).unwrap_or(false)
}

/// Calls a native command and awaits its JSON result. Errors carry the
/// host's plain-English message.
pub async fn call(action: &str, params: &serde_json::Value) -> Result<serde_json::Value, String> {
    let window = web_sys::window().ok_or_else(|| "no window".to_string())?;
    let appfront = js_sys::Reflect::get(&window, &JsValue::from_str("__appfront"))
        .map_err(|_| "desktop bridge not available".to_string())?;
    let post: js_sys::Function = js_sys::Reflect::get(&appfront, &JsValue::from_str("post"))
        .map_err(|_| "desktop bridge not available".to_string())?
        .dyn_into()
        .map_err(|_| "desktop bridge malformed".to_string())?;

    let params_js = js_sys::JSON::parse(&serde_json::to_string(params).unwrap_or_default())
        .map_err(|_| "params not serializable".to_string())?;
    let request_id = JsValue::from_f64(
        (js_sys::Math::random() * 1e9) as f64,
    );
    let promise = post
        .call3(&appfront, &JsValue::from_str(action), &params_js, &request_id)
        .map_err(|_| "bridge call failed".to_string())?;

    let result = JsFuture::from(js_sys::Promise::resolve(&promise))
        .await
        .map_err(|e| js_error_message(&e))?;
    let text: String = js_sys::JSON::stringify(&result)
        .map_err(|_| "bridge result not serializable".to_string())?
        .into();
    serde_json::from_str(&text).map_err(|e| format!("bridge result malformed: {e}"))
}

fn js_error_message(e: &JsValue) -> String {
    if let Some(err) = e.dyn_ref::<js_sys::Error>() {
        err.to_string().into()
    } else {
        format!("{e:?}")
    }
}

/// Reads an identity the user saved once (localStorage).
pub fn load_identity() -> (String, String) {
    let stored = |key: &str| -> Option<String> {
        let window = web_sys::window()?;
        let storage = window.local_storage().ok().flatten()?;
        storage.get_item(key).ok().flatten()
    };
    let name = stored("gsr.name").unwrap_or_default();
    let email = stored("gsr.email").unwrap_or_default();
    (name, email)
}

pub fn save_identity(name: &str, email: &str) {
    if let Some(window) = web_sys::window() {
        if let Ok(Some(storage)) = window.local_storage() {
            let _ = storage.set_item("gsr.name", name);
            let _ = storage.set_item("gsr.email", email);
        }
    }
}

// ----- result shapes (mirror desktop/src/git_host.rs) -----

#[derive(Debug, Deserialize)]
pub struct Snapshot {
    pub name: String,
    pub files: Vec<(String, String)>,
    #[serde(default)]
    pub skipped: Vec<String>,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub totalBytes: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StatusRow {
    pub path: String,
    pub kind: String, // "deleted" | "modified" | "untracked"
}

impl StatusRow {
    pub fn label(&self) -> &'static str {
        match self.kind.as_str() {
            "deleted" => "Deleted",
            "untracked" => "New file",
            _ => "Changed",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConflictReport {
    pub path: String,
    #[serde(default)]
    pub deleteModify: bool,
    pub headline: String,
    pub steps: Vec<String>,
    #[serde(default)]
    pub sections: Vec<ConflictSection>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConflictSection {
    #[serde(rename = "where")]
    pub where_: String,
    #[serde(default)]
    pub ours: Vec<String>,
    #[serde(default)]
    pub theirs: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BackupInfo {
    #[serde(rename = "ref")]
    pub ref_name: String,
    #[serde(default)]
    pub when: i64,
    #[serde(default)]
    pub paths: Vec<String>,
}
