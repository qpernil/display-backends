# Display Backends

`display-backends` translates application framebuffers into traffic for
physical displays attached to Linux. It supports SSD1306 over I2C, SH1106 over
I2C or SPI, and ST7789 over SPI.

Source pixels and physical controllers are independent. A display handle is
created with one fixed source format, width, height, and stride. The supported
source formats are:

| Format | Layout |
| --- | --- |
| `Mono1MsbReversePage` | One bit per pixel, MSB first, with page and x-byte order reversed |
| `Mono8` | One unsigned monochrome-intensity byte per pixel, row-major |

Every controller accepts both formats. Frames are scaled with preserved aspect
ratio and centered. SSD1306 and SH1106 threshold Mono8 at 128 into their native
1-bit pages. ST7789 converts Mono8 to RGB565 grayscale; one-bit pixels become black or
white RGB565 pixels.

The library never opens hardware paths. Its Rust and C APIs accept already-open
bus descriptors and exact GPIO line-request descriptors. It duplicates those
descriptors with close-on-exec and owns only the duplicates. This makes it
suitable for workers whose capabilities are opened by a privileged supervisor.

## Build and test

Rust 1.85.0 or later is required.

```sh
cargo test --locked
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo fmt --check
```

The C ABI is declared in `include/display_backends.h`; Linux builds produce
`target/release/libdisplay_backends.a`. The API is device-neutral:

```c
display_backends_create(backend, pixel_format, width, height, stride,
                        bus_fd, control_fd, &handle);
display_backends_write_frame(handle, bytes, length);
```

`virtual-trezor` expects this repository and its own checkout to be sibling
directories. Set `DISPLAY_BACKENDS_DIR` when using another layout.

## Hardware demo

The demo uses `embedded-graphics` to compose a native Mono8 framebuffer, then
passes it through the same conversion, controller initialization, and
bus-writing code as library consumers. It alone opens hardware paths so it can
be run directly for diagnostics.

Stop any service that owns the display, then select a backend:

```sh
cargo build --release --locked --features demo --bin display-backends-demo
sudo ./target/release/display-backends-demo st7789-spi
sudo ./target/release/display-backends-demo sh1106-spi
sudo ./target/release/display-backends-demo sh1106-i2c
sudo ./target/release/display-backends-demo ssd1306-i2c
```

Optional bus and GPIO chip paths may follow the backend name. Defaults are
`/dev/spidev0.0` or `/dev/i2c-1`, plus `/dev/gpiochip0`.

Default Raspberry Pi GPIO line order is:

| Backend | Lines |
| --- | --- |
| SH1106 I2C | reset: GPIO25 |
| SH1106 SPI | D/C: GPIO24, reset: GPIO25 |
| ST7789 SPI | D/C: GPIO25, reset: GPIO27, backlight: GPIO24 |

The demo renders 240 frames, reports its measured rate, and waits for Enter
before clearing and releasing the display.

## Ownership boundary

The library owns display protocol and encoding mechanics: controller setup,
reset/D/C/backlight operations, SPI/I2C writes, frame conversion, clearing, and
display-off. Its caller owns resource discovery, process lifecycle, retry
policy, and logging.
