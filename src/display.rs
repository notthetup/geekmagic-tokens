use std::{thread, time::Duration};

use embedded_graphics::{pixelcolor::Rgb565, prelude::*};
use embedded_hal::{digital::OutputPin, spi::SpiDevice};

use crate::{codex::State, ui::{self, Screen}};

const WIDTH: usize = 240;
const HEIGHT: usize = 240;
const ROWS: usize = 8;

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

    pub fn draw(&mut self, screen: Screen, codex: &State, ip: &str) -> Result<(), Box<dyn std::error::Error>> {
        self.command(0x2a, &[0, 0, 0, (WIDTH - 1) as u8])?;
        for y in (0..HEIGHT).step_by(ROWS) {
            self.strip.y = y as i32;
            self.strip.pixels.fill(0);
            ui::draw(&mut self.strip, screen, codex, ip)?;
            self.command(0x2b, &[0, y as u8, 0, (y + ROWS - 1) as u8])?;
            self.command(0x2c, &[])?;
            self.dc.set_high()?;
            self.spi.write(&self.strip.pixels)?;
        }
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
