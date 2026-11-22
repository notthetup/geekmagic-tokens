mod codex;
mod display;
mod net;
mod touch;
mod ui;

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
    let mut backlight = PinDriver::output(pins.gpio21)?;
    backlight.set_low()?;
    reset.set_low()?;
    thread::sleep(Duration::from_millis(20));
    reset.set_high()?;
    thread::sleep(Duration::from_millis(120));

    // Start calibrating with the backlight in its normal operating state.
    let mut display = Display::new(spi, PinDriver::output(pins.gpio2)?)?;
    let mut screen = Screen::Codex;
    let codex = codex::Shared::default();
    let mut ip = String::from("Connecting...");
    display.draw(screen, &codex.lock().unwrap(), &ip)?;
    backlight.set_high()?;
    let touch = TouchPad::new()?;
    let mut samples = [0; 21];
    for sample in &mut samples {
        *sample = touch.read()?;
        thread::sleep(Duration::from_millis(15));
    }
    samples.sort_unstable();
    let mut button = Button::new(samples[samples.len() / 2]);

    // Wi‑Fi credentials are baked in at build time: WIFI_SSID=... WIFI_PASS=... cargo build
    let nvs = EspDefaultNvsPartition::take()?;
    let (wake, woken) = mpsc::channel();
    let _net = match (option_env!("WIFI_SSID"), option_env!("WIFI_PASS")) {
        (Some(ssid), Some(password)) => {
            match net::start(peripherals.modem, ssid, password, nvs.clone(), wake, codex.clone()) {
                Ok(net) => {
                    ip = net.ip.to_string();
                    let state = codex.clone();
                    thread::Builder::new()
                        .stack_size(24 * 1024)
                        .spawn(move || codex::run(nvs, state, woken))?;
                    Some(net)
                }
                Err(err) => {
                    log::error!("Wi-Fi unavailable: {err}");
                    ip = String::from("No Wi-Fi");
                    None
                }
            }
        }
        _ => None,
    };

    // Redraw on a tap, on new data, and every 30 s so countdowns tick.
    let mut drawn = (u32::MAX, Instant::now());
    loop {
        let tapped = button.update(touch.read()?);
        if tapped {
            screen = screen.toggle();
        }
        let state = codex.lock().unwrap();
        if tapped || state.version != drawn.0 || drawn.1.elapsed() > Duration::from_secs(30) {
            display.draw(screen, &state, &ip)?;
            drawn = (state.version, Instant::now());
        }
        drop(state);
        thread::sleep(Duration::from_millis(15));
    }
}
