mod backlight;
mod claude;
mod codex;
mod display;
mod net;
mod touch;
mod ui;
mod usage;

use std::{
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use embedded_hal::spi::MODE_3;
use esp_idf_svc::{
    hal::{
        gpio::{AnyIOPin, PinDriver},
        peripherals::Peripherals,
        spi::{config, Dma, SpiDeviceDriver},
        units::FromValueType,
    },
    nvs::EspDefaultNvsPartition,
    sys,
};

use display::Display;
use touch::{Button, TouchPad};
use ui::Screen;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    net::mark_valid();

    let peripherals = Peripherals::take()?;
    let pins = peripherals.pins;
    // SmallTV Pro v02-22. GPIO3 becomes the display chip select after boot.
    let spi = SpiDeviceDriver::new_single(
        peripherals.spi2,
        pins.gpio18,
        pins.gpio23,
        Option::<AnyIOPin>::None,
        Some(pins.gpio3),
        &config::DriverConfig::new().dma(Dma::Auto(3840)),
        &config::Config::new()
            .baudrate(20.MHz().into())
            .data_mode(MODE_3),
    )?;
    let mut reset = PinDriver::output(pins.gpio4)?;
    let mut backlight =
        backlight::Backlight::new(peripherals.ledc.channel0, peripherals.ledc.timer0, pins.gpio21)?;
    reset.set_low()?;
    thread::sleep(Duration::from_millis(20));
    reset.set_high()?;
    thread::sleep(Duration::from_millis(120));

    // Start calibrating with the backlight in its normal operating state.
    let mut display = Display::new(spi, PinDriver::output(pins.gpio2)?)?;
    let mut screen = Screen::Codex;
    let codex = usage::Shared::default();
    display.draw(screen, &codex.lock().unwrap())?;
    backlight.update()?;
    let touch = TouchPad::new()?;
    let mut samples = [0; 21];
    for sample in &mut samples {
        *sample = touch.read()?;
        thread::sleep(Duration::from_millis(15));
    }
    samples.sort_unstable();
    let mut button = Button::new(samples[samples.len() / 2]);

    // Wi‑Fi and polling share one background thread so the button works while we connect.
    // Credentials are baked in at build time from .env.
    if let (Some(ssid), Some(password)) = (option_env!("WIFI_SSID"), option_env!("WIFI_PASS")) {
        let (nvs, state, modem) = (EspDefaultNvsPartition::take()?, codex.clone(), peripherals.modem);
        thread::Builder::new().stack_size(24 * 1024).spawn(move || {
            let (wake, woken) = mpsc::channel();
            match net::start(modem, ssid, password, nvs.clone(), wake, state.clone()) {
                // `run` never returns, so `_net` keeps Wi‑Fi and the HTTP server alive.
                Ok(_net) => usage::run(nvs, state, woken),
                Err(err) => log::error!("Wi-Fi unavailable: {err}"),
            }
        })?;
    }

    // Redraw on a tap, on new data, and every 30 s so countdowns tick.
    let mut drawn = (u32::MAX, Instant::now());
    loop {
        // Errors here must not end `main`: the screen would freeze while Wi‑Fi and HTTP live on.
        let tapped = touch.read().is_ok_and(|sample| button.update(sample));
        if tapped {
            screen = screen.toggle();
        }
        let state = codex.lock().unwrap();
        if tapped || state.version != drawn.0 || drawn.1.elapsed() > Duration::from_secs(30) {
            if let Err(err) = display.draw(screen, &state) {
                log::error!("draw failed: {err}");
            }
            drawn = (state.version, Instant::now());
            if let Err(err) = backlight.update() {
                log::error!("backlight: {err}");
            }
        }
        drop(state);
        thread::sleep(Duration::from_millis(15));
    }
}
