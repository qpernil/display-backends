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

enum display_backends_pixel_format {
  /* One bit per pixel, MSB first; page and x byte order are reversed. */
  DISPLAY_BACKENDS_MONO1_MSB_REVERSE_PAGE = 0,
  /* One unsigned grayscale byte per pixel, row-major. */
  DISPLAY_BACKENDS_GRAY8 = 1,
};

typedef struct DisplayBackendsHandle DisplayBackendsHandle;

/*
 * The source format is fixed for the lifetime of the handle. For Gray8,
 * stride is bytes per row. For Mono1MsbReversePage, stride is bytes per page.
 * The library duplicates bus_fd and control_fd; ownership remains with caller.
 */
int display_backends_create(uint32_t backend, uint32_t pixel_format,
                            size_t width, size_t height, size_t stride,
                            int bus_fd, int control_fd,
                            DisplayBackendsHandle **output);
int display_backends_write_frame(DisplayBackendsHandle *handle,
                                 const uint8_t *framebuffer, size_t length);
int display_backends_shutdown(DisplayBackendsHandle *handle);
void display_backends_destroy(DisplayBackendsHandle *handle);

#ifdef __cplusplus
}
#endif

#endif
