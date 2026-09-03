// SPDX-License-Identifier: GPL-3.0-or-later

use crate::{Backend, Display, FrameFormat, PixelFormat};
use std::{io, os::fd::RawFd, ptr, slice};

#[repr(C)]
pub struct DisplayBackendsHandle {
    display: Display,
}

fn error_code(error: &io::Error) -> libc::c_int {
    error.raw_os_error().unwrap_or(libc::EIO)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn display_backends_create(
    backend: u32,
    bus_fd: RawFd,
    control_fd: RawFd,
    output: *mut *mut DisplayBackendsHandle,
) -> libc::c_int {
    if output.is_null() {
        return libc::EINVAL;
    }
    unsafe { *output = ptr::null_mut() };
    let backend = match Backend::try_from(backend) {
        Ok(backend) => backend,
        Err(error) => return error_code(&error),
    };
    let control = (backend.control_line_count() != 0).then_some(control_fd);
    match Display::from_raw_fds(backend, bus_fd, control) {
        Ok(display) => {
            let handle = Box::new(DisplayBackendsHandle { display });
            unsafe { *output = Box::into_raw(handle) };
            0
        }
        Err(error) => error_code(&error),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn display_backends_write_frame(
    handle: *mut DisplayBackendsHandle,
    pixel_format: u32,
    width: usize,
    height: usize,
    stride: usize,
    framebuffer: *const u8,
    length: usize,
) -> libc::c_int {
    if handle.is_null() || framebuffer.is_null() {
        return libc::EINVAL;
    }
    let pixel_format = match PixelFormat::try_from(pixel_format) {
        Ok(pixel_format) => pixel_format,
        Err(error) => return error_code(&error),
    };
    let format = match FrameFormat::new(pixel_format, width, height, stride) {
        Ok(format) => format,
        Err(error) => return error_code(&error),
    };
    let expected = match format.required_len() {
        Ok(expected) => expected,
        Err(error) => return error_code(&error),
    };
    if length != expected {
        return libc::EINVAL;
    }
    let bytes = unsafe { slice::from_raw_parts(framebuffer, length) };
    match unsafe { &mut *handle }.display.write_frame(bytes, format) {
        Ok(()) => 0,
        Err(error) => error_code(&error),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn display_backends_write_native_frame(
    handle: *mut DisplayBackendsHandle,
    framebuffer: *const u8,
    length: usize,
) -> libc::c_int {
    if handle.is_null() || framebuffer.is_null() {
        return libc::EINVAL;
    }
    let expected = match unsafe { &*handle }.display.native_format().required_len() {
        Ok(expected) => expected,
        Err(error) => return error_code(&error),
    };
    if length != expected {
        return libc::EINVAL;
    }
    let bytes = unsafe { slice::from_raw_parts(framebuffer, length) };
    match unsafe { &mut *handle }.display.write_native_frame(bytes) {
        Ok(()) => 0,
        Err(error) => error_code(&error),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn display_backends_shutdown(
    handle: *mut DisplayBackendsHandle,
) -> libc::c_int {
    if handle.is_null() {
        return libc::EINVAL;
    }
    match unsafe { &mut *handle }.display.shutdown() {
        Ok(()) => 0,
        Err(error) => error_code(&error),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn display_backends_destroy(handle: *mut DisplayBackendsHandle) {
    if !handle.is_null() {
        drop(unsafe { Box::from_raw(handle) });
    }
}
