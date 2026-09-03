# Display Backends

`display-backends` translates application framebuffers into traffic for
physical displays attached to Linux. It supports SSD1306 over I2C, SH1106 over
I2C or SPI, and ST7789 over SPI.

The API has two layers. `write_native_frame` sends pixels already composed in
the controller's native format, with no scaling or pixel conversion:

| Controller | Native frame |
| --- | --- |
| SSD1306 / SH1106 | 128x64 `Mono1MsbReversePage` |
| ST7789 | 240x240 `Rgb565Be` |

The optional `write_frame` conversion layer accepts a producer format, width,
height, and stride, then aspect-fits and centers it in the native frame. The
supported producer formats are:

| Format | Layout |
| --- | --- |
| `Mono1MsbReversePage` | One bit per pixel, MSB first, with page and x-byte order reversed |
| `Mono8` | One unsigned monochrome-intensity byte per pixel, row-major |
| `Rgb565Be` | Two-byte RGB565 pixels, most significant byte first, row-major |

Every controller accepts all three formats through the conversion layer.
SSD1306 and SH1106 threshold intensity at 128 into native 1-bit pages. ST7789
maps monochrome intensity to RGB565 grayscale. Native frames bypass all of that
work.

Each backend keeps the last successfully displayed native frame and compares it
with the next complete native frame. An unchanged frame produces no bus traffic.
For ST7789, the backend sends the smallest pixel-aligned bounding rectangle that
contains every change. SSD1306 and SH1106 use the smallest changed column range
and the changed eight-pixel controller pages. The new frame becomes current only
after the entire transfer succeeds, so an interrupted update is retried against
the last known-good display state.

The library never opens hardware paths. Its Rust and C APIs accept already-open
bus descriptors and exact GPIO line-request descriptors. It duplicates those
descriptors with close-on-exec and owns only the duplicates. This makes it
suitable for workers whose capabilities are opened by a privileged supervisor.

## Activity indicators

The device-neutral `indicator` module schedules one logical indicator bit and
calls an `IndicatorRenderer` supplied by the application. It does not know what
the bit looks like or control the containing display's power. A renderer may
select complete on/off frames, update one region, or drive a physical LED.

An activity handle marks the worker's single command as active and increments a
monotonic command epoch when work starts. The epoch preserves evidence of a
command that starts and finishes during a synchronous frame write. Activity
that arrives while a pulse is already visible may retain one additional pulse;
further activity coalesces rather than building a replay queue. Policies select
the busy cadence, minimum edge interval, and an idle behavior: stopped off or a
blink cadence repeated a finite number of times or forever. A finite count runs
after activity; a forever cadence also starts when the indicator is enabled. A
scoped attention cadence can temporarily override command and idle scheduling.

Activity always establishes an on phase. If blinking idle is already on, the
scheduler inserts a minimum-length off separator before activity turns on. A
short command finishes off; a sustained command continues directly into the
busy cadence. Any configured idle blinking then restarts from off.

Cadence and minimum-edge intervals are measured from the start of one renderer
call to the start of the next. Rendering time is therefore part of the interval,
not an added delay. A backend that takes longer than an interval becomes the
natural rate limit; the scheduler does not replay missed transitions.

## Build and test

Rust 1.94.0 or later is required.

```sh
cargo test --locked
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo fmt --check
```

The C ABI is declared in `include/display_backends.h`; Linux builds produce
both `target/release/libdisplay_backends.a` and
`target/release/libdisplay_backends.so`. Native workers without another Rust
runtime can use the static archive. Firmware emulators that already embed a
Rust runtime use the shared library so panic/runtime symbols remain isolated.
The API is device-neutral:

```c
display_backends_create(backend, bus_fd, control_fd, &handle);
display_backends_write_native_frame(handle, native_bytes, native_length);
display_backends_write_frame(handle, pixel_format, width, height, stride,
                             producer_bytes, producer_length);
```

`virtual-trezor` expects this repository and its own checkout to be sibling
directories. Set `DISPLAY_BACKENDS_DIR` when using another layout.

## Hardware demo

The demo uses `embedded-graphics` to compose directly in each controller's
native format: full-color RGB565 for ST7789 and packed 1-bit pixels for the
monochrome OLEDs. It therefore tests the native transport layer without an
intermediate conversion. It alone opens hardware paths so it can be run
directly for diagnostics.

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

The library owns native display mechanics—controller setup,
reset/D/C/backlight operations, SPI/I2C writes, clearing, and display-off—and
an optional producer-to-native conversion layer. Its indicator module owns only
logical activity timing. Composition, display power, resource discovery,
process lifecycle, retry policy, and logging remain in the caller.

## Future work

The [TCP display backend plan](docs/future-tcp-display-backend.md) describes a
transactional remote backend for rendering frames from another Linux machine
on displays physically attached to the Pi.
