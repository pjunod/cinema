// SPDX-License-Identifier: Apache-2.0
#ifndef PLURX_M7_LIMITS_H
#define PLURX_M7_LIMITS_H

#include <stdbool.h>
#include <stdint.h>

enum {
  M7_MIN_RASTER_DIMENSION = 64,
  M7_MAX_RASTER_WIDTH = 3840,
  M7_MAX_RASTER_HEIGHT = 2160,
  M7_MAX_WINDOW_FRAMES = 48,
};

static inline bool m7_supported_raster(uint32_t width, uint32_t height) {
  return width >= M7_MIN_RASTER_DIMENSION && width <= M7_MAX_RASTER_WIDTH &&
         height >= M7_MIN_RASTER_DIMENSION && height <= M7_MAX_RASTER_HEIGHT &&
         !(width % 2) && !(height % 2);
}

#endif
