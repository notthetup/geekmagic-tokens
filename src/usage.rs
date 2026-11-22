//! What the screens show, and the loop that polls Codex and Claude for it.

use std::{
    error::Error,
    net::Ipv4Addr,
    sync::{atomic::Ordering, mpsc::Receiver, Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use esp_idf_svc::{
    http::{
        client::{Configuration, EspHttpConnection},
        Method,
    },
    nvs::EspDefaultNvsPartition,
    sys,
};

use crate::{claude, codex};

#[derive(Clone, Copy)]
pub struct Window {
    pub used: u32,
    pub resets: Option<Instant>,
}

#[derive(Clone, Copy)]
pub struct Reading {
    pub session: Option<Window>,
    pub weekly: Option<Window>,
    pub at: Instant,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Problem {
    NotSignedIn,
    SignInAgain,
    Offline,
}

/// A failed poll: what the screen shows, and why for `GET /status`.
pub struct Fail {
    pub problem: Problem,
    pub why: String,
}

impl Fail {
    pub fn new(problem: Problem, why: impl Into<String>) -> Self {
        let why = why.into();
        log::warn!("{why}");
        Self { problem, why }
    }

    pub fn offline(why: impl Into<String>) -> Self {
        Self::new(Problem::Offline, why)
    }

    pub fn http(what: &str, status: u16, body: &[u8]) -> Self {
        Self::offline(format!("{what} HTTP {status}: {}", snippet(body)))
    }
}

pub fn snippet(body: &[u8]) -> String {
    String::from_utf8_lossy(&body[..body.len().min(160)]).into_owned()
}

/// One usage source, as last polled.
#[derive(Default)]
pub struct Feed {
    pub reading: Option<Reading>,
    pub problem: Option<Problem>,
    pub error: String,
}

impl Feed {
    fn update(&mut self, result: Result<Reading, Fail>) {
        match result {
            Ok(reading) => {
                self.reading = Some(reading);
                self.problem = None;
                self.error.clear();
            }
            Err(fail) => {
                self.problem = Some(fail.problem);
                self.error = fail.why;
            }
        }
    }

    fn describe(&self, name: &str) -> String {
        let window = |label: &str, w: Option<Window>| match w {
            Some(w) => format!(
                "{name} {label}: {}% used, resets in {}s\n",
                w.used,
                w.resets.map_or(0, |r| r.saturating_duration_since(Instant::now()).as_secs())
            ),
            None => format!("{name} {label}: n/a\n"),
        };
        let mut out = format!("{name} problem: {:?}\n{name} last error: {}\n", self.problem, self.error);
        match &self.reading {
            Some(r) => {
                out += &format!("{name} reading age: {}s\n", r.at.elapsed().as_secs());
                out += &window("session", r.session);
                out += &window("weekly", r.weekly);
            }
            None => out += &format!("{name} reading: none\n"),
        }
        out
    }
}

#[derive(Default)]
pub struct State {
    /// Set once Wi‑Fi is up; `None` means still connecting.
    pub ip: Option<Ipv4Addr>,
    pub codex: Feed,
    pub claude: Feed,
    /// Bumped after every poll so the draw loop knows to redraw.
    pub version: u32,
}

pub type Shared = Arc<Mutex<State>>;

impl State {
    /// Plain-text summary for `GET /status`.
    pub fn describe(&self) -> String {
        let mut out = format!(
            "uptime: {}s\nlast redraw: {}s ago ({})\npolls: {}\n",
            crate::display::uptime_s(),
            crate::display::uptime_s() - crate::display::LAST_DRAW_S.load(Ordering::Relaxed),
            if crate::display::SHOWING_CLAUDE.load(Ordering::Relaxed) { "Claude" } else { "Codex" },
            self.version,
        );
        out += &crate::net::wifi_summary();
        out += &crate::backlight::summary();
        out += &self.codex.describe("codex");
        out += &self.claude.describe("claude");
        out
    }
}

/// Reasons to poll before the next scheduled time.
pub enum Wake {
    /// New credentials arrived on `POST /auth` or `POST /auth/claude`.
    NewAuth,
    /// Wi‑Fi came back after a drop.
    Online,
}

/// Polls forever: Codex every minute, Claude every five (its endpoint throttles harder).
/// One after the other, so only one TLS session ever holds RAM.
pub fn run(nvs: EspDefaultNvsPartition, state: Shared, wake: Receiver<Wake>) {
    let mut codex_access = None;
    let mut claude_due = Instant::now();
    loop {
        if PAUSED_UNTIL.lock().unwrap().is_some_and(|until| Instant::now() < until) {
            if let Ok(Wake::NewAuth) = wake.recv_timeout(Duration::from_secs(5)) {
                codex_access = None;
            }
            continue;
        }
        let codex = codex::poll(&nvs, &mut codex_access);
        let claude = (Instant::now() >= claude_due).then(|| {
            claude_due = Instant::now() + claude::POLL;
            claude::poll(&nvs)
        });
        {
            let mut s = state.lock().unwrap();
            s.codex.update(codex);
            if let Some(claude) = claude {
                s.claude.update(claude);
            }
            s.version += 1;
        }
        match wake.recv_timeout(codex::POLL) {
            Ok(Wake::NewAuth) => (codex_access, claude_due) = (None, Instant::now()),
            Ok(Wake::Online) => claude_due = Instant::now(),
            Err(_) => {}
        }
    }
}

/// Set by `/debug/pause` so a reading can go stale while Wi‑Fi stays up.
static PAUSED_UNTIL: Mutex<Option<Instant>> = Mutex::new(None);

pub fn pause(secs: u64) {
    *PAUSED_UNTIL.lock().unwrap() = Some(Instant::now() + Duration::from_secs(secs));
}

/// Turns a Unix time into an `Instant`, once SNTP has set the clock.
pub fn instant_at(unix: u64) -> Option<Instant> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    (now > 1_700_000_000).then(|| Instant::now() + Duration::from_secs(unix.saturating_sub(now)))
}

pub fn request(
    method: Method,
    url: &str,
    headers: &[(&str, &str)],
    body: Option<&[u8]>,
) -> Result<(u16, Vec<u8>), Box<dyn Error>> {
    let mut conn = EspHttpConnection::new(&Configuration {
        timeout: Some(Duration::from_secs(20)),
        crt_bundle_attach: Some(sys::esp_crt_bundle_attach),
        buffer_size: Some(4096),
        // The ChatGPT access token alone is ~1.8 KB; the default 512 B can't hold the header.
        buffer_size_tx: Some(4096),
        ..Default::default()
    })?;
    let length = body.map(|b| b.len().to_string());
    let mut all: Vec<(&str, &str)> = headers.to_vec();
    if let Some(length) = &length {
        all.push(("Content-Length", length));
    }
    conn.initiate_request(method, url, &all)?;
    if let Some(body) = body {
        conn.write_all(body)?;
    }
    conn.initiate_response()?;
    let status = conn.status();
    let mut out = Vec::new();
    let mut chunk = [0; 1024];
    loop {
        let n = conn.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        out.extend_from_slice(&chunk[..n]);
    }
    Ok((status, out))
}
