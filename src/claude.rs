//! Claude subscription usage from the internal endpoint behind Claude Code's `/usage`.
//!
//! The device holds a 1-year access token minted by `scripts/claude-login.sh` (scope `user:profile`)
//! and never refreshes it: a refresh would swap it for an 8-hour token.

use std::{error::Error, time::{Duration, Instant}};

use esp_idf_svc::{
    http::Method,
    nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault},
    sys,
};
use serde::Deserialize;

use crate::usage::{instant_at, request, snippet, Fail, Problem, Reading, Window};

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
/// Without a versioned Claude Code User-Agent the endpoint throttles to about one request an hour.
const USER_AGENT: &str = "claude-code/2.1.330";
pub const POLL: Duration = Duration::from_secs(300);

/// Stores the `access_token` from the token JSON pushed to `POST /auth/claude`.
pub fn save_auth(nvs: &EspDefaultNvsPartition, body: &[u8]) -> Result<(), Box<dyn Error>> {
    #[derive(Deserialize)]
    struct Token {
        access_token: String,
    }
    let token: Token = serde_json::from_slice(body)?;
    open(nvs)?.set_str("access", &token.access_token)?;
    Ok(())
}

pub fn poll(nvs: &EspDefaultNvsPartition) -> Result<Reading, Fail> {
    let store = open(nvs).map_err(|e| Fail::offline(format!("nvs: {e}")))?;
    let mut buf = [0; 512];
    let token = match store.get_str("access", &mut buf) {
        Ok(Some(token)) => format!("Bearer {token}"),
        _ => return Err(Fail::new(Problem::NotSignedIn, "claude: not signed in")),
    };
    drop(store);
    let headers = [
        ("Authorization", token.as_str()),
        ("anthropic-beta", "oauth-2025-04-20"),
        ("User-Agent", USER_AGENT),
    ];
    match request(Method::Get, USAGE_URL, &headers, None) {
        Ok((200, body)) => parse_usage(&body),
        Ok((status @ (401 | 403), body)) => {
            Err(Fail::new(Problem::SignInAgain, format!("claude usage HTTP {status}: {}", snippet(&body))))
        }
        Ok((status, body)) => Err(Fail::http("claude usage", status, &body)),
        Err(err) => Err(Fail::offline(format!("claude usage request failed: {err}"))),
    }
}

fn parse_usage(body: &[u8]) -> Result<Reading, Fail> {
    #[derive(Deserialize)]
    struct Usage {
        five_hour: Option<Win>,
        seven_day: Option<Win>,
    }
    #[derive(Deserialize)]
    struct Win {
        utilization: f64,
        resets_at: Option<String>,
    }
    let usage: Usage =
        serde_json::from_slice(body).map_err(|e| Fail::offline(format!("claude usage JSON: {e}")))?;
    let window = |w: Win| Window {
        used: w.utilization.round().clamp(0.0, 100.0) as u32,
        resets: w.resets_at.as_deref().and_then(unix_time).and_then(instant_at),
    };
    Ok(Reading {
        session: usage.five_hour.map(window),
        weekly: usage.seven_day.map(window),
        at: Instant::now(),
    })
}

/// Parses an RFC 3339 time like `2026-11-22T09:49:59.877828+00:00` into Unix seconds.
fn unix_time(s: &str) -> Option<u64> {
    let num = |range: std::ops::Range<usize>| s.get(range)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    // Days since 1970-01-01 (Howard Hinnant's days_from_civil).
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((mo + 9) % 12) + 2) / 5 + d - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    // UTC offset at the end: `Z`, or `+hh:mm` / `-hh:mm`.
    let offset = match s.rfind(['+', '-']).filter(|&i| i > 18) {
        Some(i) => {
            let mins = num(i + 1..i + 3)? * 60 + num(i + 4..i + 6)?;
            if s.as_bytes()[i] == b'-' { -mins * 60 } else { mins * 60 }
        }
        None => 0,
    };
    u64::try_from(days * 86_400 + h * 3600 + mi * 60 + sec - offset).ok()
}

fn open(nvs: &EspDefaultNvsPartition) -> Result<EspNvs<NvsDefault>, sys::EspError> {
    EspNvs::new(nvs.clone(), "claude", true)
}
