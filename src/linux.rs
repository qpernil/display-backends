// SPDX-License-Identifier: GPL-3.0-or-later

use crate::{
    encode_mono1_frame, encode_rgb565_frame, fit_rect, st7789_window, Backend, FrameFormat,
    MONO1_FRAME_SIZE, SH1106_DISPLAY_ON, SH1106_INIT, SSD1306_ADDRESS, SSD1306_INIT,
    ST7789_FRAMEBUFFER_SIZE, ST7789_INIT, ST7789_PANEL_HEIGHT, ST7789_PANEL_WIDTH,
};
use gpiocdev_uapi::v2::{set_line_values, LineValues};
use spidev::{SpiModeFlags, Spidev, SpidevOptions};
use std::{
    fs::File,
    io::{self, Write},
    os::fd::{AsRawFd, FromRawFd, RawFd},
    thread,
    time::Duration,
};

const SSD1306_I2C_ADDRESS: libc::c_ulong = 0x3c;
const SH1106_I2C_ADDRESS: libc::c_ulong = 0x3c;
const I2C_SLAVE: libc::c_ulong = 0x0703;
const SH1106_SPI_SPEED_HZ: u32 = 4_000_000;
const ST7789_SPI_SPEED_HZ: u32 = 62_500_000;
const SPI_CHUNK: usize = 4096;

enum Bus {
    I2c(File),
    Spi(Spidev),
}

pub struct Display {
    backend: Backend,
    bus: Bus,
    control: Option<File>,
    st7789_frame: Box<[u8; ST7789_FRAMEBUFFER_SIZE]>,
    mono1_frame: Box<[u8; MONO1_FRAME_SIZE]>,
}

impl Display {
    pub fn from_raw_fds(
        backend: Backend,
        bus_fd: RawFd,
        control_fd: Option<RawFd>,
    ) -> io::Result<Self> {
        let bus_file = duplicate_fd(bus_fd)?;
        let control = match control_fd {
            Some(fd) => Some(duplicate_fd(fd)?),
            None => None,
        };
        if backend.control_line_count() != 0 && control.is_none() {
            return Err(io::Error::from_raw_os_error(libc::EINVAL));
        }

        let bus = if backend.uses_spi() {
            let mut spi = Spidev::new(bus_file);
            let speed = if backend == Backend::St7789Spi {
                ST7789_SPI_SPEED_HZ
            } else {
                SH1106_SPI_SPEED_HZ
            };
            let options = SpidevOptions::new()
                .bits_per_word(8)
                .max_speed_hz(speed)
                .lsb_first(false)
                .mode(SpiModeFlags::SPI_MODE_0)
                .build();
            spi.configure(&options)?;
            Bus::Spi(spi)
        } else {
            let address = if backend == Backend::Sh1106I2c {
                SH1106_I2C_ADDRESS
            } else {
                SSD1306_I2C_ADDRESS
            };
            let result = unsafe { libc::ioctl(bus_file.as_raw_fd(), I2C_SLAVE, address) };
            if result != 0 {
                return Err(io::Error::last_os_error());
            }
            Bus::I2c(bus_file)
        };

        let mut display = Self {
            backend,
            bus,
            control,
            st7789_frame: Box::new([0; ST7789_FRAMEBUFFER_SIZE]),
            mono1_frame: Box::new([0; MONO1_FRAME_SIZE]),
        };
        display.initialize()?;
        Ok(display)
    }

    pub fn backend(&self) -> Backend {
        self.backend
    }

    pub fn native_format(&self) -> FrameFormat {
        self.backend.native_format()
    }

    pub fn write_native_frame(&mut self, framebuffer: &[u8]) -> io::Result<()> {
        if framebuffer.len() != self.native_format().required_len()? {
            return Err(io::Error::from_raw_os_error(libc::EINVAL));
        }
        match self.backend {
            Backend::Ssd1306I2c | Backend::Sh1106I2c | Backend::Sh1106Spi => {
                self.mono1_frame.copy_from_slice(framebuffer);
                self.write_oled_frame()
            }
            Backend::St7789Spi => {
                self.set_st7789_window(
                    0,
                    0,
                    ST7789_PANEL_WIDTH as u16,
                    ST7789_PANEL_HEIGHT as u16,
                )?;
                self.set_control(0, true)?;
                match &mut self.bus {
                    Bus::Spi(spi) => write_spi_chunks(spi, framebuffer),
                    Bus::I2c(_) => Err(io::Error::from_raw_os_error(libc::EINVAL)),
                }
            }
        }
    }

    pub fn write_frame(&mut self, framebuffer: &[u8], format: FrameFormat) -> io::Result<()> {
        if framebuffer.len() != format.required_len()? {
            return Err(io::Error::from_raw_os_error(libc::EINVAL));
        }
        if format == self.native_format() {
            return self.write_native_frame(framebuffer);
        }
        match self.backend {
            Backend::Ssd1306I2c | Backend::Sh1106I2c | Backend::Sh1106Spi => {
                encode_mono1_frame(&mut self.mono1_frame, framebuffer, format);
                self.write_oled_frame()
            }
            Backend::St7789Spi => {
                let (x, y, width, height) = fit_rect(
                    format.width,
                    format.height,
                    ST7789_PANEL_WIDTH,
                    ST7789_PANEL_HEIGHT,
                );
                let frame_length = width * height * 2;
                encode_rgb565_frame(
                    &mut self.st7789_frame[..frame_length],
                    framebuffer,
                    format,
                    width,
                    height,
                );
                self.set_st7789_window(x as u16, y as u16, width as u16, height as u16)?;
                self.set_control(0, true)?;
                let frame = &self.st7789_frame[..frame_length];
                match &mut self.bus {
                    Bus::Spi(spi) => write_spi_chunks(spi, frame),
                    Bus::I2c(_) => Err(io::Error::from_raw_os_error(libc::EINVAL)),
                }
            }
        }
    }

    pub fn shutdown(&mut self) -> io::Result<()> {
        match self.backend {
            Backend::St7789Spi => {
                self.clear_st7789()?;
                self.write_st7789_command(0x28)?;
                self.set_control(2, false)
            }
            Backend::Ssd1306I2c => {
                self.clear_oled()?;
                self.write_bus(&[0x00, 0xae])
            }
            Backend::Sh1106I2c | Backend::Sh1106Spi => {
                self.clear_oled()?;
                self.write_oled_message(&[0x00, 0xae])
            }
        }
    }

    fn initialize(&mut self) -> io::Result<()> {
        match self.backend {
            Backend::Ssd1306I2c => self.write_bus(SSD1306_INIT),
            Backend::Sh1106I2c | Backend::Sh1106Spi => {
                let reset = if self.backend == Backend::Sh1106I2c {
                    0
                } else {
                    1
                };
                self.pulse_reset(reset)?;
                self.write_oled_message(SH1106_INIT)?;
                thread::sleep(Duration::from_millis(100));
                self.write_oled_message(SH1106_DISPLAY_ON)
            }
            Backend::St7789Spi => {
                self.set_control(0, false)?;
                self.set_control(2, false)?;
                self.pulse_reset(1)?;
                for step in ST7789_INIT {
                    self.write_st7789_command(step.command)?;
                    if !step.data.is_empty() {
                        self.write_st7789_data(step.data)?;
                    }
                    if step.delay_ms != 0 {
                        thread::sleep(Duration::from_millis(step.delay_ms));
                    }
                }
                self.clear_st7789()?;
                self.set_control(2, true)
            }
        }
    }

    fn clear_oled(&mut self) -> io::Result<()> {
        self.mono1_frame.fill(0);
        self.write_oled_frame()
    }

    fn write_oled_frame(&mut self) -> io::Result<()> {
        if self.backend == Backend::Ssd1306I2c {
            self.write_bus(SSD1306_ADDRESS)?;
            let mut message = [0_u8; MONO1_FRAME_SIZE + 1];
            message[0] = 0x40;
            message[1..].copy_from_slice(&*self.mono1_frame);
            return self.write_bus(&message);
        }
        for page in 0..8_u8 {
            self.write_oled_message(&[0x00, 0xb0 | page, 0x02, 0x10])?;
            let mut message = [0_u8; 129];
            message[0] = 0x40;
            let offset = page as usize * 128;
            message[1..].copy_from_slice(&self.mono1_frame[offset..offset + 128]);
            self.write_oled_message(&message)?;
        }
        Ok(())
    }

    fn clear_st7789(&mut self) -> io::Result<()> {
        self.set_st7789_window(0, 0, ST7789_PANEL_WIDTH as u16, ST7789_PANEL_HEIGHT as u16)?;
        self.set_control(0, true)?;
        let zeroes = [0_u8; SPI_CHUNK];
        let mut remaining = ST7789_PANEL_WIDTH * ST7789_PANEL_HEIGHT * 2;
        while remaining != 0 {
            let length = remaining.min(zeroes.len());
            self.spi_mut()?.write_all(&zeroes[..length])?;
            remaining -= length;
        }
        Ok(())
    }

    fn pulse_reset(&self, index: usize) -> io::Result<()> {
        self.set_control(index, true)?;
        thread::sleep(Duration::from_millis(100));
        self.set_control(index, false)?;
        thread::sleep(Duration::from_millis(100));
        self.set_control(index, true)?;
        thread::sleep(Duration::from_millis(100));
        Ok(())
    }

    fn set_control(&self, index: usize, active: bool) -> io::Result<()> {
        if index >= self.backend.control_line_count() {
            return Err(io::Error::from_raw_os_error(libc::EINVAL));
        }
        let values = LineValues {
            bits: if active { 1_u64 << index } else { 0 },
            mask: 1_u64 << index,
        };
        set_line_values(
            self.control
                .as_ref()
                .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?,
            &values,
        )
        .map_err(gpio_error)
    }

    fn write_oled_message(&mut self, message: &[u8]) -> io::Result<()> {
        if self.backend == Backend::Sh1106Spi {
            if message.len() < 2 || !matches!(message[0], 0x00 | 0x40) {
                return Err(io::Error::from_raw_os_error(libc::EINVAL));
            }
            self.set_control(0, message[0] == 0x40)?;
            write_spi_chunks(self.spi_mut()?, &message[1..])
        } else {
            self.write_bus(message)
        }
    }

    fn set_st7789_window(&mut self, x: u16, y: u16, width: u16, height: u16) -> io::Result<()> {
        let (column, row) = st7789_window(x, y, width, height);
        self.write_st7789_command(0x2a)?;
        self.write_st7789_data(&column)?;
        self.write_st7789_command(0x2b)?;
        self.write_st7789_data(&row)?;
        self.write_st7789_command(0x2c)
    }

    fn write_st7789_command(&mut self, command: u8) -> io::Result<()> {
        self.set_control(0, false)?;
        self.spi_mut()?.write_all(&[command])
    }

    fn write_st7789_data(&mut self, data: &[u8]) -> io::Result<()> {
        self.set_control(0, true)?;
        write_spi_chunks(self.spi_mut()?, data)
    }

    fn write_bus(&mut self, message: &[u8]) -> io::Result<()> {
        match &mut self.bus {
            Bus::I2c(file) => file.write_all(message),
            Bus::Spi(spi) => write_spi_chunks(spi, message),
        }
    }

    fn spi_mut(&mut self) -> io::Result<&mut Spidev> {
        match &mut self.bus {
            Bus::Spi(spi) => Ok(spi),
            Bus::I2c(_) => Err(io::Error::from_raw_os_error(libc::EINVAL)),
        }
    }
}

fn duplicate_fd(fd: RawFd) -> io::Result<File> {
    if fd < 0 {
        return Err(io::Error::from_raw_os_error(libc::EBADF));
    }
    let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if duplicate < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(duplicate) })
}

fn gpio_error(error: gpiocdev_uapi::Error) -> io::Error {
    match error {
        gpiocdev_uapi::Error::Os(gpiocdev_uapi::Errno(errno)) => {
            io::Error::from_raw_os_error(errno)
        }
        other => io::Error::new(io::ErrorKind::UnexpectedEof, other),
    }
}

fn write_spi_chunks(spi: &mut Spidev, data: &[u8]) -> io::Result<()> {
    for chunk in data.chunks(SPI_CHUNK) {
        spi.write_all(chunk)?;
    }
    Ok(())
}
