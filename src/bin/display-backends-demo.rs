// SPDX-License-Identifier: GPL-3.0-or-later

#[cfg(target_os = "linux")]
mod linux {
    use display_backends::{Backend, Display, FrameFormat, PixelFormat};
    use embedded_graphics::{
        mono_font::{ascii::FONT_10X20, ascii::FONT_6X10, MonoTextStyle},
        pixelcolor::Gray8,
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

    const FRAME_WIDTH: usize = 128;
    const FRAME_HEIGHT: usize = 64;
    const FRAME_SIZE: usize = FRAME_WIDTH * FRAME_HEIGHT;

    fn frame_format() -> FrameFormat {
        FrameFormat::new(PixelFormat::Mono8, FRAME_WIDTH, FRAME_HEIGHT, FRAME_WIDTH)
            .expect("the fixed demo frame format is valid")
    }

    struct DemoFramebuffer([u8; FRAME_SIZE]);

    impl DemoFramebuffer {
        fn new() -> Self {
            Self([0; FRAME_SIZE])
        }

        fn bytes(&self) -> &[u8; FRAME_SIZE] {
            &self.0
        }
    }

    impl OriginDimensions for DemoFramebuffer {
        fn size(&self) -> Size {
            Size::new(FRAME_WIDTH as u32, FRAME_HEIGHT as u32)
        }
    }

    impl DrawTarget for DemoFramebuffer {
        type Color = Gray8;
        type Error = core::convert::Infallible;

        fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
        where
            I: IntoIterator<Item = Pixel<Self::Color>>,
        {
            for Pixel(point, color) in pixels {
                if point.x >= 0
                    && point.y >= 0
                    && point.x < FRAME_WIDTH as i32
                    && point.y < FRAME_HEIGHT as i32
                {
                    self.0[point.y as usize * FRAME_WIDTH + point.x as usize] = color.luma();
                }
            }
            Ok(())
        }

        fn clear(&mut self, color: Self::Color) -> Result<(), Self::Error> {
            self.0.fill(color.luma());
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

    fn draw_scene(framebuffer: &mut DemoFramebuffer, frame: u32) {
        framebuffer.clear(Gray8::new(0)).unwrap();
        Rectangle::new(Point::new(0, 0), Size::new(128, 64))
            .into_styled(PrimitiveStyle::with_stroke(Gray8::new(255), 1))
            .draw(framebuffer)
            .unwrap();
        Text::with_alignment(
            "RUST DISPLAY",
            Point::new(64, 17),
            MonoTextStyle::new(&FONT_6X10, Gray8::new(160)),
            Alignment::Center,
        )
        .draw(framebuffer)
        .unwrap();
        Text::with_alignment(
            "BACKENDS",
            Point::new(64, 38),
            MonoTextStyle::new(&FONT_10X20, Gray8::new(255)),
            Alignment::Center,
        )
        .draw(framebuffer)
        .unwrap();
        let x = 8 + (frame % 108) as i32;
        Circle::new(Point::new(x, 49), 8)
            .into_styled(PrimitiveStyle::with_fill(Gray8::new(
                128 + (frame % 128) as u8,
            )))
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
        let mut display =
            Display::from_raw_fds(backend, frame_format(), bus.as_raw_fd(), control_fd)?;
        let mut framebuffer = DemoFramebuffer::new();
        let start = Instant::now();
        for frame in 0..240 {
            draw_scene(&mut framebuffer, frame);
            display.write_frame(framebuffer.bytes())?;
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
