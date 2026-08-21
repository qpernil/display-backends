// SPDX-License-Identifier: GPL-3.0-or-later

use crate::{Backend, Display, SOURCE_FRAMEBUFFER_SIZE};
use std::{io, os::fd::RawFd, ptr, slice};

#[repr(C)]
pub struct DisplayBackendsHandle {
    display: Display,
}

fn error_code(error: &io::Error) -> libc::c_int {
    error.raw_os_error().unwrap_or(libc::EIO)
}

#[no_mangle]
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

#[no_mangle]
pub unsafe extern "C" fn display_backends_write_trezor_frame(
    handle: *mut DisplayBackendsHandle,
    framebuffer: *const u8,
    length: usize,
) -> libc::c_int {
    if handle.is_null() || framebuffer.is_null() || length != SOURCE_FRAMEBUFFER_SIZE {
        return libc::EINVAL;
    }
    let bytes = unsafe { slice::from_raw_parts(framebuffer, length) };
    let frame: &[u8; SOURCE_FRAMEBUFFER_SIZE] = match bytes.try_into() {
        Ok(frame) => frame,
        Err(_) => return libc::EINVAL,
    };
    match unsafe { &mut *handle }.display.write_trezor_frame(frame) {
        Ok(()) => 0,
        Err(error) => error_code(&error),
    }
}

#[no_mangle]
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

#[no_mangle]
pub unsafe extern "C" fn display_backends_destroy(handle: *mut DisplayBackendsHandle) {
    if !handle.is_null() {
        drop(unsafe { Box::from_raw(handle) });
    }
}
