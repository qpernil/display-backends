// SPDX-License-Identifier: GPL-3.0-or-later

use crate::{
    Backend, DisplayConverter, FrameFormat, MONO1_FRAME_STRIDE, SH1106_DISPLAY_ON, SH1106_INIT,
    SSD1306_INIT, ST7789_INIT, ST7789_PANEL_WIDTH, UpdateRect, sh1106_page_address,
    ssd1306_address, st7789_window,
};
use gpiocdev_uapi::v2::{LineValues, set_line_values};
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
    converter: DisplayConverter,
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
            converter: DisplayConverter::new(backend),
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
        let update = self.converter.prepare_native(framebuffer)?;
        self.finish_update(update)
    }

    pub fn write_frame(&mut self, framebuffer: &[u8], format: FrameFormat) -> io::Result<()> {
        let update = self.converter.prepare_frame(framebuffer, format)?;
        self.finish_update(update)
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
        let update = self.converter.prepare_blank();
        self.finish_update(update)
    }

    fn write_oled_update(&mut self, rect: UpdateRect) -> io::Result<()> {
        debug_assert_eq!(rect.y % 8, 0);
        debug_assert_eq!(rect.height % 8, 0);
        let first_page = rect.y / 8;
        let pages = rect.height / 8;
        if self.backend == Backend::Ssd1306I2c {
            self.write_bus(&ssd1306_address(rect))?;
            let mut message = Vec::with_capacity(1 + rect.width * pages);
            message.push(0x40);
            for page in first_page..first_page + pages {
                let offset = page * MONO1_FRAME_STRIDE + rect.x;
                message
                    .extend_from_slice(&self.converter.next_frame()[offset..offset + rect.width]);
            }
            return self.write_bus(&message);
        }
        for page in first_page..first_page + pages {
            self.write_oled_message(&sh1106_page_address(page, rect.x))?;
            let mut message = Vec::with_capacity(1 + rect.width);
            message.push(0x40);
            let offset = page * MONO1_FRAME_STRIDE + rect.x;
            message.extend_from_slice(&self.converter.next_frame()[offset..offset + rect.width]);
            self.write_oled_message(&message)?;
        }
        Ok(())
    }

    fn clear_st7789(&mut self) -> io::Result<()> {
        let update = self.converter.prepare_blank();
        self.finish_update(update)
    }

    fn finish_update(&mut self, update: Option<UpdateRect>) -> io::Result<()> {
        let Some(rect) = update else {
            return Ok(());
        };
        match self.backend {
            Backend::Ssd1306I2c | Backend::Sh1106I2c | Backend::Sh1106Spi => {
                self.write_oled_update(rect)?
            }
            Backend::St7789Spi => self.write_st7789_update(rect)?,
        }
        self.converter.commit();
        Ok(())
    }

    fn write_st7789_update(&mut self, rect: UpdateRect) -> io::Result<()> {
        self.set_st7789_window(
            rect.x as u16,
            rect.y as u16,
            rect.width as u16,
            rect.height as u16,
        )?;
        self.set_control(0, true)?;
        let frame = self.converter.next_frame();
        let Bus::Spi(spi) = &mut self.bus else {
            return Err(io::Error::from_raw_os_error(libc::EINVAL));
        };
        for y in rect.y..rect.y + rect.height {
            let offset = (y * ST7789_PANEL_WIDTH + rect.x) * 2;
            write_spi_chunks(spi, &frame[offset..offset + rect.width * 2])?;
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
