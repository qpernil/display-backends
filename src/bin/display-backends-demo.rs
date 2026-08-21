// SPDX-License-Identifier: GPL-3.0-or-later

#[cfg(target_os = "linux")]
mod linux {
    use display_backends::{
        set_mono1_pixel, Backend, Display, FrameFormat, MONO1_FRAME_HEIGHT, MONO1_FRAME_SIZE,
        MONO1_FRAME_WIDTH, ST7789_FRAMEBUFFER_SIZE, ST7789_PANEL_HEIGHT, ST7789_PANEL_WIDTH,
    };
    use embedded_graphics::{
        mono_font::{ascii::FONT_10X20, ascii::FONT_6X10, MonoTextStyle},
        pixelcolor::{BinaryColor, Rgb565},
        prelude::*,
        primitives::{Circle, PrimitiveStyle, Rectangle},
        text::{Alignment, Text},
    };
    use gpiocdev_uapi::v2::{get_line, LineConfig, LineFlags, LineRequest, LineValues, Offsets};
    use std::{
        error::Error,
        fs::{File, OpenOptions},
        io,
        os::fd::AsRawFd,
        time::Instant,
    };

    struct MonoFramebuffer([u8; MONO1_FRAME_SIZE]);

    impl MonoFramebuffer {
        fn new() -> Self {
            Self([0; MONO1_FRAME_SIZE])
        }

        fn bytes(&self) -> &[u8; MONO1_FRAME_SIZE] {
            &self.0
        }
    }

    impl OriginDimensions for MonoFramebuffer {
        fn size(&self) -> Size {
            Size::new(MONO1_FRAME_WIDTH as u32, MONO1_FRAME_HEIGHT as u32)
        }
    }

    impl DrawTarget for MonoFramebuffer {
        type Color = BinaryColor;
        type Error = core::convert::Infallible;

        fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
        where
            I: IntoIterator<Item = Pixel<Self::Color>>,
        {
            for Pixel(point, color) in pixels {
                if point.x >= 0 && point.y >= 0 {
                    set_mono1_pixel(
                        &mut self.0,
                        FrameFormat::mono1_128x64(),
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

    struct ColorFramebuffer(Vec<u8>);

    impl ColorFramebuffer {
        fn new() -> Self {
            Self(vec![0; ST7789_FRAMEBUFFER_SIZE])
        }

        fn bytes(&self) -> &[u8] {
            &self.0
        }
    }

    impl OriginDimensions for ColorFramebuffer {
        fn size(&self) -> Size {
            Size::new(ST7789_PANEL_WIDTH as u32, ST7789_PANEL_HEIGHT as u32)
        }
    }

    impl DrawTarget for ColorFramebuffer {
        type Color = Rgb565;
        type Error = core::convert::Infallible;

        fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
        where
            I: IntoIterator<Item = Pixel<Self::Color>>,
        {
            for Pixel(point, color) in pixels {
                if point.x >= 0
                    && point.y >= 0
                    && point.x < ST7789_PANEL_WIDTH as i32
                    && point.y < ST7789_PANEL_HEIGHT as i32
                {
                    let offset = (point.y as usize * ST7789_PANEL_WIDTH + point.x as usize) * 2;
                    self.0[offset..offset + 2].copy_from_slice(&color.into_storage().to_be_bytes());
                }
            }
            Ok(())
        }

        fn clear(&mut self, color: Self::Color) -> Result<(), Self::Error> {
            let pixel = color.into_storage().to_be_bytes();
            for destination in self.0.chunks_exact_mut(2) {
                destination.copy_from_slice(&pixel);
            }
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

    fn request_output_lines(path: &str, offsets: &[u32]) -> io::Result<File> {
        let chip = OpenOptions::new().read(true).write(true).open(path)?;
        let mut config = LineConfig {
            flags: LineFlags::OUTPUT,
            ..Default::default()
        };
        config.add_values(&LineValues::from_slice(&vec![false; offsets.len()]));
        let request = LineRequest {
            offsets: Offsets::from_slice(offsets),
            consumer: "display-backends-demo-control".into(),
            config,
            num_lines: offsets.len() as u32,
            ..Default::default()
        };
        get_line(&chip, request).map_err(|error| match error {
            gpiocdev_uapi::Error::Os(gpiocdev_uapi::Errno(errno)) => {
                io::Error::from_raw_os_error(errno)
            }
            other => io::Error::other(other),
        })
    }

    fn draw_mono_scene(framebuffer: &mut MonoFramebuffer, frame: u32) {
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

    fn draw_color_scene(framebuffer: &mut ColorFramebuffer, frame: u32) {
        let navy = Rgb565::new(0, 4, 10);
        let cyan = Rgb565::new(0, 52, 31);
        let yellow = Rgb565::new(31, 52, 0);
        let white = Rgb565::new(31, 63, 31);
        framebuffer.clear(navy).unwrap();
        Rectangle::new(Point::new(4, 4), Size::new(232, 232))
            .into_styled(PrimitiveStyle::with_stroke(cyan, 3))
            .draw(framebuffer)
            .unwrap();
        Text::with_alignment(
            "DISPLAY BACKENDS",
            Point::new(120, 38),
            MonoTextStyle::new(&FONT_10X20, white),
            Alignment::Center,
        )
        .draw(framebuffer)
        .unwrap();
        Text::with_alignment(
            "NATIVE RGB565",
            Point::new(120, 68),
            MonoTextStyle::new(&FONT_6X10, yellow),
            Alignment::Center,
        )
        .draw(framebuffer)
        .unwrap();
        let x = 18 + (frame % 174) as i32;
        Circle::new(Point::new(x, 100), 44)
            .into_styled(PrimitiveStyle::with_fill(Rgb565::new(
                (frame % 32) as u8,
                42,
                31 - (frame % 32) as u8,
            )))
            .draw(framebuffer)
            .unwrap();
        Rectangle::new(Point::new(20, 180), Size::new(200, 24))
            .into_styled(PrimitiveStyle::with_stroke(white, 2))
            .draw(framebuffer)
            .unwrap();
        Rectangle::new(Point::new(24, 184), Size::new((frame % 193) + 1, 16))
            .into_styled(PrimitiveStyle::with_fill(yellow))
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
            Backend::Sh1106I2c => Some(request_output_lines(gpio_path, &[25])?),
            Backend::Sh1106Spi => Some(request_output_lines(gpio_path, &[24, 25])?),
            Backend::St7789Spi => Some(request_output_lines(gpio_path, &[25, 27, 24])?),
        };
        let control_fd = gpio.as_ref().map(AsRawFd::as_raw_fd);
        let mut display = Display::from_raw_fds(backend, bus.as_raw_fd(), control_fd)?;
        let start = Instant::now();
        if backend == Backend::St7789Spi {
            let mut framebuffer = ColorFramebuffer::new();
            for frame in 0..240 {
                draw_color_scene(&mut framebuffer, frame);
                display.write_native_frame(framebuffer.bytes())?;
            }
        } else {
            let mut framebuffer = MonoFramebuffer::new();
            for frame in 0..240 {
                draw_mono_scene(&mut framebuffer, frame);
                display.write_native_frame(framebuffer.bytes())?;
            }
        }
        let elapsed = start.elapsed();
        println!(
            "Composed and sent 240 native frames through {} in {:.2}s ({:.1} frames/s)",
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
