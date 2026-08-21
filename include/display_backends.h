/* SPDX-License-Identifier: GPL-3.0-or-later */

#ifndef DISPLAY_BACKENDS_H
#define DISPLAY_BACKENDS_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

enum display_backends_kind {
  DISPLAY_BACKENDS_SSD1306_I2C = 0,
  DISPLAY_BACKENDS_SH1106_I2C = 1,
  DISPLAY_BACKENDS_SH1106_SPI = 2,
  DISPLAY_BACKENDS_ST7789_SPI = 3,
};

typedef struct DisplayBackendsHandle DisplayBackendsHandle;

/* The library duplicates bus_fd and control_fd; ownership remains with caller. */
int display_backends_create(uint32_t backend, int bus_fd, int control_fd,
                            DisplayBackendsHandle **output);
int display_backends_write_trezor_frame(DisplayBackendsHandle *handle,
                                        const uint8_t *framebuffer,
                                        size_t length);
int display_backends_shutdown(DisplayBackendsHandle *handle);
void display_backends_destroy(DisplayBackendsHandle *handle);

#ifdef __cplusplus
}
#endif

#endif
