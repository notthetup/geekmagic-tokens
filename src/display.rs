use std::{
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
    thread,
    time::Duration,
};

use embedded_graphics::{pixelcolor::Rgb565, prelude::*};
use embedded_hal::{digital::OutputPin, spi::SpiDevice};

use crate::{codex::State, ui::{self, Screen}};

const WIDTH: usize = 240;
const HEIGHT: usize = 240;
const ROWS: usize = 8;

/// Seconds since boot of the last full redraw and which screen it showed, for `GET /status`.
/// (No 64-bit atomics on Xtensa.)
pub static LAST_DRAW_S: AtomicU32 = AtomicU32::new(0);
pub static SHOWING_CLAUDE: AtomicBool = AtomicBool::new(false);

pub fn uptime_s() -> u32 {
    (unsafe { esp_idf_svc::sys::esp_timer_get_time() } / 1_000_000) as u32
}

pub struct Display<Spi, Dc> {
    spi: Spi,
    dc: Dc,
    strip: Strip,
}

impl<Spi, Dc> Display<Spi, Dc>
where
    Spi: SpiDevice<u8>,
    Dc: OutputPin,
    Spi::Error: std::error::Error + 'static,
    Dc::Error: std::error::Error + 'static,
{
    pub fn new(spi: Spi, dc: Dc) -> Result<Self, Box<dyn std::error::Error>> {
        let mut display = Self {
            spi,
            dc,
            strip: Strip {
                y: 0,
                pixels: vec![0; WIDTH * ROWS * 2],
            },
        };
        display.command(0x11, &[])?; // Sleep out
        thread::sleep(Duration::from_millis(120));
        display.command(0x3a, &[0x55])?; // RGB565
        display.command(0x36, &[0x00])?; // RGB, normal orientation
        display.command(0x21, &[])?; // Display inversion on
        display.command(0x29, &[])?; // Display on
        Ok(display)
    }

    pub fn draw(&mut self, screen: Screen, codex: &State) -> Result<(), Box<dyn std::error::Error>> {
        self.command(0x2a, &[0, 0, 0, (WIDTH - 1) as u8])?;
        for y in (0..HEIGHT).step_by(ROWS) {
            self.strip.y = y as i32;
            self.strip.pixels.fill(0);
            ui::draw(&mut self.strip, screen, codex)?;
            self.command(0x2b, &[0, y as u8, 0, (y + ROWS - 1) as u8])?;
            self.command(0x2c, &[])?;
            self.dc.set_high()?;
            self.spi.write(&self.strip.pixels)?;
        }
        LAST_DRAW_S.store(uptime_s(), Ordering::Relaxed);
        SHOWING_CLAUDE.store(matches!(screen, Screen::Claude), Ordering::Relaxed);
        Ok(())
    }

    fn command(&mut self, command: u8, data: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
        self.dc.set_low()?;
        self.spi.write(&[command])?;
        if !data.is_empty() {
            self.dc.set_high()?;
            self.spi.write(data)?;
        }
        Ok(())
    }
}

// Render eight rows at a time to leave RAM for TLS on the ESP32.
struct Strip {
    y: i32,
    pixels: Vec<u8>,
}

impl OriginDimensions for Strip {
    fn size(&self) -> Size {
        Size::new(WIDTH as u32, HEIGHT as u32)
    }
}

impl DrawTarget for Strip {
    type Color = Rgb565;
    type Error = std::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Rgb565>>,
    {
        for Pixel(Point { x, y }, color) in pixels {
            let row = y - self.y;
            if (0..WIDTH as i32).contains(&x) && (0..ROWS as i32).contains(&row) {
                let index = 2 * (row as usize * WIDTH + x as usize);
                self.pixels[index..index + 2].copy_from_slice(&color.into_storage().to_be_bytes());
            }
        }
        Ok(())
    }
}

/// Renders what the panel would show as a top-down 24-bit BMP, one strip at a time.
pub fn render_bmp<E>(
    screen: Screen,
    codex: &State,
    mut out: impl FnMut(&[u8]) -> Result<(), E>,
) -> Result<(), E> {
    let row = WIDTH * 3;
    let size = 54 + row * HEIGHT;
    let mut header = [0u8; 54];
    header[..2].copy_from_slice(b"BM");
    header[2..6].copy_from_slice(&(size as u32).to_le_bytes());
    header[10] = 54;
    header[14] = 40;
    header[18..22].copy_from_slice(&(WIDTH as i32).to_le_bytes());
    header[22..26].copy_from_slice(&(-(HEIGHT as i32)).to_le_bytes());
    header[26] = 1;
    header[28] = 24;
    out(&header)?;
    let mut strip = Strip { y: 0, pixels: vec![0; WIDTH * ROWS * 2] };
    let mut bgr = vec![0u8; row * ROWS];
    for y in (0..HEIGHT).step_by(ROWS) {
        strip.y = y as i32;
        strip.pixels.fill(0);
        let _ = ui::draw(&mut strip, screen, codex);
        for (px, rgb) in strip.pixels.chunks_exact(2).zip(bgr.chunks_exact_mut(3)) {
            let c = Rgb565::from(embedded_graphics::pixelcolor::raw::RawU16::new(u16::from_be_bytes([px[0], px[1]])));
            rgb.copy_from_slice(&[c.b() << 3, c.g() << 2, c.r() << 3]);
        }
        out(&bgr)?;
    }
    Ok(())
}
