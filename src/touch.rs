use std::time::{Duration, Instant};

use esp_idf_svc::sys::{self, esp, EspError};

// The SmallTV Pro top electrode uses GPIO32 (T9 on classic ESP32).
const PAD: sys::touch_pad_t = sys::touch_pad_t_TOUCH_PAD_NUM9;

pub struct TouchPad;

impl TouchPad {
    pub fn new() -> Result<Self, EspError> {
        // The legacy ESP-IDF driver owns the touch peripheral for the app lifetime.
        unsafe {
            esp!(sys::touch_pad_init())?;
            esp!(sys::touch_pad_set_fsm_mode(
                sys::touch_fsm_mode_t_TOUCH_FSM_MODE_SW
            ))?;
            esp!(sys::touch_pad_config(PAD, 0))?;
        }
        Ok(Self)
    }

    pub fn read(&self) -> Result<u16, EspError> {
        let mut value = 0;
        // The value lives until the synchronous measurement returns.
        unsafe { esp!(sys::touch_pad_read(PAD, &mut value))? };
        Ok(value)
    }
}

pub struct Button {
    baseline: u32,
    pressed_at: Option<Instant>,
    consecutive: u8,
}

impl Button {
    pub fn new(baseline: u16) -> Self {
        Self {
            baseline: u32::from(baseline),
            pressed_at: None,
            consecutive: 0,
        }
    }

    pub fn update(&mut self, sample: u16) -> bool {
        let sample = u32::from(sample);
        let changed = if self.pressed_at.is_some() {
            sample * 100 >= self.baseline * 90
        } else {
            sample * 100 <= self.baseline * 80
        };
        if !changed {
            self.consecutive = 0;
            return false;
        }
        self.consecutive += 1;
        if self.consecutive < 3 {
            return false;
        }
        self.consecutive = 0;
        if let Some(pressed_at) = self.pressed_at.take() {
            pressed_at.elapsed() < Duration::from_millis(600)
        } else {
            self.pressed_at = Some(Instant::now());
            false
        }
    }
}
