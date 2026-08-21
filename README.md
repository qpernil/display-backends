# Display Backends

`display-backends` translates the 128x64 one-bit framebuffer used by Trezor
One firmware into traffic for physical displays attached to Linux. It supports
SSD1306 over I2C, SH1106 over I2C or SPI, and ST7789 over SPI.

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
`target/release/libdisplay_backends.a`.

`virtual-trezor` expects this repository and its own checkout to be sibling
directories. Set `DISPLAY_BACKENDS_DIR` when using another layout.

## Hardware demo

The demo uses the same framebuffer conversion, controller initialization, and
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
