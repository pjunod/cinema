#ifndef PLURX_DV_RGB48_H
#define PLURX_DV_RGB48_H

#include <math.h>
#include <stdint.h>
#include <stdbool.h>
#include <stddef.h>
#include <string.h>
#if defined(__SSE2__)
#include <emmintrin.h>
#endif

// Keep the native float upload identical to scalar division, including the
// active rounding mode. Integer storage normalization can change FEL's exact
// zero residual, so every input code is still divided by 1023 before upload.
static inline bool dv_unpack_10bit_plane(const uint16_t *input, size_t count,
                                        float *output) {
  size_t at = 0;
#if defined(__SSE2__)
  const __m128i zero = _mm_setzero_si128();
  const __m128i outside = _mm_set1_epi16((short)0xfc00);
  const __m128 divisor = _mm_set1_ps(1023.0f);
  for (; count - at >= 8; at += 8) {
    __m128i codes = _mm_loadu_si128((const __m128i *)(input + at));
    if (_mm_movemask_epi8(_mm_cmpeq_epi16(_mm_and_si128(codes, outside), zero)) != 0xffff)
      return false;
    _mm_storeu_ps(output + at, _mm_div_ps(
        _mm_cvtepi32_ps(_mm_unpacklo_epi16(codes, zero)), divisor));
    _mm_storeu_ps(output + at + 4, _mm_div_ps(
        _mm_cvtepi32_ps(_mm_unpackhi_epi16(codes, zero)), divisor));
  }
#endif
  for (; at < count; at++) {
    if (input[at] > 1023)
      return false;
    output[at] = input[at] / 1023.0f;
  }
  return true;
}

// The caller validates finite input before conversion. Preserve lrintf's
// rounding mode without paying for general NaN-aware fminf/fmaxf calls.
static inline uint16_t dv_pack_finite_rgb48(float value) {
  if (value < 0)
    value = 0;
  if (value > 1)
    value = 1;
  return (uint16_t)lrintf(value * 65535);
}

// Preserve the scalar conversion, including the active rounding mode and
// rejection of nonfinite alpha. Loads may be unaligned; stores never extend
// beyond the six bytes owned by a pixel. A refused frame is never published.
static inline bool dv_pack_rgba_rgb48(const float *rgba, size_t pixels,
                                     uint8_t *rgb, size_t capacity) {
  if (pixels > SIZE_MAX / 6 || capacity < pixels * 6)
    return false;
  for (size_t pixel = 0; pixel < pixels; pixel++) {
#if defined(__SSE2__)
    __m128 value = _mm_loadu_ps(rgba + 4 * pixel);
    __m128i exponent = _mm_and_si128(_mm_castps_si128(value),
                                   _mm_set1_epi32(0x7f800000));
    if (_mm_movemask_epi8(_mm_cmpeq_epi32(exponent,
                                        _mm_set1_epi32(0x7f800000))))
      return false;
    value = _mm_min_ps(_mm_set1_ps(1.0f),
                       _mm_max_ps(_mm_setzero_ps(), value));
    __m128i code = _mm_cvtps_epi32(_mm_mul_ps(value, _mm_set1_ps(65535.0f)));
    code = _mm_sub_epi32(code, _mm_set1_epi32(32768));
    code = _mm_xor_si128(_mm_packs_epi32(code, code),
                         _mm_set1_epi16((short)0x8000));
    uint32_t rg = (uint32_t)_mm_cvtsi128_si32(code);
    uint16_t b = (uint16_t)_mm_extract_epi16(code, 2);
    memcpy(rgb + 6 * pixel, &rg, sizeof(rg));
    memcpy(rgb + 6 * pixel + 4, &b, sizeof(b));
#else
    for (size_t component = 0; component < 4; component++)
      if (!isfinite(rgba[4 * pixel + component]))
        return false;
    for (size_t component = 0; component < 3; component++) {
      uint16_t code = dv_pack_finite_rgb48(rgba[4 * pixel + component]);
      rgb[6 * pixel + 2 * component] = (uint8_t)code;
      rgb[6 * pixel + 2 * component + 1] = (uint8_t)(code >> 8);
    }
#endif
  }
  return true;
}
#endif
