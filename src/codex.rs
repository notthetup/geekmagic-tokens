//! Codex usage from ChatGPT's internal quota endpoint, with the device's own rotating login.

use std::{
    error::Error,
    time::{Duration, Instant},
};

use esp_idf_svc::{
    http::Method,
    nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault},
    sys,
};
use serde::Deserialize;

use crate::usage::{request, Fail, Problem, Reading, Window};

const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const POLL: Duration = Duration::from_secs(60);

/// The shape of Codex's `auth.json`, as pushed from the computer to `POST /auth`.
#[derive(Deserialize)]
struct AuthFile {
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

/// Reads usage, refreshing the cached access token when it is missing or rejected.
pub fn poll(nvs: &EspDefaultNvsPartition, access: &mut Option<String>) -> Result<Reading, Fail> {
    let store = open(nvs).map_err(|e| Fail::offline(format!("nvs: {e}")))?;
    let mut buf = [0; 128];
    let account = match store.get_str("account", &mut buf) {
        Ok(Some(account)) => account.to_owned(),
        _ => return Err(Fail::new(Problem::NotSignedIn, "codex: not signed in")),
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
            Ok((200, body)) => return parse_usage(&body),
            Ok((401 | 403, _)) if attempt == 0 => *access = None,
            Ok((status @ (401 | 403), body)) => {
                return Err(Fail::new(Problem::SignInAgain, format!("usage HTTP {status}: {}", crate::usage::snippet(&body))))
            }
            Ok((status, body)) => return Err(Fail::http("usage", status, &body)),
            Err(err) => return Err(Fail::offline(format!("usage request failed: {err}"))),
        }
    }
    unreachable!()
}

/// Exchanges the stored refresh token for an access token. OpenAI rotates refresh
/// tokens, so the new one is saved before anything else happens.
fn refresh(nvs: &EspDefaultNvsPartition) -> Result<String, Fail> {
    let mut store = open(nvs).map_err(|e| Fail::offline(format!("nvs: {e}")))?;
    let mut buf = vec![0; 2048];
    let refresh_token = match store.get_str("refresh", &mut buf) {
        Ok(Some(token)) => token.to_owned(),
        _ => return Err(Fail::new(Problem::NotSignedIn, "codex: not signed in")),
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
            let tokens: Tokens =
                serde_json::from_slice(&body).map_err(|e| Fail::offline(format!("token JSON: {e}")))?;
            if let Some(next) = tokens.refresh_token {
                store.set_str("refresh", &next).map_err(|e| Fail::offline(format!("save refresh: {e}")))?;
            }
            Ok(tokens.access_token)
        }
        Ok((400 | 401, body)) => Err(Fail::new(
            Problem::SignInAgain,
            format!("token refresh rejected: {}", crate::usage::snippet(&body)),
        )),
        Ok((status, body)) => Err(Fail::http("token refresh", status, &body)),
        Err(err) => Err(Fail::offline(format!("token refresh failed: {err}"))),
    }
}

fn parse_usage(body: &[u8]) -> Result<Reading, Fail> {
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
    let usage: Usage = serde_json::from_slice(body).map_err(|e| Fail::offline(format!("usage JSON: {e}")))?;
    let limits = usage.rate_limit.ok_or_else(|| Fail::offline("usage JSON: no rate_limit"))?;
    let now = Instant::now();
    let window = |w: Win| Window {
        used: w.used_percent.round().clamp(0.0, 100.0) as u32,
        resets: w.reset_after_seconds.map(|s| now + Duration::from_secs(s)),
    };
    Ok(Reading {
        session: limits.primary_window.map(window),
        weekly: limits.secondary_window.map(window),
        at: now,
    })
}

fn open(nvs: &EspDefaultNvsPartition) -> Result<EspNvs<NvsDefault>, sys::EspError> {
    EspNvs::new(nvs.clone(), "codex", true)
}
