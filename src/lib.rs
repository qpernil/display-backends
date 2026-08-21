// SPDX-License-Identifier: GPL-3.0-or-later

//! Physical display backends for Linux workers that receive already-opened
//! bus and GPIO capability handles.

use std::io;

#[cfg(target_os = "linux")]
mod ffi;
#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
pub use linux::Display;

pub const MONO1_FRAME_WIDTH: usize = 128;
pub const MONO1_FRAME_HEIGHT: usize = 64;
pub const MONO1_FRAME_STRIDE: usize = 128;
pub const MONO1_FRAME_SIZE: usize = MONO1_FRAME_STRIDE * MONO1_FRAME_HEIGHT.div_ceil(8);

pub const ST7789_PANEL_WIDTH: usize = 240;
pub const ST7789_PANEL_HEIGHT: usize = 240;
pub const ST7789_FRAMEBUFFER_SIZE: usize = ST7789_PANEL_WIDTH * ST7789_PANEL_HEIGHT * 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum PixelFormat {
    Mono1MsbReversePage = 0,
    Mono8 = 1,
    Rgb565Be = 2,
}

impl TryFrom<u32> for PixelFormat {
    type Error = io::Error;

    fn try_from(value: u32) -> io::Result<Self> {
        match value {
            0 => Ok(Self::Mono1MsbReversePage),
            1 => Ok(Self::Mono8),
            2 => Ok(Self::Rgb565Be),
            _ => Err(invalid_argument()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameFormat {
    pub pixel_format: PixelFormat,
    pub width: usize,
    pub height: usize,
    pub stride: usize,
}

impl FrameFormat {
    pub fn new(
        pixel_format: PixelFormat,
        width: usize,
        height: usize,
        stride: usize,
    ) -> io::Result<Self> {
        let format = Self {
            pixel_format,
            width,
            height,
            stride,
        };
        format.required_len()?;
        Ok(format)
    }

    pub fn mono1_128x64() -> Self {
        Self {
            pixel_format: PixelFormat::Mono1MsbReversePage,
            width: MONO1_FRAME_WIDTH,
            height: MONO1_FRAME_HEIGHT,
            stride: MONO1_FRAME_STRIDE,
        }
    }

    pub fn rgb565_240x240() -> Self {
        Self {
            pixel_format: PixelFormat::Rgb565Be,
            width: ST7789_PANEL_WIDTH,
            height: ST7789_PANEL_HEIGHT,
            stride: ST7789_PANEL_WIDTH * 2,
        }
    }

    pub fn required_len(self) -> io::Result<usize> {
        let minimum_stride = match self.pixel_format {
            PixelFormat::Mono1MsbReversePage | PixelFormat::Mono8 => self.width,
            PixelFormat::Rgb565Be => self.width.checked_mul(2).ok_or_else(invalid_argument)?,
        };
        if self.width == 0
            || self.height == 0
            || self.width > u16::MAX as usize
            || self.height > u16::MAX as usize
            || self.stride < minimum_stride
        {
            return Err(invalid_argument());
        }
        let rows = match self.pixel_format {
            PixelFormat::Mono1MsbReversePage => self.height.div_ceil(8),
            PixelFormat::Mono8 | PixelFormat::Rgb565Be => self.height,
        };
        self.stride.checked_mul(rows).ok_or_else(invalid_argument)
    }

    pub fn intensity(self, data: &[u8], x: usize, y: usize) -> u8 {
        match self.pixel_format {
            PixelFormat::Mono1MsbReversePage => {
                let page = y / 8;
                let offset =
                    (self.height.div_ceil(8) - 1 - page) * self.stride + (self.stride - 1 - x);
                let mask = 1 << (7 - y % 8);
                if data[offset] & mask != 0 {
                    0xff
                } else {
                    0x00
                }
            }
            PixelFormat::Mono8 => data[y * self.stride + x],
            PixelFormat::Rgb565Be => {
                let offset = y * self.stride + x * 2;
                let pixel = u16::from_be_bytes([data[offset], data[offset + 1]]);
                let red = ((pixel >> 11) & 0x1f) * 255 / 31;
                let green = ((pixel >> 5) & 0x3f) * 255 / 63;
                let blue = (pixel & 0x1f) * 255 / 31;
                ((red * 77 + green * 150 + blue * 29) >> 8) as u8
            }
        }
    }

    pub fn rgb565(self, data: &[u8], x: usize, y: usize) -> u16 {
        match self.pixel_format {
            PixelFormat::Rgb565Be => {
                let offset = y * self.stride + x * 2;
                u16::from_be_bytes([data[offset], data[offset + 1]])
            }
            PixelFormat::Mono1MsbReversePage | PixelFormat::Mono8 => {
                let gray = u16::from(self.intensity(data, x, y));
                ((gray >> 3) << 11) | ((gray >> 2) << 5) | (gray >> 3)
            }
        }
    }
}

fn invalid_argument() -> io::Error {
    io::Error::from_raw_os_error(libc::EINVAL)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Backend {
    Ssd1306I2c = 0,
    Sh1106I2c = 1,
    Sh1106Spi = 2,
    St7789Spi = 3,
}

impl Backend {
    pub fn name(self) -> &'static str {
        match self {
            Self::Ssd1306I2c => "ssd1306-i2c",
            Self::Sh1106I2c => "sh1106-i2c",
            Self::Sh1106Spi => "sh1106-spi",
            Self::St7789Spi => "st7789-spi",
        }
    }

    pub fn uses_spi(self) -> bool {
        matches!(self, Self::Sh1106Spi | Self::St7789Spi)
    }

    pub fn control_line_count(self) -> usize {
        match self {
            Self::Ssd1306I2c => 0,
            Self::Sh1106I2c => 1,
            Self::Sh1106Spi => 2,
            Self::St7789Spi => 3,
        }
    }

    pub fn native_format(self) -> FrameFormat {
        match self {
            Self::Ssd1306I2c | Self::Sh1106I2c | Self::Sh1106Spi => FrameFormat::mono1_128x64(),
            Self::St7789Spi => FrameFormat::rgb565_240x240(),
        }
    }
}

impl TryFrom<u32> for Backend {
    type Error = io::Error;

    fn try_from(value: u32) -> io::Result<Self> {
        match value {
            0 => Ok(Self::Ssd1306I2c),
            1 => Ok(Self::Sh1106I2c),
            2 => Ok(Self::Sh1106Spi),
            3 => Ok(Self::St7789Spi),
            _ => Err(io::Error::from_raw_os_error(libc::EINVAL)),
        }
    }
}

#[cfg(any(target_os = "linux", test))]
pub(crate) const SSD1306_INIT: &[u8] = &[
    0x00, 0xae, 0xd5, 0x80, 0xa8, 0x3f, 0xd3, 0x00, 0x40, 0x8d, 0x14, 0x20, 0x00, 0xa1, 0xc8, 0xda,
    0x12, 0x81, 0xcf, 0xd9, 0xf1, 0xdb, 0x40, 0xa4, 0xa6, 0xaf,
];
#[cfg(any(target_os = "linux", test))]
pub(crate) const SSD1306_ADDRESS: &[u8] = &[0x00, 0x21, 0x00, 0x7f, 0x22, 0x00, 0x07];

#[cfg(any(target_os = "linux", test))]
pub(crate) const SH1106_INIT: &[u8] = &[
    0x00, 0xae, 0x02, 0x10, 0x40, 0x81, 0xa0, 0xa1, 0xc8, 0xa6, 0xa8, 0x3f, 0xd3, 0x00, 0xd5, 0x80,
    0xd9, 0xf1, 0xda, 0x12, 0xdb, 0x40, 0x20, 0x02, 0xa4, 0xa6,
];
#[cfg(any(target_os = "linux", test))]
pub(crate) const SH1106_DISPLAY_ON: &[u8] = &[0x00, 0xaf];

#[cfg(any(target_os = "linux", test))]
#[derive(Clone, Copy)]
pub(crate) struct InitStep {
    pub command: u8,
    pub data: &'static [u8],
    pub delay_ms: u64,
}

#[cfg(any(target_os = "linux", test))]
pub(crate) const ST7789_INIT: &[InitStep] = &[
    InitStep {
        command: 0x11,
        data: &[],
        delay_ms: 120,
    },
    InitStep {
        command: 0x36,
        data: &[0x70],
        delay_ms: 0,
    },
    InitStep {
        command: 0x3a,
        data: &[0x05],
        delay_ms: 0,
    },
    InitStep {
        command: 0xb2,
        data: &[0x0c, 0x0c, 0x00, 0x33, 0x33],
        delay_ms: 0,
    },
    InitStep {
        command: 0xb7,
        data: &[0x00],
        delay_ms: 0,
    },
    InitStep {
        command: 0xbb,
        data: &[0x3f],
        delay_ms: 0,
    },
    InitStep {
        command: 0xc0,
        data: &[0x2c],
        delay_ms: 0,
    },
    InitStep {
        command: 0xc2,
        data: &[0x01],
        delay_ms: 0,
    },
    InitStep {
        command: 0xc3,
        data: &[0x0d],
        delay_ms: 0,
    },
    InitStep {
        command: 0xc6,
        data: &[0x0f],
        delay_ms: 0,
    },
    InitStep {
        command: 0xd0,
        data: &[0xa7],
        delay_ms: 0,
    },
    InitStep {
        command: 0xd0,
        data: &[0xa4, 0xa1],
        delay_ms: 0,
    },
    InitStep {
        command: 0xd6,
        data: &[0xa1],
        delay_ms: 0,
    },
    InitStep {
        command: 0xe0,
        data: &[
            0xf0, 0x00, 0x02, 0x01, 0x00, 0x00, 0x27, 0x43, 0x3f, 0x33, 0x0e, 0x0e, 0x26, 0x2e,
        ],
        delay_ms: 0,
    },
    InitStep {
        command: 0xe1,
        data: &[
            0xf0, 0x07, 0x0d, 0x0d, 0x0b, 0x16, 0x26, 0x43, 0x3e, 0x3f, 0x19, 0x19, 0x31, 0x3a,
        ],
        delay_ms: 0,
    },
    InitStep {
        command: 0x21,
        data: &[],
        delay_ms: 0,
    },
    InitStep {
        command: 0x29,
        data: &[],
        delay_ms: 20,
    },
];

pub fn set_mono1_pixel(framebuffer: &mut [u8], format: FrameFormat, x: usize, y: usize, on: bool) {
    if format.pixel_format != PixelFormat::Mono1MsbReversePage
        || x >= format.width
        || y >= format.height
        || framebuffer.len() != format.required_len().unwrap_or(0)
    {
        return;
    }
    let page = y / 8;
    let offset = (format.height.div_ceil(8) - 1 - page) * format.stride + (format.stride - 1 - x);
    let mask = 1 << (7 - y % 8);
    if on {
        framebuffer[offset] |= mask;
    } else {
        framebuffer[offset] &= !mask;
    }
}

pub fn fit_rect(
    source_width: usize,
    source_height: usize,
    width: usize,
    height: usize,
) -> (usize, usize, usize, usize) {
    let (render_width, render_height) = if width * source_height <= height * source_width {
        (width, (source_height * width / source_width).max(1))
    } else {
        ((source_width * height / source_height).max(1), height)
    };
    (
        (width - render_width) / 2,
        (height - render_height) / 2,
        render_width,
        render_height,
    )
}

pub fn encode_rgb565_frame(
    destination: &mut [u8],
    source: &[u8],
    format: FrameFormat,
    render_width: usize,
    render_height: usize,
) {
    destination.fill(0);
    let (offset_x, offset_y, width, height) =
        fit_rect(format.width, format.height, render_width, render_height);
    for y in 0..height {
        let source_y = y * format.height / height;
        for x in 0..width {
            let source_x = x * format.width / width;
            let rgb565 = format.rgb565(source, source_x, source_y);
            let output = ((offset_y + y) * render_width + offset_x + x) * 2;
            destination[output] = (rgb565 >> 8) as u8;
            destination[output + 1] = rgb565 as u8;
        }
    }
}

pub fn encode_mono1_frame(
    destination: &mut [u8; MONO1_FRAME_SIZE],
    source: &[u8],
    format: FrameFormat,
) {
    destination.fill(0);
    let target = FrameFormat::mono1_128x64();
    let (offset_x, offset_y, width, height) = fit_rect(
        format.width,
        format.height,
        MONO1_FRAME_WIDTH,
        MONO1_FRAME_HEIGHT,
    );
    for y in 0..height {
        let source_y = y * format.height / height;
        for x in 0..width {
            let source_x = x * format.width / width;
            set_mono1_pixel(
                destination,
                target,
                offset_x + x,
                offset_y + y,
                format.intensity(source, source_x, source_y) >= 128,
            );
        }
    }
}

#[cfg(any(target_os = "linux", test))]
pub(crate) fn st7789_window(x: u16, y: u16, width: u16, height: u16) -> ([u8; 4], [u8; 4]) {
    let x_end = x + width - 1;
    let y_end = y + height - 1;
    (
        [(x >> 8) as u8, x as u8, (x_end >> 8) as u8, x_end as u8],
        [(y >> 8) as u8, y as u8, (y_end >> 8) as u8, y_end as u8],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_metadata_is_stable_for_the_c_abi() {
        assert_eq!(Backend::Ssd1306I2c as u32, 0);
        assert_eq!(Backend::Sh1106I2c.control_line_count(), 1);
        assert_eq!(Backend::Sh1106Spi.control_line_count(), 2);
        assert_eq!(Backend::St7789Spi.control_line_count(), 3);
        assert!(Backend::St7789Spi.uses_spi());
        assert_eq!(
            Backend::Ssd1306I2c.native_format(),
            FrameFormat::mono1_128x64()
        );
        assert_eq!(
            Backend::St7789Spi.native_format(),
            FrameFormat::rgb565_240x240()
        );
    }

    #[test]
    fn st7789_window_matches_centered_128x64_input() {
        let (x, y, width, height) = fit_rect(128, 64, 240, 240);
        assert_eq!((x, y, width, height), (0, 60, 240, 120));
        let (column, row) = st7789_window(0, 60, 240, 120);
        assert_eq!(column, [0x00, 0x00, 0x00, 0xef]);
        assert_eq!(row, [0x00, 0x3c, 0x00, 0xb3]);
    }

    #[test]
    fn st7789_conversion_preserves_source_pixels() {
        let format = FrameFormat::mono1_128x64();
        let mut source = [0_u8; MONO1_FRAME_SIZE];
        for y in 0..format.height {
            for x in 0..format.width {
                set_mono1_pixel(&mut source, format, x, y, ((x / 7) + (y / 5)) % 2 != 0);
            }
        }
        let mut output = [0_u8; 240 * 120 * 2];
        encode_rgb565_frame(&mut output, &source, format, 240, 120);
        for y in 0..120 {
            for x in 0..240 {
                let source_x = x * format.width / 240;
                let source_y = y * format.height / 120;
                let expected = if ((source_x / 7) + (source_y / 5)) % 2 != 0 {
                    0xff
                } else {
                    0x00
                };
                let index = (y * 240 + x) * 2;
                assert_eq!(&output[index..index + 2], &[expected, expected]);
            }
        }
    }

    #[test]
    fn mono8_converts_to_rgb565_and_mono1() {
        let format = FrameFormat::new(PixelFormat::Mono8, 2, 1, 2).unwrap();
        let source = [0x00, 0x80];
        let mut color = [0_u8; 4];
        encode_rgb565_frame(&mut color, &source, format, 2, 1);
        assert_eq!(color, [0x00, 0x00, 0x84, 0x10]);

        let mut mono = [0_u8; MONO1_FRAME_SIZE];
        encode_mono1_frame(&mut mono, &source, format);
        let target = FrameFormat::mono1_128x64();
        assert_eq!(target.intensity(&mono, 0, 0), 0x00);
        assert_eq!(target.intensity(&mono, 127, 0), 0xff);
    }

    #[test]
    fn native_rgb565_conversion_is_byte_exact() {
        let format = FrameFormat::new(PixelFormat::Rgb565Be, 2, 1, 4).unwrap();
        let source = [0xf8, 0x00, 0x00, 0x1f];
        let mut output = [0_u8; 4];
        encode_rgb565_frame(&mut output, &source, format, 2, 1);
        assert_eq!(output, source);

        let mut mono = [0_u8; MONO1_FRAME_SIZE];
        let grayscale = [0xff, 0xff, 0x00, 0x00];
        encode_mono1_frame(&mut mono, &grayscale, format);
        let target = FrameFormat::mono1_128x64();
        assert_eq!(target.intensity(&mono, 0, 0), 0xff);
        assert_eq!(target.intensity(&mono, 127, 0), 0x00);
    }

    #[test]
    fn native_mono1_conversion_is_byte_exact() {
        let format = FrameFormat::mono1_128x64();
        let mut source = [0_u8; MONO1_FRAME_SIZE];
        for (index, byte) in source.iter_mut().enumerate() {
            *byte = (index as u8).wrapping_mul(37);
        }
        let mut output = [0_u8; MONO1_FRAME_SIZE];
        encode_mono1_frame(&mut output, &source, format);
        assert_eq!(output, source);
    }

    #[test]
    fn frame_formats_validate_stride_and_length() {
        assert!(FrameFormat::new(PixelFormat::Mono8, 4, 3, 3).is_err());
        assert_eq!(
            FrameFormat::new(PixelFormat::Mono8, 4, 3, 6)
                .unwrap()
                .required_len()
                .unwrap(),
            18
        );
        assert_eq!(
            FrameFormat::new(PixelFormat::Mono1MsbReversePage, 4, 9, 4)
                .unwrap()
                .required_len()
                .unwrap(),
            8
        );
        assert!(FrameFormat::new(PixelFormat::Rgb565Be, 4, 3, 7).is_err());
        assert_eq!(
            FrameFormat::new(PixelFormat::Rgb565Be, 4, 3, 10)
                .unwrap()
                .required_len()
                .unwrap(),
            30
        );
    }

    #[test]
    fn controller_sequences_remain_exact() {
        assert_eq!(SSD1306_INIT.len(), 26);
        assert_eq!(SSD1306_ADDRESS, [0x00, 0x21, 0x00, 0x7f, 0x22, 0x00, 0x07]);
        assert_eq!(SH1106_INIT.len(), 26);
        assert_eq!(SH1106_DISPLAY_ON, [0x00, 0xaf]);
        assert_eq!(ST7789_INIT.len(), 17);
        assert_eq!(ST7789_INIT[0].command, 0x11);
        assert_eq!(ST7789_INIT[0].delay_ms, 120);
        assert_eq!(ST7789_INIT[1].data, [0x70]);
        assert_eq!(ST7789_INIT[16].command, 0x29);
    }
}
