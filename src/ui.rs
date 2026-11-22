use std::time::{Duration, Instant};

use embedded_graphics::{
    image::{Image, ImageRaw},
    mono_font::{
        ascii::{FONT_10X20, FONT_8X13},
        MonoTextStyle,
    },
    pixelcolor::{raw::BigEndian, Rgb565},
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
    text::{Alignment, Baseline, Text},
};

use crate::codex::{Problem, State, Window};

const LOGO: u32 = 32;
// 32×32 big-endian RGB565, made by scripts/logo.swift from assets/*.svg.
const CODEX_LOGO: &[u8] = include_bytes!("../assets/codex.raw");
const CLAUDE_CODE_LOGO: &[u8] = include_bytes!("../assets/claude-code.raw");

#[derive(Clone, Copy)]
pub enum Screen {
    Codex,
    Claude,
}

impl Screen {
    pub fn toggle(self) -> Self {
        match self {
            Self::Codex => Self::Claude,
            Self::Claude => Self::Codex,
        }
    }
}

pub fn draw<D>(target: &mut D, screen: Screen, codex: &State, ip: &str) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb565>,
{
    let heading = MonoTextStyle::new(&FONT_10X20, Rgb565::WHITE);
    let (title, logo) = match screen {
        Screen::Codex => ("Codex", CODEX_LOGO),
        Screen::Claude => ("Claude Code", CLAUDE_CODE_LOGO),
    };
    Text::with_baseline(title, Point::new(16, 19), heading, Baseline::Top).draw(target)?;
    let logo = ImageRaw::<Rgb565, BigEndian>::new(logo, LOGO);
    Image::new(&logo, Point::new(224 - LOGO as i32, 13)).draw(target)?;

    match screen {
        Screen::Codex => match (&codex.reading, codex.problem) {
            (Some(reading), problem) => {
                let now = Instant::now();
                usage_bar(target, 62, "Session", reading.session, now)?;
                usage_bar(target, 129, "Weekly", reading.weekly, now)?;
                let age = now - reading.at;
                let footer = match problem {
                    Some(Problem::SignInAgain) => String::from("Sign in again"),
                    _ if age > Duration::from_secs(180) => format!("Updated {} ago", span(Duration::from_secs(age.as_secs() / 60 * 60))),
                    _ => ip.to_owned(),
                };
                footnote(target, &footer)?;
            }
            (None, problem) => {
                message(
                    target,
                    match problem {
                        Some(Problem::NotSignedIn) => "Not signed in",
                        Some(Problem::SignInAgain) => "Sign in again",
                        Some(Problem::Offline) => "Can't reach Codex",
                        None => "Loading...",
                    },
                )?;
                footnote(target, ip)?;
            }
        },
        Screen::Claude => message(target, "Claude setup unavailable")?,
    }
    Ok(())
}

fn message<D: DrawTarget<Color = Rgb565>>(target: &mut D, text: &str) -> Result<(), D::Error> {
    let body = MonoTextStyle::new(&FONT_8X13, Rgb565::WHITE);
    Text::with_alignment(text, Point::new(120, 120), body, Alignment::Center).draw(target)?;
    Ok(())
}

fn footnote<D: DrawTarget<Color = Rgb565>>(target: &mut D, text: &str) -> Result<(), D::Error> {
    let muted = MonoTextStyle::new(&FONT_8X13, Rgb565::CSS_LIGHT_GRAY);
    Text::new(text, Point::new(16, 219), muted).draw(target)?;
    Ok(())
}

fn usage_bar<D>(
    target: &mut D,
    y: i32,
    label: &str,
    window: Option<Window>,
    now: Instant,
) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb565>,
{
    let text = MonoTextStyle::new(&FONT_8X13, Rgb565::WHITE);
    Text::new(label, Point::new(16, y), text).draw(target)?;
    let Some(window) = window else {
        Text::with_alignment("n/a", Point::new(223, y), text, Alignment::Right).draw(target)?;
        return Ok(());
    };
    Text::with_alignment(
        &format!("{}% used", window.used),
        Point::new(223, y),
        text,
        Alignment::Right,
    )
    .draw(target)?;

    let bar = Point::new(16, y + 9);
    Rectangle::new(bar, Size::new(208, 22))
        .into_styled(PrimitiveStyle::with_fill(Rgb565::new(7, 14, 9)))
        .draw(target)?;
    Rectangle::new(bar, Size::new(208 * window.used.min(100) / 100, 22))
        .into_styled(PrimitiveStyle::with_fill(Rgb565::new(8, 38, 24)))
        .draw(target)?;

    if let Some(resets) = window.resets {
        let reset = span(resets.saturating_duration_since(now));
        let badge_width = reset.len() as u32 * 8 + 10;
        Rectangle::new(
            Point::new(220 - badge_width as i32, y + 12),
            Size::new(badge_width, 16),
        )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::BLACK))
        .draw(target)?;
        Text::with_alignment(&reset, Point::new(214, y + 24), text, Alignment::Right)
            .draw(target)?;
    }
    Ok(())
}

/// Compact duration like the T3 chat badge: `4d 15h`, `4h 29m`, `12m`. Rounds up.
fn span(d: Duration) -> String {
    let m = d.as_secs().div_ceil(60);
    match (m / 1440, m / 60 % 24, m % 60) {
        (0, 0, m) => format!("{m}m"),
        (0, h, m) => format!("{h}h {m}m"),
        (d, h, _) => format!("{d}d {h}h"),
    }
}
