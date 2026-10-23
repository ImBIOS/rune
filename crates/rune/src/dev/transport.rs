//! Blocking socket I/O stays off the app thread. Queues and peer counts are bounded.
//! The port also serves the knobs page over plain HTTP, so `http://<address>/` is the panel.
use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tungstenite::{Message, WebSocket, protocol::WebSocketConfig};
use crate::Waker;

pub(crate) struct Request {
    pub message: Value,
    pub reply: mpsc::Sender<Value>,
}

#[derive(Default)]
pub(crate) struct Bridge {
    pub incoming: VecDeque<Request>,
    pub revision: u64,
    pub latest: Value,
}

pub(crate) struct Server {
    pub address: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    pub fn bind(address: SocketAddr, bridge: Arc<Mutex<Bridge>>, wake: Waker) -> io::Result<Self> {
        if !local_ip(address.ip()) { return Err(io::Error::new(io::ErrorKind::InvalidInput, "dev panel must bind loopback or a Tailscale address")); }
        let listener = TcpListener::bind(address)?;
        let address = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let thread = thread::Builder::new().name("rune-dev-listener".into()).spawn(move || {
            let mut peers: Vec<JoinHandle<()>> = Vec::new();
            for accepted in listener.incoming() {
                if stopping.load(Ordering::Acquire) { break; }
                let stream = match accepted {
                    Ok(stream) => stream,
                    Err(e) => { eprintln!("Rune dev accept: {e}"); break; }
                };
                let mut i = 0;
                while i < peers.len() {
                    if peers[i].is_finished() {
                        if peers.swap_remove(i).join().is_err() { eprintln!("Rune dev peer panicked"); }
                    } else { i += 1; }
                }
                if peers.len() >= 8 { continue; }
                let (bridge, wake, stop) = (bridge.clone(), wake.clone(), stopping.clone());
                match thread::Builder::new().name("rune-dev-peer".into()).spawn(move || {
                    if let Err(e) = peer(stream, bridge, wake, stop) {
                        // Disconnects are ordinary; malformed requests receive a protocol error.
                        if !matches!(e, tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed | tungstenite::Error::Io(_)) {
                            eprintln!("Rune dev peer: {e}");
                        }
                    }
                }) {
                    Ok(peer) => peers.push(peer),
                    Err(e) => eprintln!("Rune dev peer thread: {e}"),
                }
            }
            for peer in peers {
                if peer.join().is_err() { eprintln!("Rune dev peer panicked"); }
            }
        })?;
        Ok(Self { address, stop, thread: Some(thread) })
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Wake blocking accept. Peer reads/writes have bounded timeouts.
        if let Err(e) = TcpStream::connect_timeout(&self.address, Duration::from_millis(250)) {
            eprintln!("Rune dev listener shutdown: {e}");
        }
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() { eprintln!("Rune dev listener panicked"); }
        }
    }
}

fn send(socket: &mut WebSocket<TcpStream>, value: &Value) -> tungstenite::Result<()> {
    socket.send(Message::Text(value.to_string().into()))
}

fn peer(stream: TcpStream, bridge: Arc<Mutex<Bridge>>, wake: Waker, stop: Arc<AtomicBool>) -> tungstenite::Result<()> {
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(Duration::from_millis(250)))?;
    stream.set_write_timeout(Some(Duration::from_millis(250)))?;
    match request_head(&stream) {
        Ok(Some(head)) if !same_machine_or_tailnet(&head) => return Ok(respond(stream, &head, "403 Forbidden", "text/plain", "Forbidden")?),
        Ok(Some(head)) if !is_upgrade(&head) => return Ok(serve_page(stream, &head)?),
        Ok(Some(_)) => {}
        Ok(None) | Err(_) => return Ok(()), // Closed, oversized or slow: nothing to answer.
    }
    let config = WebSocketConfig::default().max_message_size(Some(64 * 1024)).max_frame_size(Some(64 * 1024));
    let mut socket = match tungstenite::accept_with_config(stream, Some(config)) {
        Ok(socket) => socket,
        Err(_) => return Ok(()), // Failed or timed-out handshakes own no app resources.
    };
    socket.get_mut().set_read_timeout(Some(Duration::from_millis(16)))?;
    let (reply, replies) = mpsc::channel();
    let mut revision = u64::MAX;
    while !stop.load(Ordering::Acquire) {
        let snapshot = {
            let b = bridge.lock().unwrap();
            if revision == b.revision { None } else {
                revision = b.revision;
                Some(b.latest.clone())
            }
        };
        if let Some(snapshot) = snapshot { send(&mut socket, &snapshot)?; }
        for response in replies.try_iter() { send(&mut socket, &response)?; }
        match socket.read() {
            Ok(Message::Text(text)) => {
                match serde_json::from_str::<Value>(&text) {
                    Ok(message) if message["type"] == "get" => {
                        let snapshot = bridge.lock().unwrap().latest.clone();
                        send(&mut socket, &snapshot)?;
                    }
                    Ok(message) => {
                        let queued = {
                            let mut b = bridge.lock().unwrap();
                            if b.incoming.len() >= 256 { false } else {
                                b.incoming.push_back(Request { message, reply: reply.clone() });
                                true
                            }
                        };
                        if queued { wake(); } else { send(&mut socket, &json!({"type":"error","message":"command queue is full"}))?; }
                    }
                    Err(e) => send(&mut socket, &json!({"type":"error","message":e.to_string()}))?,
                }
            }
            Ok(Message::Close(_)) => break,
            Ok(_) => socket.flush()?,
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// The bundle `scripts/dev-panel/build.ts` writes; `cargo test` checks it against its source.
pub(crate) const PANEL_JS: &str = include_str!("panel.js");
const PAGE: &str = r#"<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Rune</title></head><body><script type="module" src="/panel.js"></script></body></html>"#;

/// Peeks the request head without consuming it, so a WebSocket handshake still reads it whole.
fn request_head(stream: &TcpStream) -> io::Result<Option<String>> {
    let mut buffer = [0; 8192];
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let n = stream.peek(&mut buffer)?;
        if n == 0 { return Ok(None); }
        if let Some(end) = buffer[..n].windows(4).position(|w| w == b"\r\n\r\n") {
            // Its length must match the peeked bytes: the page server consumes exactly that many.
            return Ok(std::str::from_utf8(&buffer[..end + 4]).ok().map(str::to_owned));
        }
        if n == buffer.len() || Instant::now() > deadline { return Ok(None); }
        thread::sleep(Duration::from_millis(2));
    }
}

fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().skip(1).filter_map(|line| line.split_once(':'))
        .find(|(n, _)| n.trim().eq_ignore_ascii_case(name)).map(|(_, value)| value.trim())
}

fn is_upgrade(head: &str) -> bool {
    header(head, "upgrade").is_some_and(|value| value.eq_ignore_ascii_case("websocket"))
}

fn local_ip(ip: std::net::IpAddr) -> bool {
    ip.is_loopback() || match ip {
        std::net::IpAddr::V4(ip) => ip.octets()[0] == 100 && (64..128).contains(&ip.octets()[1]),
        std::net::IpAddr::V6(ip) => ip.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
    }
}

/// Another site's tab may reach a loopback port, directly or by DNS rebinding. Both the
/// `Host` and any `Origin` (native clients send none) must name this machine or the tailnet.
pub(crate) fn same_machine_or_tailnet(head: &str) -> bool {
    fn allowed(authority: &str) -> bool {
        let host = match authority.strip_prefix('[') {
            Some(rest) => rest.split_once(']').map_or("", |(host, _)| host),
            None => authority.rsplit_once(':').map_or(authority, |(host, _)| host),
        }.to_ascii_lowercase();
        host == "localhost" || host.ends_with(".ts.net") || host.parse().is_ok_and(local_ip)
    }
    let origin = header(head, "origin").map(|origin| {
        origin.split_once("://").is_some_and(|(scheme, rest)| matches!(scheme, "http" | "https") && allowed(rest.split('/').next().unwrap_or(rest)))
    });
    header(head, "host").is_some_and(allowed) && origin.unwrap_or(true)
}

fn serve_page(stream: TcpStream, head: &str) -> io::Result<()> {
    let mut parts = head.split_ascii_whitespace();
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let path = path.split('?').next().unwrap_or(path);
    let (status, kind, body) = match (method, path) {
        ("GET" | "HEAD", "/" | "/index.html") => ("200 OK", "text/html", PAGE),
        ("GET" | "HEAD", "/panel.js") => ("200 OK", "text/javascript", PANEL_JS),
        ("GET" | "HEAD", _) => ("404 Not Found", "text/plain", "Not found"),
        _ => ("405 Method Not Allowed", "text/plain", "Method not allowed"),
    };
    respond(stream, head, status, kind, body)
}

fn respond(mut stream: TcpStream, head: &str, status: &str, kind: &str, body: &str) -> io::Result<()> {
    // Consume the request first: closing with unread bytes resets the connection mid-response.
    stream.read_exact(&mut vec![0; head.len()])?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let method = head.split_ascii_whitespace().next().unwrap_or("");
    write!(stream, "HTTP/1.1 {status}\r\nContent-Type: {kind}; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-cache\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n", body.len())?;
    if method != "HEAD" { stream.write_all(body.as_bytes())?; }
    stream.flush()?;
    stream.shutdown(std::net::Shutdown::Write)
}
