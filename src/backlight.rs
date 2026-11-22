use std::{
    sync::atomic::{AtomicU32, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use esp_idf_svc::{
    hal::{
        gpio::OutputPin,
        ledc::{config::TimerConfig, LedcChannel, LedcDriver, LedcTimer, LedcTimerDriver},
        peripheral::Peripheral,
        units::FromValueType,
    },
    sys::EspError,
};

const DAY: u32 = 70;
const NIGHT: u32 = 10;
/// Night runs from 22:00 to 07:00 Singapore time (UTC+8, no daylight saving).
const NIGHT_FROM: u64 = 22;
const NIGHT_UNTIL: u64 = 7;
const UTC_OFFSET_S: u64 = 8 * 3600;

/// Percent set by `/debug/brightness`; 0 means follow the clock.
pub static OVERRIDE: AtomicU32 = AtomicU32::new(0);
static LEVEL: AtomicU32 = AtomicU32::new(0);
static MODE: AtomicU32 = AtomicU32::new(0);
const MODES: [&str; 4] = ["no clock", "day", "night", "override"];

pub struct Backlight {
    pwm: LedcDriver<'static>,
}

impl Backlight {
    pub fn new<C, T>(
        channel: impl Peripheral<P = C> + 'static,
        timer: impl Peripheral<P = T> + 'static,
        pin: impl Peripheral<P = impl OutputPin> + 'static,
    ) -> Result<Self, EspError>
    where
        C: LedcChannel<SpeedMode = <T as LedcTimer>::SpeedMode>,
        T: LedcTimer + 'static,
    {
        let timer = LedcTimerDriver::new(timer, &TimerConfig::new().frequency(5.kHz().into()))?;
        let mut pwm = LedcDriver::new(channel, timer, pin)?;
        pwm.set_duty(0)?;
        Ok(Self { pwm })
    }

    /// Sets the level the clock (or an override) calls for. Cheap to call often.
    pub fn update(&mut self) -> Result<(), EspError> {
        let (percent, mode) = match (OVERRIDE.load(Ordering::Relaxed), local_hour()) {
            (p, _) if p > 0 => (p.min(100), 3),
            (_, None) => (DAY, 0),
            (_, Some(h)) if h >= NIGHT_FROM || h < NIGHT_UNTIL => (NIGHT, 2),
            _ => (DAY, 1),
        };
        MODE.store(mode, Ordering::Relaxed);
        if LEVEL.swap(percent, Ordering::Relaxed) != percent {
            self.pwm.set_duty(self.pwm.get_max_duty() * percent / 100)?;
        }
        Ok(())
    }
}

/// Local hour once SNTP has set the clock; `None` before that.
fn local_hour() -> Option<u64> {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    (secs > 1_700_000_000).then(|| (secs + UTC_OFFSET_S) / 3600 % 24)
}

pub fn summary() -> String {
    format!(
        "brightness: {}% ({})\n",
        LEVEL.load(Ordering::Relaxed),
        MODES[MODE.load(Ordering::Relaxed) as usize]
    )
}
