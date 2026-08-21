// SPDX-License-Identifier: GPL-3.0-or-later

#[cfg(target_os = "linux")]
mod linux {
    use display_backends::{
        set_source_pixel, Backend, Display, SOURCE_FRAMEBUFFER_SIZE, SOURCE_HEIGHT, SOURCE_WIDTH,
    };
    use embedded_graphics::{
        mono_font::{ascii::FONT_10X20, ascii::FONT_6X10, MonoTextStyle},
        pixelcolor::BinaryColor,
        prelude::*,
        primitives::{Circle, PrimitiveStyle, Rectangle},
        text::{Alignment, Text},
    };
    use gpiocdev::{line::Value, Request};
    use std::{error::Error, fs::OpenOptions, os::fd::AsRawFd, time::Instant};

    struct TrezorFramebuffer([u8; SOURCE_FRAMEBUFFER_SIZE]);

    impl TrezorFramebuffer {
        fn new() -> Self {
            Self([0; SOURCE_FRAMEBUFFER_SIZE])
        }

        fn bytes(&self) -> &[u8; SOURCE_FRAMEBUFFER_SIZE] {
            &self.0
        }
    }

    impl OriginDimensions for TrezorFramebuffer {
        fn size(&self) -> Size {
            Size::new(SOURCE_WIDTH as u32, SOURCE_HEIGHT as u32)
        }
    }

    impl DrawTarget for TrezorFramebuffer {
        type Color = BinaryColor;
        type Error = core::convert::Infallible;

        fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
        where
            I: IntoIterator<Item = Pixel<Self::Color>>,
        {
            for Pixel(point, color) in pixels {
                if point.x >= 0 && point.y >= 0 {
                    set_source_pixel(
                        &mut self.0,
                        point.x as usize,
                        point.y as usize,
                        color == BinaryColor::On,
                    );
                }
            }
            Ok(())
        }

        fn clear(&mut self, color: Self::Color) -> Result<(), Self::Error> {
            self.0.fill(if color == BinaryColor::On { 0xff } else { 0 });
            Ok(())
        }
    }

    fn backend(value: &str) -> Result<Backend, Box<dyn Error>> {
        match value {
            "ssd1306-i2c" => Ok(Backend::Ssd1306I2c),
            "sh1106-i2c" => Ok(Backend::Sh1106I2c),
            "sh1106-spi" => Ok(Backend::Sh1106Spi),
            "st7789-spi" => Ok(Backend::St7789Spi),
            _ => Err(format!("unsupported backend {value}").into()),
        }
    }

    fn draw_scene(framebuffer: &mut TrezorFramebuffer, frame: u32) {
        framebuffer.clear(BinaryColor::Off).unwrap();
        Rectangle::new(Point::new(0, 0), Size::new(128, 64))
            .into_styled(PrimitiveStyle::with_stroke(BinaryColor::On, 1))
            .draw(framebuffer)
            .unwrap();
        Text::with_alignment(
            "RUST DISPLAY",
            Point::new(64, 17),
            MonoTextStyle::new(&FONT_6X10, BinaryColor::On),
            Alignment::Center,
        )
        .draw(framebuffer)
        .unwrap();
        Text::with_alignment(
            "BACKENDS",
            Point::new(64, 38),
            MonoTextStyle::new(&FONT_10X20, BinaryColor::On),
            Alignment::Center,
        )
        .draw(framebuffer)
        .unwrap();
        let x = 8 + (frame % 108) as i32;
        Circle::new(Point::new(x, 49), 8)
            .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
            .draw(framebuffer)
            .unwrap();
    }

    pub fn run() -> Result<(), Box<dyn Error>> {
        let args: Vec<String> = std::env::args().collect();
        if args.len() < 2 || args.len() > 4 {
            return Err(format!(
                "usage: {} BACKEND [bus-device [gpiochip]]",
                args.first()
                    .map(String::as_str)
                    .unwrap_or("display-backends-demo")
            )
            .into());
        }
        let backend = backend(&args[1])?;
        let default_bus = if backend.uses_spi() {
            "/dev/spidev0.0"
        } else {
            "/dev/i2c-1"
        };
        let bus_path = args.get(2).map(String::as_str).unwrap_or(default_bus);
        let gpio_path = args.get(3).map(String::as_str).unwrap_or("/dev/gpiochip0");

        let bus = OpenOptions::new().read(true).write(true).open(bus_path)?;
        let gpio = match backend {
            Backend::Ssd1306I2c => None,
            Backend::Sh1106I2c => Some(
                Request::builder()
                    .on_chip(gpio_path)
                    .with_consumer("display-backends-demo-control")
                    .with_lines(&[25])
                    .as_output(Value::Inactive)
                    .request()?,
            ),
            Backend::Sh1106Spi => Some(
                Request::builder()
                    .on_chip(gpio_path)
                    .with_consumer("display-backends-demo-control")
                    .with_lines(&[24, 25])
                    .as_output(Value::Inactive)
                    .request()?,
            ),
            Backend::St7789Spi => Some(
                Request::builder()
                    .on_chip(gpio_path)
                    .with_consumer("display-backends-demo-control")
                    .with_lines(&[25, 27, 24])
                    .as_output(Value::Inactive)
                    .request()?,
            ),
        };
        let control_fd = gpio.as_ref().map(AsRawFd::as_raw_fd);
        let mut display = Display::from_raw_fds(backend, bus.as_raw_fd(), control_fd)?;
        let mut framebuffer = TrezorFramebuffer::new();
        let start = Instant::now();
        for frame in 0..240 {
            draw_scene(&mut framebuffer, frame);
            display.write_trezor_frame(framebuffer.bytes())?;
        }
        let elapsed = start.elapsed();
        println!(
            "Rendered 240 frames through {} in {:.2}s ({:.1} frames/s)",
            backend.name(),
            elapsed.as_secs_f64(),
            240.0 / elapsed.as_secs_f64()
        );
        println!("Press Enter to clear and release the display.");
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        display.shutdown()?;
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn main() {
    if let Err(error) = linux::run() {
        eprintln!("display-backends-demo: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("display-backends-demo requires Linux");
    std::process::exit(1);
}
