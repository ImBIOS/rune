//! Pinned session tabs. OpenCode Desktop keeps its tab list on the client (Electron's
//! `drafts.sqlite`), never on the server, so this example keeps its own in
//! `<data dir>/rune-opencode/tabs.json`, one list per server.
//!
//! Until you change a tab here, each launch reads Desktop Beta's list (read-only, through the
//! `sqlite3` CLI) so the two show the same tabs; without Beta it starts from recent sessions.

use std::path::PathBuf;
use std::process::Command;

use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq)]
pub struct Tab {
    pub id: String,
    /// The title and directory when the tab was saved, for a session outside the loaded list.
    pub title: String,
    pub directory: String,
}

fn data_dir() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    if cfg!(target_os = "macos") {
        return Some(home.join("Library/Application Support"));
    }
    Some(std::env::var_os("XDG_DATA_HOME").map_or_else(|| home.join(".local/share"), PathBuf::from))
}

fn file() -> Option<PathBuf> {
    Some(data_dir()?.join("rune-opencode/tabs.json"))
}

fn parse(list: &Value) -> Vec<Tab> {
    list.as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| {
            Some(Tab {
                id: t["id"].as_str()?.to_owned(),
                title: t["title"].as_str().unwrap_or("").to_owned(),
                directory: t["directory"].as_str().unwrap_or("").to_owned(),
            })
        })
        .collect()
}

/// This example's saved list for `server`, if you've changed tabs here before.
pub fn load(server: &str) -> Option<Vec<Tab>> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(file()?).ok()?).ok()?;
    Some(parse(&v[server]))
}

pub fn save(server: &str, tabs: &[Tab]) {
    let Some(path) = file() else { return };
    let mut v: Value = std::fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(json!({}));
    v[server] = tabs.iter().map(|t| json!({ "id": t.id, "title": t.title, "directory": t.directory })).collect();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, v.to_string()).is_ok() {
        let _ = std::fs::rename(tmp, path);
    }
}

/// Desktop Beta's tabs for its own sidecar server (the local `:4096` service), most recently
/// written window first. Read-only: `mode=ro` and `-readonly`.
pub fn from_beta() -> Option<Vec<Tab>> {
    let db = data_dir()?.join("ai.opencode.desktop.beta/drafts.sqlite");
    if !db.exists() {
        return None;
    }
    let uri = format!("file:{}?mode=ro", db.display());
    let sql = "select key, value from state where name = (select name from state where key = 'tabs' \
               and name like 'opencode.window.%' order by updated_at desc limit 1) and key in ('tabs', 'tabs.info')";
    let out = Command::new("sqlite3").args(["-readonly", "-json", &uri, sql]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let rows: Value = serde_json::from_slice(&out.stdout).ok()?;
    let get = |k: &str| -> Value {
        rows.as_array()
            .into_iter()
            .flatten()
            .find(|r| r["key"] == k)
            .and_then(|r| serde_json::from_str(r["value"].as_str()?).ok())
            .unwrap_or(Value::Null)
    };
    let (tabs, info) = (get("tabs"), get("tabs.info"));
    let list: Vec<Tab> = tabs
        .as_array()?
        .iter()
        .filter(|t| t["type"] == "session" && t["server"] == "sidecar")
        .filter_map(|t| {
            let id = t["sessionId"].as_str()?.to_owned();
            // tabs.info is keyed "<server>\n/server/<base64 server>/session/<id>".
            let meta = info.as_object().and_then(|m| m.iter().find(|(k, _)| k.ends_with(&format!("/session/{id}"))).map(|e| e.1));
            let field = |k: &str| meta.and_then(|m| m[k].as_str()).unwrap_or("").to_owned();
            Some(Tab { title: field("title"), directory: field("directory"), id })
        })
        .collect();
    Some(list)
}
