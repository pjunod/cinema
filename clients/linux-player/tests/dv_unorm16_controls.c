#include <stdbool.h>
#include <stdint.h>
#include <inttypes.h>
#include <math.h>
#include <stdio.h>
#include <string.h>

typedef uint32_t uint;
_Static_assert(sizeof(float) == 4, "binary32 required");

static uint floatBitsToUint(float x)
{
    uint bits;
    memcpy(&bits, &x, sizeof bits);
    return bits;
}
static float from_bits(uint bits)
{
    float x;
    memcpy(&x, &bits, sizeof x);
    return x;
}

// Compute the two output words independently from the reference's uint64_t product.
// The GLSL code calls umulExtended with a 24-bit value and a 16-bit constant.
static void split_product(uint a, uint b, uint *high, uint *low)
{
    uint low_product = (a & 65535u) * b;
    uint high_product = (a >> 16) * b;
    *low = low_product + (high_product << 16);
    *high = (high_product >> 16) + (*low < low_product);
}
#define umulExtended(a, b, hi, lo) split_product((a), (b), &(hi), &(lo))
#include "exact_unorm16.glsl"

// Independent wide rational oracle. Uses div/mod and twice-remainder comparisons.
static uint reference(uint bits)
{
    float x = from_bits(bits);
    if (!(x > 0.0f)) return 0;
    if (x >= 1.0f) return 65535;
    if (x < 0x1p-17f) return 0;
    uint64_t numerator = (UINT64_C(8388608) + bits % UINT32_C(8388608)) * UINT64_C(65535);
    unsigned exponent = (bits / UINT32_C(8388608)) % 256;
    uint64_t denominator = UINT64_C(1) << (150 - exponent);
    uint quotient = (uint)(numerator / denominator);
    uint64_t twice_remainder = (numerator % denominator) * 2;
    return quotient + (twice_remainder > denominator ||
                       (twice_remainder == denominator && quotient % 2 != 0));
}

static int exhaustive(void)
{
    unsigned long long values = 0, errors = 0, up = 0, down = 0;
    puts("{\"exponents\":[");
    for (uint exponent = 110; exponent < 127; exponent++) {
        unsigned exponent_errors = 0, exponent_up = 0, exponent_down = 0;
        uint first = exponent << 23;
        for (uint i = 0; i < (1u << 23); i++) {
            uint bits = first + i;
            float x = from_bits(bits);
            uint exact = reference(bits);
            uint code = exact_unorm16_code(x);
            float rounded_product = x * 65535.0f;
            uint float_product_code = (uint)nearbyintf(rounded_product);
            exponent_errors += code != exact;
            exponent_up += float_product_code > exact;
            exponent_down += float_product_code < exact;
        }
        printf("{\"e\":%d,\"tested\":8388608,\"mismatches\":%u,"
               "\"float_product_too_high\":%u,\"float_product_too_low\":%u}%s\n",
               (int)exponent - 127, exponent_errors, exponent_up, exponent_down,
               exponent == 126 ? "" : ",");
        values += 1u << 23;
        errors += exponent_errors;
        up += exponent_up;
        down += exponent_down;
    }
    printf("],\"tested\":%llu,\"mismatches\":%llu,\"float_product_too_high\":%llu,"
           "\"float_product_too_low\":%llu}\n", values, errors, up, down);
    return errors ? 1 : 0;
}

int main(void)
{
    if (exact_unorm16_code(-INFINITY) != 0 || exact_unorm16_code(INFINITY) != 65535 ||
        exact_unorm16_code(NAN) != 0 || exact_unorm16_code(-0.0f) != 0 ||
        exact_unorm16_code(0.0f) != 0 || exact_unorm16_code(1.0f) != 65535)
        return 1;
    return exhaustive();
}
