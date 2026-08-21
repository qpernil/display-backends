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

pub const SOURCE_WIDTH: usize = 128;
pub const SOURCE_HEIGHT: usize = 64;
pub const SOURCE_FRAMEBUFFER_SIZE: usize = SOURCE_WIDTH * SOURCE_HEIGHT / 8;

pub const ST7789_PANEL_WIDTH: usize = 240;
pub const ST7789_PANEL_HEIGHT: usize = 240;
pub const ST7789_RENDER_WIDTH: usize = 240;
pub const ST7789_RENDER_HEIGHT: usize = 120;
pub const ST7789_RENDER_X: usize = 0;
pub const ST7789_RENDER_Y: usize = 60;
pub const ST7789_RENDER_BUFFER_SIZE: usize = ST7789_RENDER_WIDTH * ST7789_RENDER_HEIGHT * 2;

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

pub fn set_source_pixel(
    framebuffer: &mut [u8; SOURCE_FRAMEBUFFER_SIZE],
    x: usize,
    y: usize,
    on: bool,
) {
    if x >= SOURCE_WIDTH || y >= SOURCE_HEIGHT {
        return;
    }
    let offset = SOURCE_FRAMEBUFFER_SIZE - 1 - x - (y / 8) * SOURCE_WIDTH;
    let mask = 1 << (7 - y % 8);
    if on {
        framebuffer[offset] |= mask;
    } else {
        framebuffer[offset] &= !mask;
    }
}

pub fn encode_st7789_frame(
    destination: &mut [u8; ST7789_RENDER_BUFFER_SIZE],
    source: &[u8; SOURCE_FRAMEBUFFER_SIZE],
) {
    let mut output = 0;
    for y in 0..ST7789_RENDER_HEIGHT {
        let source_y = y * SOURCE_HEIGHT / ST7789_RENDER_HEIGHT;
        for x in 0..ST7789_RENDER_WIDTH {
            let source_x = x * SOURCE_WIDTH / ST7789_RENDER_WIDTH;
            let offset = SOURCE_FRAMEBUFFER_SIZE - 1 - source_x - (source_y / 8) * SOURCE_WIDTH;
            let mask = 1 << (7 - source_y % 8);
            let component = if source[offset] & mask != 0 {
                0xff
            } else {
                0x00
            };
            destination[output] = component;
            destination[output + 1] = component;
            output += 2;
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
    }

    #[test]
    fn st7789_window_matches_the_centered_trezor_view() {
        let (column, row) = st7789_window(0, 60, 240, 120);
        assert_eq!(column, [0x00, 0x00, 0x00, 0xef]);
        assert_eq!(row, [0x00, 0x3c, 0x00, 0xb3]);
    }

    #[test]
    fn st7789_conversion_preserves_source_pixels() {
        let mut source = [0_u8; SOURCE_FRAMEBUFFER_SIZE];
        for y in 0..SOURCE_HEIGHT {
            for x in 0..SOURCE_WIDTH {
                set_source_pixel(&mut source, x, y, ((x / 7) + (y / 5)) % 2 != 0);
            }
        }
        let mut output = [0_u8; ST7789_RENDER_BUFFER_SIZE];
        encode_st7789_frame(&mut output, &source);
        for y in 0..ST7789_RENDER_HEIGHT {
            for x in 0..ST7789_RENDER_WIDTH {
                let source_x = x * SOURCE_WIDTH / ST7789_RENDER_WIDTH;
                let source_y = y * SOURCE_HEIGHT / ST7789_RENDER_HEIGHT;
                let expected = if ((source_x / 7) + (source_y / 5)) % 2 != 0 {
                    0xff
                } else {
                    0x00
                };
                let index = (y * ST7789_RENDER_WIDTH + x) * 2;
                assert_eq!(&output[index..index + 2], &[expected, expected]);
            }
        }
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
