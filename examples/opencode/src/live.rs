//! The OpenCode server connection: read-only GETs on a worker thread, `/api/event` on another,
//! both waking the window when data lands. Sending a prompt is a separate, gated path.
//!
//! The password comes from `~/.local/share/opencode-studio-migration/password` (or
//! `OPENCODE_SERVER_PASSWORD`) and only ever goes into the Authorization header.

use std::io::{BufRead, BufReader};
use std::net::{SocketAddr, TcpStream};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use rune::Waker;
use serde_json::Value;

use crate::model::{self, Message, SessionInfo};

pub const DEFAULT_SERVER: &str = "http://127.0.0.1:4096";

pub enum Incoming {
    Sessions(Vec<SessionInfo>),
    Info(SessionInfo),
    Messages { session: String, messages: Vec<Message> },
    Event { ty: String, data: Value },
    Status(String),
}

pub enum Request {
    Session(String),
    Sessions,
    /// One session's info (`time.idle`/`time.viewed` after it finishes or someone views it).
    Info(String),
}

#[derive(Clone)]
pub struct Server {
    pub base: String,
    auth: Option<String>,
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server").field("base", &self.base).finish_non_exhaustive()
    }
}

fn base64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for c in input.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for k in 0..4 {
            if k <= c.len() {
                out.push(T[(n >> (18 - 6 * k) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

impl Server {
    pub fn new(base: &str) -> Server {
        let password = std::env::var("OPENCODE_SERVER_PASSWORD").ok().or_else(|| {
            let home = std::env::var("HOME").ok()?;
            std::fs::read_to_string(format!("{home}/.local/share/opencode-studio-migration/password")).ok()
        });
        let auth = password.map(|p| format!("Basic {}", base64(format!("opencode:{}", p.trim()).as_bytes())));
        Server { base: base.trim_end_matches('/').to_owned(), auth }
    }

    fn addr(&self) -> Option<SocketAddr> {
        let host = self.base.strip_prefix("http://")?.split('/').next()?;
        let host = host.replace("localhost", "127.0.0.1");
        host.parse().ok()
    }

    /// A quick TCP check, so a missing server falls back to the fixture without waiting.
    pub fn reachable(&self) -> bool {
        self.addr().is_some_and(|a| TcpStream::connect_timeout(&a, Duration::from_millis(250)).is_ok())
    }

    fn agent(timeout: Option<Duration>) -> ureq::Agent {
        ureq::Agent::config_builder().timeout_global(timeout).http_status_as_error(true).build().into()
    }

    pub fn get(&self, path: &str) -> Result<Value, String> {
        let mut req = Self::agent(Some(Duration::from_secs(15))).get(format!("{}{path}", self.base));
        if let Some(a) = &self.auth {
            req = req.header("Authorization", a);
        }
        let mut resp = req.call().map_err(|e| format!("GET {path}: {e}"))?;
        let text = resp
            .body_mut()
            .with_config()
            .limit(64 << 20)
            .read_to_string()
            .map_err(|e| format!("GET {path}: {e}"))?;
        serde_json::from_str(&text).map_err(|e| format!("GET {path}: {e}"))
    }

    /// `POST /session/:id/prompt`. Only reachable with `--allow-send`, a flag for a human at the keyboard;
    /// agents and tests never pass it against the live server (tests use a local mock).
    pub fn send_prompt(&self, session: &str, text: &str) -> Result<(), String> {
        let path = format!("/api/session/{session}/prompt");
        let mut req = Self::agent(Some(Duration::from_secs(15)))
            .post(format!("{}{path}", self.base))
            .header("Content-Type", "application/json");
        if let Some(a) = &self.auth {
            req = req.header("Authorization", a);
        }
        let body = serde_json::json!({ "text": text }).to_string();
        req.send(body.as_bytes()).map(|_| ()).map_err(|e| format!("POST {path}: {e}"))
    }

    fn events(&self, inbox: &Sender<Incoming>, wake: &Waker) -> Result<(), String> {
        let mut req = Self::agent(None).get(format!("{}/api/event", self.base)).header("Accept", "text/event-stream");
        if let Some(a) = &self.auth {
            req = req.header("Authorization", a);
        }
        let resp = req.call().map_err(|e| format!("events: {e}"))?;
        let reader = BufReader::new(resp.into_body().into_reader());
        for line in reader.lines() {
            let line = line.map_err(|e| format!("events: {e}"))?;
            let Some(data) = line.strip_prefix("data:") else { continue };
            let Ok(v) = serde_json::from_str::<Value>(data.trim()) else { continue };
            let ty = v["type"].as_str().unwrap_or("").to_owned();
            if ty.is_empty() || ty == "server.heartbeat" {
                continue;
            }
            if inbox.send(Incoming::Event { ty, data: v["data"].clone() }).is_err() {
                return Ok(());
            }
            wake();
        }
        Err("events: stream ended".into())
    }
}

/// Starts the worker and event threads (the first loads already happened). Returns the
/// request sender and the inbox.
pub fn connect(server: Server, wake: Waker) -> (Sender<Request>, Receiver<Incoming>) {
    let (inbox_tx, inbox) = channel();
    let (req_tx, req_rx) = channel::<Request>();

    {
        let server = server.clone();
        let inbox_tx = inbox_tx.clone();
        let wake = wake.clone();
        std::thread::Builder::new()
            .name("opencode-fetch".into())
            .spawn(move || {
                let send = |m: Incoming| {
                    let ok = inbox_tx.send(m).is_ok();
                    wake();
                    ok
                };
                let sessions = |server: &Server| server.get("/api/session?limit=120").map(|v| model::parse_sessions(&v));
                for r in req_rx {
                    let ok = match r {
                        Request::Session(id) => match server.get(&format!("/api/session/{id}/message?limit=80&order=desc")) {
                            Ok(v) => send(Incoming::Messages { session: id, messages: model::parse_messages(&v) }),
                            Err(e) => send(Incoming::Status(e)),
                        },
                        Request::Sessions => match sessions(&server) {
                            Ok(list) => send(Incoming::Sessions(list)),
                            Err(e) => send(Incoming::Status(e)),
                        },
                        Request::Info(id) => match server.get(&format!("/api/session/{id}")) {
                            Ok(v) => send(Incoming::Info(model::parse_session(v.get("data").unwrap_or(&v)))),
                            Err(e) => send(Incoming::Status(e)),
                        },
                    };
                    if !ok {
                        return;
                    }
                    // Gentle: at most a few requests a second.
                    std::thread::sleep(Duration::from_millis(250));
                }
            })
            .expect("spawn fetch thread");
    }

    std::thread::Builder::new()
        .name("opencode-events".into())
        .spawn(move || {
            let mut backoff = 2;
            loop {
                if let Err(e) = server.events(&inbox_tx, &wake) {
                    if inbox_tx.send(Incoming::Status(e)).is_err() {
                        return;
                    }
                    wake();
                }
                std::thread::sleep(Duration::from_secs(backoff));
                backoff = (backoff * 2).min(30);
            }
        })
        .expect("spawn event thread");

    (req_tx, inbox)
}

/// A one-request HTTP server for tests: answers 200 and returns what it received.
#[cfg(test)]
pub fn mock_once() -> (String, std::thread::JoinHandle<String>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind mock");
    let base = format!("http://{}", listener.local_addr().unwrap());
    let h = std::thread::spawn(move || {
        let (mut s, _) = listener.accept().expect("accept");
        s.set_read_timeout(Some(Duration::from_secs(5))).ok();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = s.read(&mut chunk).unwrap_or(0);
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            let text = String::from_utf8_lossy(&buf);
            if let Some(h) = text.find("\r\n\r\n") {
                let len = text[..h]
                    .lines()
                    .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap_or(0)))
                    .unwrap_or(0usize);
                if buf.len() >= h + 4 + len {
                    break;
                }
            }
        }
        let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Type: application/json\r\n\r\n{}");
        String::from_utf8_lossy(&buf).into_owned()
    });
    (base, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc4648() {
        assert_eq!(base64(b"opencode:pw"), "b3BlbmNvZGU6cHc=");
        assert_eq!(base64(b"ab"), "YWI=");
        assert_eq!(base64(b"abc"), "YWJj");
    }

    #[test]
    fn send_posts_prompt_to_mock() {
        let (base, h) = mock_once();
        let server = Server { base, auth: Some("Basic dGVzdA==".into()) };
        server.send_prompt("ses_test", "hello there").unwrap();
        let req = h.join().unwrap();
        assert!(req.starts_with("POST /api/session/ses_test/prompt HTTP/1.1"), "{req}");
        assert!(req.contains("authorization: Basic dGVzdA==") || req.contains("Authorization: Basic dGVzdA=="));
        assert!(req.ends_with(r#"{"text":"hello there"}"#), "{req}");
    }
}
