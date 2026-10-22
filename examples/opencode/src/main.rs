//! OpenCode Desktop's home and session pages in Rune, live against an OpenCode server
//! (read-only) or from a synthetic fixture.

mod icons;
mod live;
mod markdown;
mod model;
mod palette;
mod pickers;
mod view;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::mpsc::{Receiver, Sender};

use rune::{AppSpec, Key, KeyEvent, TextEdit, Tx, Waker, edge};
use serde_json::Value;

use live::{DEFAULT_SERVER, Incoming, Request, Server};
use model::{Message, Project, SessionInfo, Transcript};

const FIXTURE: &str = include_str!("../assets/fixture.json");

#[derive(Clone, Debug, PartialEq)]
pub enum Route {
    Home,
    Session(String),
    /// "New session": an empty page with the composer.
    Draft,
}

pub enum Source {
    Live { server: Server, requests: Option<Sender<Request>>, allow_send: bool },
    Fixture,
}

pub struct App {
    pub source: Source,
    inbox: Option<Receiver<Incoming>>,
    pub projects: Vec<Project>,
    pub sessions: Vec<SessionInfo>,
    pub route: Route,
    pub transcripts: HashMap<String, Transcript>,
    pub expanded: HashSet<String>,
    pub open_projects: HashSet<String>,
    pub composer: TextEdit,
    pub search: TextEdit,
    pub pickers: pickers::Pickers,
    pub edge: usize,
    pub reduced_motion: bool,
    pub status: String,
    /// Wall-clock milliseconds for relative times (fixture: its own fixed "now").
    pub now_ms: i64,
    base_ms: i64,
    clock: f64,
    fixture: Value,
    playback: VecDeque<(f64, String, Value)>,
    sends: u32,
    waker: Option<Waker>,
}

fn arg(name: &str) -> Option<String> {
    let a = rune::extra_args();
    a.iter().position(|x| x == name).and_then(|i| a.get(i + 1).cloned())
}

fn flag(name: &str) -> bool {
    rune::extra_args().iter().any(|x| x == name)
}

fn wall_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

impl App {
    fn empty() -> App {
        let edge_name = arg("--edge").unwrap_or_else(|| "feather".into());
        let edge = edge::ALL.iter().position(|e| e.name == edge_name).unwrap_or(0);
        App {
            source: Source::Fixture,
            inbox: None,
            projects: Vec::new(),
            sessions: Vec::new(),
            route: Route::Home,
            transcripts: HashMap::new(),
            expanded: HashSet::new(),
            open_projects: HashSet::new(),
            composer: TextEdit::new(""),
            search: TextEdit::new(""),
            pickers: pickers::Pickers::default(),
            edge,
            reduced_motion: flag("--reduced-motion"),
            status: String::new(),
            now_ms: 0,
            base_ms: 0,
            clock: 0.0,
            fixture: Value::Null,
            playback: VecDeque::new(),
            sends: 0,
            waker: None,
        }
    }

    fn new() -> App {
        let mut app = Self::empty();
        let fixture_path = arg("--fixture");
        let server = Server::new(&arg("--server").unwrap_or_else(|| DEFAULT_SERVER.into()));
        let want_live = fixture_path.is_none() && (flag("--live") || server.reachable());
        if want_live && app.load_live(server) {
            return app;
        }
        let src = match &fixture_path {
            Some(p) => std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{p}: {e}")),
            None => FIXTURE.to_owned(),
        };
        app.load_fixture(&src);
        app
    }

    fn load_fixture(&mut self, src: &str) {
        let v: Value = serde_json::from_str(src).expect("fixture JSON");
        self.pickers.load(&v);
        self.base_ms = v["now"].as_i64().unwrap_or(0);
        self.now_ms = self.base_ms;
        self.projects = model::parse_projects(&v["projects"]);
        self.sessions = model::parse_sessions(&v["sessions"]);
        if let Some(map) = v["messages"].as_object() {
            for (id, list) in map {
                let msgs = model::parse_messages(&serde_json::json!({ "data": list }));
                self.transcripts.insert(id.clone(), Transcript { session: id.clone(), messages: msgs });
            }
        }
        self.status = "Fixture".into();
        self.fixture = v;
        self.source = Source::Fixture;
        self.sort_projects();
    }

    fn load_live(&mut self, server: Server) -> bool {
        // The first loads are synchronous so the first frame (and a headless snapshot) has data.
        let Ok(p) = server.get("/api/project") else { return false };
        self.projects = model::parse_projects(&p);
        if let Ok(s) = server.get("/api/session?limit=120") {
            self.sessions = model::parse_sessions(&s);
        }
        self.base_ms = wall_ms();
        self.now_ms = self.base_ms;
        self.status = format!("Live · {}", server.base.trim_start_matches("http://"));
        let allow_send = flag("--allow-send");
        self.source = Source::Live { server, requests: None, allow_send };
        self.sort_projects();
        true
    }

    fn sort_projects(&mut self) {
        // Projects by their latest session, then by activity.
        let latest: HashMap<&str, i64> = self.sessions.iter().fold(HashMap::new(), |mut m, s| {
            let e = m.entry(s.project.as_str()).or_insert(0);
            *e = (*e).max(s.updated);
            m
        });
        let key = |p: &Project| latest.get(p.id.as_str()).copied().unwrap_or(0).max(p.active);
        let mut ps = std::mem::take(&mut self.projects);
        ps.sort_by_key(|p| std::cmp::Reverse(key(p)));
        self.projects = ps;
        if let Some(first) = self.projects.first() {
            self.open_projects.insert(first.id.clone());
        }
    }

    pub fn live(&self) -> bool {
        matches!(self.source, Source::Live { .. })
    }

    pub fn session(&self, id: &str) -> Option<&SessionInfo> {
        self.sessions.iter().find(|s| s.id == id)
    }

    pub fn project(&self, id: &str) -> Option<&Project> {
        self.projects.iter().find(|p| p.id == id)
    }

    /// Opens a session: lands at the bottom on the first frame with nothing animating in.
    pub fn open(&mut self, id: &str, tx: &mut Tx) {
        self.close_picker(false);
        self.route = Route::Session(id.to_owned());
        if let Some(s) = self.session(id) {
            let p = s.project.clone();
            self.open_projects.insert(p);
        }
        tx.settle();
        match &self.source {
            Source::Live { requests: Some(r), .. } => {
                let _ = r.send(Request::Session(id.to_owned()));
            }
            Source::Live { .. } => {}
            Source::Fixture => self.start_stream_if_due(id),
        }
    }

    fn start_stream_if_due(&mut self, id: &str) {
        let stream = &self.fixture["stream"];
        if stream["session"].as_str() != Some(id) || stream["played"].as_bool() == Some(true) || flag("--no-stream") {
            return;
        }
        let at = self.clock;
        for e in stream["events"].as_array().into_iter().flatten() {
            let t = at + e["t"].as_f64().unwrap_or(0.0);
            self.playback.push_back((t, e["type"].as_str().unwrap_or("").to_owned(), e["data"].clone()));
        }
        self.fixture["stream"]["played"] = true.into();
        self.wake_at_playback();
    }

    /// Applies the fixture's streaming turn at once (the finished session).
    fn finish_stream(&mut self) {
        let events = self.fixture["stream"]["events"].as_array().cloned().unwrap_or_default();
        for e in events {
            self.apply_event(e["type"].as_str().unwrap_or(""), &e["data"]);
        }
        self.fixture["stream"]["played"] = true.into();
    }

    /// Windowed runs sleep between frames; a timer thread wakes the window for each event.
    fn wake_at_playback(&self) {
        let Some(w) = self.waker.clone() else { return };
        let times: Vec<f64> = self.playback.iter().map(|e| e.0 - self.clock).collect();
        std::thread::spawn(move || {
            let start = std::time::Instant::now();
            for t in times {
                let d = std::time::Duration::from_secs_f64(t.max(0.0));
                if let Some(rest) = d.checked_sub(start.elapsed()) {
                    std::thread::sleep(rest);
                }
                w();
            }
        });
    }

    pub fn send(&mut self, tx: &mut Tx) {
        let text = self.composer.text.trim().to_owned();
        if text.is_empty() {
            return;
        }
        let Route::Session(id) = self.route.clone() else {
            self.status = "Pick a session to send to (this build never creates sessions)".into();
            return;
        };
        match &self.source {
            Source::Live { allow_send: false, .. } => {
                self.status = "Read-only: relaunch with --allow-send to send prompts".into();
                return;
            }
            Source::Live { server, .. } => {
                let (server, sid, body) = (server.clone(), id.clone(), text.clone());
                std::thread::spawn(move || {
                    let _ = server.send_prompt(&sid, &body);
                });
            }
            Source::Fixture => {
                let selection = self.selection();
                self.sends += 1;
                let reply = format!("msg_zzzzreply{:04}", self.sends);
                let at = self.clock;
                for e in self.fixture["reply"].as_array().cloned().unwrap_or_default() {
                    let mut d = e["data"].clone();
                    d["sessionID"] = id.clone().into();
                    d["assistantMessageID"] = reply.clone().into();
                    if e["type"] == "session.step.started" {
                        d["agent"] = selection.agent.clone().into();
                        d["model"] = serde_json::json!({"id": selection.model.id, "providerID": selection.model.provider, "variant": selection.model.variant});
                    }
                    self.playback.push_back((at + e["t"].as_f64().unwrap_or(0.0), e["type"].as_str().unwrap_or("").into(), d));
                }
                self.wake_at_playback();
            }
        }
        let t = self.transcripts.entry(id.clone()).or_insert_with(|| Transcript { session: id.clone(), messages: vec![] });
        t.messages.push(Message::User { id: format!("msg_zzzzsent{:04}", self.sends), text });
        tx.fly_text("prompt", format!("text-msg_zzzzsent{:04}", self.sends));
        self.composer.clear();
    }

    fn apply_event(&mut self, ty: &str, data: &Value) -> bool {
        let sid = data["sessionID"].as_str().unwrap_or("").to_owned();
        let now = self.now_ms;
        let mut changed = false;
        if let Some(t) = self.transcripts.get_mut(&sid) {
            changed |= t.apply(ty, data, now);
        }
        if ty.starts_with("session.") && !sid.is_empty() {
            if let Some(s) = self.sessions.iter_mut().find(|s| s.id == sid) {
                if s.updated < now - 1000 || ty == "session.step.ended" {
                    s.updated = now;
                    changed = true;
                }
                if ty == "session.renamed"
                    && let Some(t) = data["title"].as_str()
                {
                    s.title = t.to_owned();
                    changed = true;
                }
            } else if ty == "session.created" || ty == "session.step.started" {
                // A session we haven't listed: refresh the list (one GET).
                if let Source::Live { requests: Some(r), .. } = &self.source {
                    let _ = r.send(Request::Sessions);
                }
            }
            // A step started in the open session may follow a prompt we haven't seen.
            if ty == "session.step.started"
                && self.route == Route::Session(sid.clone())
                && let Source::Live { requests: Some(r), .. } = &self.source
            {
                let _ = r.send(Request::Session(sid));
            }
        }
        if ty == "project.updated"
            && let Some(p) = self.projects.iter_mut().find(|p| Some(p.id.as_str()) == data["id"].as_str())
        {
            p.active = data["time"]["active"].as_i64().unwrap_or(p.active);
        }
        changed
    }
}

fn poll(s: &mut App, now: f64, tx: &mut Tx) -> bool {
    s.clock = now;
    s.now_ms = if s.live() { wall_ms() } else { s.base_ms + (now * 1000.0) as i64 };
    let mut changed = false;
    if let Some(inbox) = s.inbox.take() {
        for m in inbox.try_iter() {
            match m {
                Incoming::Sessions(list) => {
                    s.sessions = list;
                    s.sort_projects();
                }
                Incoming::Messages { session, messages } => {
                    let first = !s.transcripts.contains_key(&session);
                    let t = s.transcripts.entry(session.clone()).or_insert_with(|| Transcript { session: session.clone(), messages: vec![] });
                    t.merge(messages);
                    if first && s.route == Route::Session(session) {
                        // Guarantee 8: the first load of the open session lands, nothing grows in.
                        tx.settle();
                    }
                }
                Incoming::Event { ty, data } => {
                    s.apply_event(&ty, &data);
                }
                Incoming::Status(e) => s.status = e,
            }
            changed = true;
        }
        s.inbox = Some(inbox);
    }
    while s.playback.front().is_some_and(|e| e.0 <= now + 1e-9) {
        let (_, ty, data) = s.playback.pop_front().unwrap();
        s.apply_event(&ty, &data);
        changed = true;
    }
    changed
}

fn setup(s: &mut App, waker: Waker) {
    s.waker = Some(waker.clone());
    if !s.playback.is_empty() {
        s.wake_at_playback();
    }
    if let Source::Live { server, requests, .. } = &mut s.source {
        let (r, inbox) = live::connect(server.clone(), waker);
        *requests = Some(r);
        s.inbox = Some(inbox);
    }
    // `--session <id or title words>` opens a session at start.
    if let Some(q) = arg("--session") {
        let q = q.to_lowercase();
        let found = s.sessions.iter().find(|x| x.id == q || x.title.to_lowercase().contains(&q)).map(|x| x.id.clone());
        if let Some(id) = found {
            s.open(&id, &mut Tx::default());
        }
    }
    // The first frame of a live session already has its transcript (one synchronous GET).
    if let (Route::Session(id), Source::Live { server, .. }) = (&s.route, &s.source)
        && !s.transcripts.contains_key(id)
        && let Ok(v) = server.get(&format!("/api/session/{id}/message?limit=80&order=desc"))
    {
        s.transcripts.insert(id.clone(), Transcript { session: id.clone(), messages: model::parse_messages(&v) });
    }
}

fn on_key(s: &mut App, ev: &KeyEvent, tx: &mut Tx) -> bool {
    if s.picker_key(ev) { return true; }
    match &ev.key {
        Key::Command('e') => {
            s.edge = (s.edge + 1) % edge::ALL.len();
            true
        }
        Key::Command('n') => {
            s.route = Route::Draft;
            tx.settle();
            true
        }
        Key::Escape if s.route != Route::Home && !s.composer.focused => {
            s.route = Route::Home;
            tx.settle();
            true
        }
        _ => false,
    }
}

fn open_scene(s: &mut App) {
    let id = s.fixture["open"].as_str().map(str::to_owned).or_else(|| s.sessions.first().map(|x| x.id.clone()));
    if let Some(id) = id {
        s.route = Route::Session(id.clone());
        if let Some(p) = s.session(&id).map(|x| x.project.clone()) {
            s.open_projects.insert(p);
        }
    }
}

fn main() {
    rune::run(AppSpec {
        title: "OpenCode",
        size: (1280.0, 820.0),
        min_size: None,
        init: App::new,
        view: view::app,
        scenes: &[
            ("home", |_| {}),
            ("session", |s| {
                open_scene(s);
                s.finish_stream();
            }),
            ("streaming", |s| {
                open_scene(s);
                // Playback starts on the first poll.
                if let Some(id) = s.fixture["open"].as_str().map(str::to_owned) {
                    s.start_stream_if_due(&id);
                }
            }),
            ("draft", |s| s.route = Route::Draft),
        ],
        fonts: &[
            include_bytes!("../assets/fonts/InterVariable.ttf"),
            include_bytes!("../assets/fonts/IBMPlexMono-Regular.ttf"),
            include_bytes!("../assets/fonts/IBMPlexMono-Medium.ttf"),
        ],
        families: ("Inter", "IBM Plex Mono"),
        setup: Some(setup),
        poll: Some(poll),
        on_key: Some(on_key),
    });
}
