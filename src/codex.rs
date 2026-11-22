use std::{
    error::Error,
    net::Ipv4Addr,
    sync::{atomic::Ordering, mpsc::Receiver, Arc, Mutex},
    time::{Duration, Instant},
};

use esp_idf_svc::{
    http::{
        client::{Configuration, EspHttpConnection},
        Method,
    },
    nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault},
    sys,
};
use serde::Deserialize;

const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const POLL: Duration = Duration::from_secs(60);

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

/// Reasons to poll before the next scheduled minute.
pub enum Wake {
    /// New credentials arrived on `POST /auth`; drop the cached access token.
    NewAuth,
    /// Wi‑Fi came back after a drop.
    Online,
}

#[derive(Default)]
pub struct State {
    /// Set once Wi‑Fi is up; `None` means still connecting.
    pub ip: Option<Ipv4Addr>,
    pub reading: Option<Reading>,
    pub problem: Option<Problem>,
    pub version: u32,
}

pub type Shared = Arc<Mutex<State>>;

impl State {
    /// Plain-text summary for `GET /status`.
    pub fn describe(&self) -> String {
        let window = |name: &str, w: Option<Window>| match w {
            Some(w) => format!(
                "{name}: {}% used, resets in {}s\n",
                w.used,
                w.resets.map_or(0, |r| r.saturating_duration_since(Instant::now()).as_secs())
            ),
            None => format!("{name}: n/a\n"),
        };
        let mut out = format!(
            "uptime: {}s\nlast redraw: {}s ago ({})\npolls: {}\nproblem: {:?}\nlast error: {}\n",
            crate::display::uptime_s(),
            crate::display::uptime_s() - crate::display::LAST_DRAW_S.load(Ordering::Relaxed),
            if crate::display::SHOWING_CLAUDE.load(Ordering::Relaxed) { "Claude" } else { "Codex" },
            self.version,
            self.problem,
            LAST_ERROR.lock().unwrap()
        );
        out += &crate::net::wifi_summary();
        match &self.reading {
            Some(r) => {
                out += &format!("reading age: {}s\n", r.at.elapsed().as_secs());
                out += &window("session", r.session);
                out += &window("weekly", r.weekly);
            }
            None => out += "reading: none\n",
        }
        out
    }
}

/// The shape of Codex's `auth.json`, as pushed from the computer to `POST /auth`.
#[derive(Deserialize)]
pub struct AuthFile {
    tokens: AuthTokens,
}

#[derive(Deserialize)]
struct AuthTokens {
    refresh_token: String,
    account_id: String,
}

pub fn save_auth(nvs: &EspDefaultNvsPartition, body: &[u8]) -> Result<(), Box<dyn Error>> {
    let auth: AuthFile = serde_json::from_slice(body)?;
    let mut store = open(nvs)?;
    store.set_str("refresh", &auth.tokens.refresh_token)?;
    store.set_str("account", &auth.tokens.account_id)?;
    Ok(())
}

/// Polls usage forever. A message on `wake` polls immediately.
pub fn run(nvs: EspDefaultNvsPartition, state: Shared, wake: Receiver<Wake>) {
    let mut access: Option<String> = None;
    loop {
        if PAUSED_UNTIL.lock().unwrap().is_some_and(|until| Instant::now() < until) {
            if let Ok(Wake::NewAuth) = wake.recv_timeout(Duration::from_secs(5)) {
                access = None;
            }
            continue;
        }
        let result = poll(&nvs, &mut access);
        {
            let mut s = state.lock().unwrap();
            match result {
                Ok(reading) => {
                    s.reading = Some(reading);
                    s.problem = None;
                    LAST_ERROR.lock().unwrap().clear();
                }
                Err(problem) => s.problem = Some(problem),
            }
            s.version += 1;
        }
        if let Ok(Wake::NewAuth) = wake.recv_timeout(POLL) {
            access = None;
        }
    }
}

fn poll(nvs: &EspDefaultNvsPartition, access: &mut Option<String>) -> Result<Reading, Problem> {
    let store = open(nvs).map_err(|e| offline(format!("nvs: {e}")))?;
    let mut buf = [0; 128];
    let account = match store.get_str("account", &mut buf) {
        Ok(Some(account)) => account.to_owned(),
        _ => return Err(Problem::NotSignedIn),
    };
    drop(store);

    for attempt in 0..2 {
        if access.is_none() {
            *access = Some(refresh(nvs)?);
        }
        let token = access.as_deref().unwrap();
        let headers = [
            ("Authorization", &*format!("Bearer {token}")),
            ("ChatGPT-Account-Id", &*account),
            ("User-Agent", "codex-cli"),
        ];
        match request(Method::Get, USAGE_URL, &headers, None) {
            Ok((200, body)) => return parse_usage(&body).ok_or(Problem::Offline),
            Ok((401 | 403, _)) if attempt == 0 => *access = None,
            Ok((401 | 403, _)) => return Err(Problem::SignInAgain),
            Ok((status, body)) => {
                note(format!("usage HTTP {status}: {}", String::from_utf8_lossy(&body[..body.len().min(160)])));
                return Err(Problem::Offline);
            }
            Err(err) => {
                note(format!("usage request failed: {err}"));
                return Err(Problem::Offline);
            }
        }
    }
    Err(Problem::Offline)
}

/// Exchanges the stored refresh token for an access token. OpenAI rotates refresh
/// tokens, so the new one is saved before anything else happens.
fn refresh(nvs: &EspDefaultNvsPartition) -> Result<String, Problem> {
    let mut store = open(nvs).map_err(|e| offline(format!("nvs: {e}")))?;
    let mut buf = vec![0; 2048];
    let refresh_token = match store.get_str("refresh", &mut buf) {
        Ok(Some(token)) => token.to_owned(),
        _ => return Err(Problem::NotSignedIn),
    };
    let body = serde_json::json!({
        "client_id": CLIENT_ID,
        "grant_type": "refresh_token",
        "refresh_token": refresh_token,
    })
    .to_string();
    let headers = [("Content-Type", "application/json")];
    match request(Method::Post, TOKEN_URL, &headers, Some(body.as_bytes())) {
        Ok((200, body)) => {
            #[derive(Deserialize)]
            struct Tokens {
                access_token: String,
                refresh_token: Option<String>,
            }
            let tokens: Tokens = serde_json::from_slice(&body).map_err(|e| offline(format!("token JSON: {e}")))?;
            if let Some(next) = tokens.refresh_token {
                store.set_str("refresh", &next).map_err(|e| offline(format!("save refresh: {e}")))?;
            }
            Ok(tokens.access_token)
        }
        Ok((400 | 401, body)) => {
            note(format!("token refresh rejected: {}", String::from_utf8_lossy(&body[..body.len().min(160)])));
            Err(Problem::SignInAgain)
        }
        Ok((status, body)) => {
            note(format!("token refresh HTTP {status}: {}", String::from_utf8_lossy(&body[..body.len().min(160)])));
            Err(Problem::Offline)
        }
        Err(err) => {
            note(format!("token refresh failed: {err}"));
            Err(Problem::Offline)
        }
    }
}

fn parse_usage(body: &[u8]) -> Option<Reading> {
    #[derive(Deserialize)]
    struct Usage {
        rate_limit: Option<Limits>,
    }
    #[derive(Deserialize)]
    struct Limits {
        primary_window: Option<Win>,
        secondary_window: Option<Win>,
    }
    #[derive(Deserialize)]
    struct Win {
        used_percent: f64,
        reset_after_seconds: Option<u64>,
    }
    let usage: Usage = serde_json::from_slice(body)
        .map_err(|err| note(format!("usage JSON: {err}")))
        .ok()?;
    let limits = usage.rate_limit?;
    let now = Instant::now();
    let window = |w: Win| Window {
        used: w.used_percent.round().clamp(0.0, 100.0) as u32,
        resets: w.reset_after_seconds.map(|s| now + Duration::from_secs(s)),
    };
    Some(Reading {
        session: limits.primary_window.map(window),
        weekly: limits.secondary_window.map(window),
        at: now,
    })
}

fn request(
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

/// Set by `/debug/pause` so a reading can go stale while Wi‑Fi stays up.
static PAUSED_UNTIL: Mutex<Option<Instant>> = Mutex::new(None);

pub fn pause(secs: u64) {
    *PAUSED_UNTIL.lock().unwrap() = Some(Instant::now() + Duration::from_secs(secs));
}

/// Last failure, shown by `GET /status` since the device has no serial console.
static LAST_ERROR: Mutex<String> = Mutex::new(String::new());

fn note(msg: String) {
    log::warn!("{msg}");
    *LAST_ERROR.lock().unwrap() = msg;
}

fn offline(msg: String) -> Problem {
    note(msg);
    Problem::Offline
}

fn open(nvs: &EspDefaultNvsPartition) -> Result<EspNvs<NvsDefault>, sys::EspError> {
    EspNvs::new(nvs.clone(), "codex", true)
}
