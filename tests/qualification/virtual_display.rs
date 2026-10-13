//! Wire-contract tests use the production message builder and the real viewer.
#![allow(dead_code)]
#[path = "../../../raspberry-pi-i2c-target/src/display_protocol.rs"]
mod viewer;

use crate::{Backend, DisplayConverter, MONO1_FRAME_SIZE, oled_i2c_messages};
use viewer::{DisplayController, DisplayParser};

fn feed(parser: &mut DisplayParser, bytes: &[u8], chunk: usize) {
    for input in bytes.chunks(chunk) {
        let mut update = parser.push(input);
        assert_eq!(update.discarded_bytes, 0);
        while update.completed_frames != 0 || update.partial_frame_boundary {
            update = parser.push(&[]);
            assert_eq!(update.discarded_bytes, 0);
        }
    }
}

fn wire_update(converter: &mut DisplayConverter, backend: Backend, frame: &[u8]) -> Vec<Vec<u8>> {
    let rect = converter.prepare_native(frame).unwrap().unwrap();
    let messages = oled_i2c_messages(backend, rect, converter.next_frame());
    converter.commit();
    messages
}

#[test]
fn partial_updates_survive_every_read_split_and_merged_records() {
    for (backend, controller) in [
        (Backend::Ssd1306I2c, DisplayController::Ssd1306),
        (Backend::Sh1106I2c, DisplayController::Sh1106),
    ] {
        // Include bytes that look exactly like address/data control sequences.
        let pattern = [
            0x00, 0xb0, 0x02, 0x10, 0x40, 0x00, 0x21, 0x7f, 0x22, 0x80, 0xff,
        ];
        let mut frame = [0; MONO1_FRAME_SIZE];
        for (i, byte) in frame.iter_mut().enumerate() {
            *byte = pattern[i % pattern.len()];
        }
        let mut converter = DisplayConverter::new(backend);
        let first = wire_update(&mut converter, backend, &frame).concat();
        frame[2 * 128 + 10] ^= 0xff;
        let second = wire_update(&mut converter, backend, &frame);
        // One changed byte must stay one changed byte on the I2C wire.
        assert_eq!(second.len(), 2);
        assert_eq!(second[1].len(), 2);
        let mut stream = first;
        stream.extend(second.concat());
        frame[7 * 128 + 127] ^= 0xff;
        stream.extend(wire_update(&mut converter, backend, &frame).concat());
        // Exercise a low-page wrap, multiple changed pages, and an x=0 edge.
        frame[0] ^= 0xff;
        frame[3 * 128 + 12] ^= 0xff;
        stream.extend(wire_update(&mut converter, backend, &frame).concat());
        for chunk in [1, 2, 3, 5, 7, 13, 127, 128, 129, 512, 8192] {
            let mut parser = DisplayParser::new(controller);
            feed(&mut parser, &stream, chunk);
            assert_eq!(parser.framebuffer(), &frame, "{backend:?}, chunk={chunk}");
        }
        // Every single possible split, not just representative chunk sizes.
        for split in 0..=stream.len() {
            let mut parser = DisplayParser::new(controller);
            feed(&mut parser, &stream[..split], 8192);
            feed(&mut parser, &stream[split..], 8192);
            assert_eq!(parser.framebuffer(), &frame, "{backend:?}, split={split}");
        }
        assert!(converter.prepare_native(&frame).unwrap().is_none());
    }
}

#[test]
fn native_sh1106_commands_and_viewer_write_identical_pixels() {
    let backend = Backend::Sh1106I2c;
    let mut converter = DisplayConverter::new(backend);
    converter.assume_blank();
    let mut native_ram = [0u8; 8 * 132];
    let mut frame = [0; MONO1_FRAME_SIZE];
    let mut parser = DisplayParser::new(DisplayController::Sh1106);
    for (page, x, width) in [(0, 0, 1), (2, 10, 3), (7, 127, 1), (1, 0, 128)] {
        for col in x..x + width {
            frame[page * 128 + col] = (col as u8).wrapping_add(0x40);
        }
        let messages = wire_update(&mut converter, backend, &frame);
        for pair in messages.as_chunks::<2>().0 {
            let command = &pair[0];
            assert_eq!(command.len(), 6);
            assert_eq!(command[0], 0x00);
            let page = (command[1] & 7) as usize;
            let mut column = 0usize;
            // Model the documented native commands, with no emulator framing knowledge.
            for &byte in &command[2..] {
                match byte {
                    0x00..=0x0f => column = (column & 0xf0) | byte as usize,
                    0x10..=0x1f => column = (column & 0x0f) | ((byte as usize & 15) << 4),
                    _ => panic!("not a native column-address command"),
                }
            }
            assert_eq!(pair[1][0], 0x40);
            for &pixel in &pair[1][1..] {
                native_ram[page * 132 + column] = pixel;
                column += 1;
            }
        }
        feed(&mut parser, &messages.concat(), 1);
        for page in 0..8 {
            assert_eq!(
                &native_ram[page * 132 + 2..page * 132 + 130],
                &frame[page * 128..(page + 1) * 128]
            );
        }
        assert_eq!(parser.framebuffer(), &frame);
    }
}

/// Opt-in real-bus qualification, targeting only the OLED address 0x3c.
/// Pair this with virtual-display and tests/sdl_capture.c on the target Pi.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires an explicitly selected lab I2C bus and running virtual display"]
fn physical_i2c_display_updates() {
    use std::{fs::OpenOptions, io::Write, os::fd::AsRawFd, thread, time::Duration};
    let path = std::env::var("DISPLAY_TEST_BUS").expect("select DISPLAY_TEST_BUS explicitly");
    let controller = std::env::var("DISPLAY_TEST_CONTROLLER").expect("select ssd1306 or sh1106");
    let backend = match controller.as_str() {
        "ssd1306" => Backend::Ssd1306I2c,
        "sh1106" => Backend::Sh1106I2c,
        _ => panic!("unsupported display controller"),
    };
    let expected =
        std::env::var("DISPLAY_TEST_EXPECTED").expect("select an expected-frame output file");
    let mut bus = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    // Cooperate with other clients on this physical bus for the whole test.
    assert_eq!(unsafe { libc::flock(bus.as_raw_fd(), libc::LOCK_EX) }, 0);
    assert_eq!(
        unsafe { libc::ioctl(bus.as_raw_fd(), 0x0703, 0x3c as libc::c_ulong) },
        0
    );
    let init = match backend {
        Backend::Ssd1306I2c => crate::SSD1306_INIT,
        Backend::Sh1106I2c => crate::SH1106_INIT,
        _ => unreachable!(),
    };
    bus.write_all(init).unwrap();
    if backend == Backend::Sh1106I2c {
        bus.write_all(crate::SH1106_DISPLAY_ON).unwrap();
    }
    let mut converter = DisplayConverter::new(backend);
    let mut frame = [0; MONO1_FRAME_SIZE];
    for (i, byte) in frame.iter_mut().enumerate() {
        *byte = (i as u8).wrapping_mul(37).wrapping_add(0x40);
    }
    let mut messages_sent = 0usize;
    let mut bytes_sent = init.len() + if backend == Backend::Sh1106I2c { 2 } else { 0 };
    for step in 0..=200 {
        if step != 0 {
            // Repeated small writes span all pages and both horizontal edges.
            let index = (step * 127 + step / 8) % MONO1_FRAME_SIZE;
            frame[index] ^= 0x5a;
        }
        let messages = wire_update(&mut converter, backend, &frame);
        if step != 0 {
            assert_eq!(messages.len(), 2);
            assert_eq!(messages[1].len(), 2);
        }
        for message in messages {
            bus.write_all(&message).unwrap();
            messages_sent += 1;
            bytes_sent += message.len();
        }
        thread::sleep(Duration::from_millis(2));
    }
    std::fs::write(expected, frame).unwrap();
    println!(
        "{controller}: 201 updates, {messages_sent} messages, {bytes_sent} bytes; last 200 updates each have one pixel byte"
    );
}
