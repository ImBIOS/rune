//! OpenCode's data as this app sees it: projects, sessions and transcripts, parsed leniently
//! from the server's JSON (`/api/project`, `/api/session`, `/api/session/:id/message`) and
//! kept current by the same events `/api/event` streams.

use serde_json::Value;

#[derive(Clone, Debug)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub active: i64,
}

#[derive(Clone, Debug, Default)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl Tokens {
    fn parse(v: &Value) -> Tokens {
        let n = |v: &Value| v.as_u64().unwrap_or(0);
        Tokens {
            input: n(&v["input"]),
            output: n(&v["output"]),
            cache_read: n(&v["cache"]["read"]),
            cache_write: n(&v["cache"]["write"]),
        }
    }
    /// Tokens in the context window for this step.
    pub fn context(&self) -> u64 {
        self.input + self.cache_read + self.cache_write + self.output
    }
}

#[derive(Clone, Debug, Default)]
pub struct ModelRef {
    pub id: String,
    pub provider: String,
    pub variant: String,
}

impl ModelRef {
    fn parse(v: &Value) -> ModelRef {
        ModelRef {
            id: s(&v["id"]),
            provider: s(&v["providerID"]),
            variant: s(&v["variant"]),
        }
    }

    /// "claude-opus-5-5" → "Claude Opus 5.5".
    pub fn display(&self) -> String {
        let mut out = Vec::new();
        let parts: Vec<&str> = self.id.split('-').collect();
        let mut i = 0;
        while i < parts.len() {
            let p = parts[i];
            if p.chars().all(|c| c.is_ascii_digit()) {
                let mut num = p.to_owned();
                while i + 1 < parts.len() && parts[i + 1].chars().all(|c| c.is_ascii_digit()) && parts[i + 1].len() <= 2 {
                    num.push('.');
                    num.push_str(parts[i + 1]);
                    i += 1;
                }
                out.push(num);
            } else if p.eq_ignore_ascii_case("gpt") {
                out.push("GPT".into());
            } else {
                out.push(capitalize(p));
            }
            i += 1;
        }
        if out.is_empty() { "Default model".into() } else { out.join(" ") }
    }
}

pub fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map_or(String::new(), |f| f.to_uppercase().collect::<String>() + c.as_str())
}

#[derive(Clone, Debug, Default)]
pub struct SessionInfo {
    pub id: String,
    pub parent: Option<String>,
    pub project: String,
    pub title: String,
    pub directory: String,
    pub updated: i64,
    pub model: ModelRef,
    pub cost: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ToolStatus {
    Pending,
    Running,
    Completed,
    Error,
}

#[derive(Clone, Debug)]
pub enum Part {
    Text { text: String },
    Reasoning { text: String, started: i64, completed: Option<i64> },
    Tool { id: String, name: String, status: ToolStatus, title: String, input: Value },
}

#[derive(Clone, Debug)]
pub enum Message {
    User { id: String, text: String },
    Assistant(Assistant),
    /// Shell, compaction, notices: shown as a quiet one-line row.
    Note { id: String, text: String },
}

#[derive(Clone, Debug, Default)]
pub struct Assistant {
    pub id: String,
    pub completed: Option<i64>,
    pub parts: Vec<Part>,
    pub cost: f64,
    pub tokens: Tokens,
    pub error: Option<String>,
}

impl Message {
    pub fn id(&self) -> &str {
        match self {
            Message::User { id, .. } | Message::Note { id, .. } => id,
            Message::Assistant(a) => &a.id,
        }
    }
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or("").to_owned()
}

fn i(v: &Value) -> i64 {
    v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)).unwrap_or(0)
}

pub fn parse_projects(v: &Value) -> Vec<Project> {
    let list = v.as_array().or_else(|| v["data"].as_array());
    list.into_iter()
        .flatten()
        .filter_map(|p| {
            let dir = s(&p["canonical"]);
            if dir.is_empty() {
                return None;
            }
            let name = dir.rsplit('/').next().unwrap_or(&dir).to_owned();
            Some(Project { id: s(&p["id"]), name, active: i(&p["time"]["active"]).max(i(&p["time"]["updated"])) })
        })
        .collect()
}

pub fn parse_session(p: &Value) -> SessionInfo {
    SessionInfo {
        id: s(&p["id"]),
        parent: p["parentID"].as_str().map(str::to_owned),
        project: s(&p["projectID"]),
        title: s(&p["title"]),
        directory: s(&p["location"]["directory"]),
        updated: i(&p["time"]["updated"]),
        model: ModelRef::parse(&p["model"]),
        cost: p["cost"].as_f64().unwrap_or(0.0),
    }
}

pub fn parse_sessions(v: &Value) -> Vec<SessionInfo> {
    v["data"].as_array().or_else(|| v.as_array()).into_iter().flatten().map(parse_session).collect()
}

fn tool_title(name: &str, input: &Value) -> String {
    let get = |k: &str| input[k].as_str().map(str::to_owned);
    let short = |p: String| {
        let parts: Vec<&str> = p.split('/').collect();
        if parts.len() > 3 { parts[parts.len() - 2..].join("/") } else { p }
    };
    let t = match name {
        "shell" | "bash" => get("description").or_else(|| get("command")),
        "read" | "write" | "edit" => get("filePath").or_else(|| get("path")).map(short),
        "grep" | "glob" => get("pattern"),
        "task" | "subagent" => get("description").or_else(|| get("prompt")),
        "webfetch" => get("url"),
        "skill" => get("name").or_else(|| get("id")),
        _ => None,
    };
    let t = t.unwrap_or_default();
    let line = t.lines().next().unwrap_or("").trim().to_owned();
    if line.chars().count() > 120 { line.chars().take(119).collect::<String>() + "…" } else { line }
}

fn tool_status(v: &Value) -> ToolStatus {
    match v.as_str().unwrap_or("") {
        "completed" => ToolStatus::Completed,
        "error" | "failed" => ToolStatus::Error,
        "running" => ToolStatus::Running,
        _ => ToolStatus::Pending,
    }
}

fn parse_part(c: &Value) -> Option<Part> {
    Some(match c["type"].as_str()? {
        "text" => Part::Text { text: s(&c["text"]) },
        "reasoning" => Part::Reasoning {
            text: s(&c["text"]),
            started: i(&c["time"]["created"]),
            completed: c["time"]["completed"].as_i64(),
        },
        "tool" => {
            let name = s(&c["name"]);
            let input = c["state"]["input"].clone();
            Part::Tool {
                id: s(&c["id"]),
                title: tool_title(&name, &input),
                name,
                status: tool_status(&c["state"]["status"]),
                input,
            }
        }
        _ => return None,
    })
}

pub fn parse_message(m: &Value) -> Option<Message> {
    let id = s(&m["id"]);
    Some(match m["type"].as_str()? {
        "user" => Message::User { id, text: s(&m["text"]) },
        "assistant" => Message::Assistant(Assistant {
            id,
            completed: m["time"]["completed"].as_i64(),
            parts: m["content"].as_array().into_iter().flatten().filter_map(parse_part).collect(),
            cost: m["cost"].as_f64().unwrap_or(0.0),
            tokens: Tokens::parse(&m["tokens"]),
            error: m["error"]["message"].as_str().map(str::to_owned),
        }),
        "shell" => Message::Note { id, text: format!("$ {}", s(&m["command"])) },
        "compaction" => Message::Note { id, text: "Compacted the conversation".into() },
        _ => return None,
    })
}

/// Messages in chronological order from one `desc` page.
pub fn parse_messages(v: &Value) -> Vec<Message> {
    let mut out: Vec<Message> = v["data"].as_array().into_iter().flatten().filter_map(parse_message).collect();
    // Message IDs are time-ordered.
    out.sort_by(|a, b| a.id().cmp(b.id()));
    out
}

/// One open transcript, kept current from events.
#[derive(Clone, Debug, Default)]
pub struct Transcript {
    pub session: String,
    pub messages: Vec<Message>,
}

impl Transcript {
    fn assistant(&mut self, id: &str) -> Option<&mut Assistant> {
        self.messages.iter_mut().rev().find_map(|m| match m {
            Message::Assistant(a) if a.id == id => Some(a),
            _ => None,
        })
    }

    /// The assistant step still producing output, if any.
    pub fn busy(&self) -> bool {
        matches!(self.messages.last(), Some(Message::Assistant(a)) if a.completed.is_none() && a.error.is_none())
            || matches!(self.messages.last(), Some(Message::User { .. }))
    }

    pub fn merge(&mut self, fresh: Vec<Message>) {
        for m in fresh {
            if let Some(slot) = self.messages.iter_mut().find(|o| o.id() == m.id()) {
                // Keep live-streamed text when the fetched copy is behind.
                if let (Message::Assistant(old), Message::Assistant(new)) = (&*slot, &m)
                    && new.completed.is_none()
                    && old.parts.len() > new.parts.len()
                {
                    continue;
                }
                *slot = m;
            } else {
                let at = self.messages.partition_point(|o| o.id() < m.id());
                self.messages.insert(at, m);
            }
        }
    }

    /// Applies one `/api/event` payload for this session; returns whether anything changed.
    pub fn apply(&mut self, ty: &str, d: &Value, now: i64) -> bool {
        if d["sessionID"].as_str() != Some(self.session.as_str()) {
            return false;
        }
        let mid = s(&d["assistantMessageID"]);
        let ordinal = d["ordinal"].as_u64().map(|o| o as usize);
        match ty {
            "session.prompted" | "session.user.message" => {
                let id = s(&d["messageID"]);
                if !id.is_empty() && !self.messages.iter().any(|m| m.id() == id) {
                    self.messages.push(Message::User { id, text: s(&d["text"]) });
                }
            }
            "session.step.started" => {
                if self.assistant(&mid).is_none() {
                    self.messages.push(Message::Assistant(Assistant { id: mid, ..Default::default() }));
                }
            }
            "session.text.started" | "session.reasoning.started" => {
                let Some(a) = self.assistant(&mid) else { return false };
                let part = if ty == "session.text.started" {
                    Part::Text { text: String::new() }
                } else {
                    Part::Reasoning { text: String::new(), started: now, completed: None }
                };
                place(&mut a.parts, ordinal, part);
            }
            "session.text.delta" | "session.reasoning.delta" => {
                let Some(a) = self.assistant(&mid) else { return false };
                let delta = d["delta"].as_str().unwrap_or("");
                let want_text = ty == "session.text.delta";
                let at = ordinal.filter(|&o| o < a.parts.len()).or(a.parts.len().checked_sub(1));
                match at.map(|k| &mut a.parts[k]) {
                    Some(Part::Text { text }) if want_text => text.push_str(delta),
                    Some(Part::Reasoning { text, .. }) if !want_text => text.push_str(delta),
                    _ => {
                        let part = if want_text {
                            Part::Text { text: delta.to_owned() }
                        } else {
                            Part::Reasoning { text: delta.to_owned(), started: now, completed: None }
                        };
                        place(&mut a.parts, ordinal, part);
                    }
                }
            }
            "session.text.ended" | "session.reasoning.ended" => {
                let Some(a) = self.assistant(&mid) else { return false };
                let full = d["text"].as_str().map(str::to_owned);
                match ordinal.and_then(|o| a.parts.get_mut(o)) {
                    Some(Part::Text { text }) => {
                        if let Some(f) = full {
                            *text = f;
                        }
                    }
                    Some(Part::Reasoning { text, completed, .. }) => {
                        if let Some(f) = full {
                            *text = f;
                        }
                        *completed = Some(now);
                    }
                    _ => return false,
                }
            }
            "session.tool.input.started" | "session.tool.called" => {
                let Some(a) = self.assistant(&mid) else { return false };
                let id = s(&d["id"]);
                let input = d["input"].clone();
                let found = a.parts.iter_mut().find_map(|p| match p {
                    Part::Tool { id: tid, name, status, title, input: inp, .. } if *tid == id => {
                        Some((name, status, title, inp))
                    }
                    _ => None,
                });
                match found {
                    Some((name, status, title, inp)) => {
                        if ty == "session.tool.called" {
                            *status = ToolStatus::Running;
                            *title = tool_title(name, &input);
                            *inp = input;
                        }
                    }
                    None => {
                        let name = s(&d["name"]);
                        a.parts.push(Part::Tool {
                            id,
                            title: tool_title(&name, &input),
                            name,
                            status: if ty == "session.tool.called" { ToolStatus::Running } else { ToolStatus::Pending },
                            input,
                        });
                    }
                }
            }
            "session.tool.success" | "session.tool.failed" => {
                let Some(a) = self.assistant(&mid) else { return false };
                let id = s(&d["id"]);
                for p in &mut a.parts {
                    if let Part::Tool { id: tid, status, .. } = p
                        && *tid == id
                    {
                        *status = if ty == "session.tool.success" { ToolStatus::Completed } else { ToolStatus::Error };
                    }
                }
            }
            "session.step.ended" | "session.step.failed" => {
                let Some(a) = self.assistant(&mid) else { return false };
                a.completed = Some(now);
                a.cost = d["cost"].as_f64().unwrap_or(a.cost);
                if d["tokens"].is_object() {
                    a.tokens = Tokens::parse(&d["tokens"]);
                }
                if ty == "session.step.failed" {
                    a.error = Some(d["error"]["message"].as_str().unwrap_or("The step failed").to_owned());
                }
            }
            _ => return false,
        }
        true
    }
}

fn place(parts: &mut Vec<Part>, ordinal: Option<usize>, part: Part) {
    match ordinal {
        Some(o) if o < parts.len() => parts[o] = part,
        _ => parts.push(part),
    }
}

/// "3m ago"-style age from milliseconds, as Desktop's short relative times.
pub fn ago(now: i64, then: i64) -> String {
    let s = ((now - then) / 1000).max(0);
    if s < 60 {
        "now".into()
    } else if s < 3600 {
        format!("{}m ago", s / 60)
    } else if s < 86_400 {
        format!("{}h ago", s / 3600)
    } else {
        format!("{}d ago", s / 86_400)
    }
}

pub fn tokens_short(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_names() {
        let m = |id: &str| ModelRef { id: id.into(), ..Default::default() }.display();
        assert_eq!(m("claude-opus-5-5"), "Claude Opus 5.5");
        assert_eq!(m("gpt-6-astra"), "GPT 6 Astra");
        assert_eq!(m("gemini-4-argon"), "Gemini 4 Argon");
    }

    #[test]
    fn streams_text_by_ordinal() {
        let mut t = Transcript { session: "s".into(), messages: vec![] };
        let ev = |ty: &str, v: Value| (ty.to_owned(), v);
        let evs = [
            ev("session.step.started", serde_json::json!({"sessionID":"s","assistantMessageID":"m"})),
            ev("session.text.started", serde_json::json!({"sessionID":"s","assistantMessageID":"m","ordinal":0})),
            ev("session.text.delta", serde_json::json!({"sessionID":"s","assistantMessageID":"m","ordinal":0,"delta":"Hel"})),
            ev("session.text.delta", serde_json::json!({"sessionID":"s","assistantMessageID":"m","ordinal":0,"delta":"lo"})),
        ];
        for (ty, d) in &evs {
            assert!(t.apply(ty, d, 0));
        }
        let Message::Assistant(a) = &t.messages[0] else { panic!() };
        assert!(matches!(&a.parts[0], Part::Text { text } if text == "Hello"));
        assert!(t.busy());
    }
}
