// Synthetic direct-texture mechanics probe. This is not encoded P7 or parsed-RPU proof.
#include <float.h>
#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <libplacebo/dispatch.h>
#include <libplacebo/renderer.h>
#include <libplacebo/shaders/colorspace.h>
#include <libplacebo/shaders/sampling.h>
#include <libplacebo/utils/upload.h>
#include <libplacebo/vulkan.h>

#define WIDTH 16
#define HEIGHT 16
#define FLOAT_COUNT (WIDTH * HEIGHT * 4)

static void require(bool condition, const char *message)
{
    if (!condition) {
        fprintf(stderr, "Probe failed: %s\n", message);
        exit(EXIT_FAILURE);
    }
}

static void save(const char *name, const void *data, size_t length)
{
    FILE *file = fopen(name, "wb");
    require(file != NULL, "opening output artifact");
    bool written = fwrite(data, 1, length, file) == length;
    int closed = fclose(file);
    require(written && closed == 0, "writing and closing output artifact");
}

static void probe_log(void *priv, enum pl_log_level level, const char *message)
{
    (void) priv;
    fprintf(stderr, "level%d: %s\n", level, message);
}

static const double lms_to_rgb[3][3] = {
    { 3.06441879, -2.16597676, 0.10155818 },
    {-0.65612108,  1.78554118, -0.12943749 },
    { 0.01736321, -0.04725154, 1.03004253 },
};

// This synthetic matrix cancels the renderer's HPE LMS-to-RGB transform.
// Combined with identity reshaping, positive in-gamut input has the analytic
// result BL + residual. It does not describe an arbitrary commercial RPU.
static void inverse_matrix(pl_matrix3x3 *out)
{
    double matrix[3][6] = {0};
    for (int row = 0; row < 3; row++) {
        for (int column = 0; column < 3; column++) {
            matrix[row][column] = lms_to_rgb[row][column];
            matrix[row][column + 3] = row == column;
        }
    }
    for (int row = 0; row < 3; row++) {
        double divisor = matrix[row][row];
        require(divisor != 0, "nonzero synthetic matrix pivot");
        for (int column = 0; column < 6; column++) {
            matrix[row][column] /= divisor;
        }
        for (int other = 0; other < 3; other++) {
            if (other == row) {
                continue;
            }
            double factor = matrix[other][row];
            for (int column = 0; column < 6; column++) {
                matrix[other][column] -= factor * matrix[row][column];
            }
        }
    }
    for (int row = 0; row < 3; row++) {
        for (int column = 0; column < 3; column++) {
            out->m[row][column] = matrix[row][column + 3];
        }
    }
}

static void save_rgb48le(const char *name, const float *rgba)
{
    uint8_t output[WIDTH * HEIGHT * 6];
    for (int pixel = 0; pixel < WIDTH * HEIGHT; pixel++) {
        for (int component = 0; component < 3; component++) {
            float value = fminf(1, fmaxf(0, rgba[4 * pixel + component]));
            uint16_t code = (uint16_t) lrintf(value * 65535);
            size_t offset = (size_t) (3 * pixel + component) * 2;
            output[offset] = (uint8_t) code;
            output[offset + 1] = (uint8_t) (code >> 8);
        }
    }
    save(name, output, sizeof(output));
}

static void save_frame(const char *test, const char *stage, const float *rgba)
{
    for (size_t index = 0; index < FLOAT_COUNT; index++) {
        require(isfinite(rgba[index]), "finite input/output frame components");
    }
    char path[160];
    int length = snprintf(path, sizeof(path), "outputs/%s-%s.rgba32f", test, stage);
    require(length > 0 && (size_t) length < sizeof(path), "float output path");
    save(path, rgba, FLOAT_COUNT * sizeof(float));
    length = snprintf(path, sizeof(path), "outputs/%s-%s.rgb48le", test, stage);
    require(length > 0 && (size_t) length < sizeof(path), "integer output path");
    save_rgb48le(path, rgba);
}

int main(void)
{
    const uint16_t endian = 1;
    require(*(const uint8_t *) &endian == 1, "little-endian RGBA32F host");
    require(sizeof(float) == 4 && FLT_RADIX == 2 && FLT_MANT_DIG == 24,
            "IEEE754 binary32 host representation");

    pl_log log = pl_log_create(PL_API_VER, pl_log_params(
        .log_cb = probe_log,
        .log_level = PL_LOG_DEBUG,
    ));
    require(log != NULL, "logging context");
    pl_vulkan vk = pl_vulkan_create(log, pl_vulkan_params(.allow_software = true));
    require(vk != NULL, "software Vulkan device");
    pl_gpu gpu = vk->gpu;
    pl_fmt format = pl_find_fmt(gpu, PL_FMT_FLOAT, 4, 32, 32,
                               PL_FMT_CAP_RENDERABLE | PL_FMT_CAP_SAMPLEABLE |
                               PL_FMT_CAP_HOST_READABLE);
    require(format != NULL, "RGBA32F format");
    require(format->type == PL_FMT_FLOAT && !format->opaque &&
            format->num_components == 4 && format->texel_size == 16,
            "host-compatible packed float texture");
    for (int component = 0; component < 4; component++) {
        require(format->host_bits[component] == 32 &&
                format->sample_order[component] == component,
                "RGBA32F component order and precision");
    }
    printf("{\"kind\":\"environment\",\"api\":%d,\"output_format\":\"%s\","
           "\"format_pixel_size\":%zu,\"input_domain\":\"synthetic native DV components\","
           "\"rpu_input\":\"structured pl_dovi_metadata; no parser\"}\n",
           PL_API_VER, format->name, format->texel_size);

    const char *tests[] = {"zero", "nonzero", "omitted", "shifted", "disabled"};
    pl_dispatch dispatch = pl_dispatch_create(log, gpu);
    pl_renderer renderer = pl_renderer_create(log, gpu);
    require(dispatch != NULL && renderer != NULL, "dispatch and renderer");

    for (int test = 0; test < 5; test++) {
        float base[FLOAT_COUNT], enhancement[FLOAT_COUNT], output[FLOAT_COUNT];
        for (int y = 0; y < HEIGHT; y++) {
            for (int x = 0; x < WIDTH; x++) {
                int pixel = y * WIDTH + x;
                for (int component = 0; component < 3; component++) {
                    base[4 * pixel + component] =
                        0.25f + 0.02f * component + 0.005f * x + 0.003f * y;
                    int shifted_x = test == 3 ? (x + 1) % WIDTH : x;
                    enhancement[4 * pixel + component] = test == 0 ? 0.5f :
                        ((shifted_x + y + component) % 2 ? 0.625f : 0.375f);
                }
                base[4 * pixel + 3] = enhancement[4 * pixel + 3] = 1;
            }
        }
        save_frame(tests[test], "bl", base);
        save_frame(tests[test], "el", enhancement);
        pl_tex base_texture = pl_tex_create(gpu, pl_tex_params(
            .w = WIDTH, .h = HEIGHT, .format = format,
            .sampleable = true, .initial_data = base,
        ));
        pl_tex enhancement_texture = pl_tex_create(gpu, pl_tex_params(
            .w = WIDTH, .h = HEIGHT, .format = format,
            .sampleable = true, .initial_data = enhancement,
        ));
        pl_tex output_texture = pl_tex_create(gpu, pl_tex_params(
            .w = WIDTH, .h = HEIGHT, .format = format,
            .renderable = true, .host_readable = true,
        ));
        require(base_texture && enhancement_texture && output_texture, "frame textures");

        struct pl_dovi_metadata metadata = {0};
        for (int component = 0; component < 3; component++) {
            metadata.nonlinear.m[component][component] = 1;
            metadata.comp[component].num_pivots = 2;
            metadata.comp[component].pivots[0] = 0;
            metadata.comp[component].pivots[1] = 1;
            metadata.comp[component].poly_coeffs[0][1] = 1;
            metadata.nlq[component].offset = 0.5f;
            metadata.nlq[component].deadzone_slope = 0.2f;
            metadata.nlq[component].deadzone_threshold = 0;
        }
        inverse_matrix(&metadata.linear);
        metadata.nlq_active = test != 4;
        struct pl_color_repr representation = {
            .sys = PL_COLOR_SYSTEM_DOLBYVISION,
            .levels = PL_COLOR_LEVELS_FULL,
            .dovi = &metadata,
        };

        pl_dispatch_reset_frame(dispatch);
        pl_shader shader = pl_dispatch_begin(dispatch);
        // Subshaders need distinct identifier namespaces; using two ordinary
        // dispatch_begin calls collides and composition can be omitted.
        pl_shader enhancement_shader = pl_shader_alloc(log, pl_shader_params(
            .gpu = gpu, .id = 200,
        ));
        require(shader && enhancement_shader, "frame shaders");
        require(pl_shader_sample_direct(shader, pl_sample_src(
            .tex = base_texture, .new_w = WIDTH, .new_h = HEIGHT, .components = 3,
        )), "base sample shader");
        require(pl_shader_sample_direct(enhancement_shader, pl_sample_src(
            .tex = enhancement_texture, .new_w = WIDTH, .new_h = HEIGHT, .components = 3,
        )), "enhancement sample shader");
        pl_shader_decode_color_ex(shader, pl_color_decode_args(
            .repr = &representation,
            .enhancement_layer = test == 2 ? NULL : enhancement_shader,
        ));
        require(!pl_shader_is_failed(shader), "color decode shader state");
        require(pl_dispatch_finish(dispatch, pl_dispatch_params(
            .shader = &shader, .target = output_texture,
        )), "pre-map decode dispatch");
        pl_shader_free(&enhancement_shader);
        require(pl_tex_download(gpu, pl_tex_transfer_params(
            .tex = output_texture, .ptr = output,
        )), "pre-map frame download");
        save_frame(tests[test], "reconstruction", output);

        struct pl_frame enhancement_frame = {
            .num_planes = 1,
            .planes = {{
                .texture = enhancement_texture, .components = 3,
                .component_mapping = {0, 1, 2},
            }},
            .repr = {.sys = PL_COLOR_SYSTEM_RGB, .levels = PL_COLOR_LEVELS_FULL},
            .color = pl_color_space_hdr10,
        };
        struct pl_frame source = {
            .num_planes = 1,
            .planes = {{
                .texture = base_texture, .components = 3,
                .component_mapping = {0, 1, 2},
            }},
            .repr = {
                .sys = PL_COLOR_SYSTEM_DOLBYVISION,
                .levels = PL_COLOR_LEVELS_FULL, .dovi = &metadata,
            },
            .color = pl_color_space_hdr10,
            .enhancement_layer = test == 2 ? NULL : &enhancement_frame,
        };
        struct pl_frame destination = {
            .num_planes = 1,
            .planes = {{
                .texture = output_texture, .components = 3,
                .component_mapping = {0, 1, 2},
            }},
            .repr = {
                .sys = PL_COLOR_SYSTEM_RGB, .levels = PL_COLOR_LEVELS_FULL,
                .bits = {.color_depth = 32},
            },
            .color = pl_color_space_hdr10,
        };
        source.color.hdr.max_luma = destination.color.hdr.max_luma = 10000;
        source.color.hdr.min_luma = destination.color.hdr.min_luma = 0.005;
        struct pl_color_map_params color_map = pl_color_map_default_params;
        color_map.tone_mapping_function = &pl_tone_map_clip;
        color_map.gamut_mapping = &pl_gamut_map_clip;
        color_map.metadata = PL_HDR_METADATA_NONE;
        struct pl_render_params parameters = pl_render_fast_params;
        parameters.color_map_params = &color_map;
        parameters.dither_params = NULL;
        parameters.peak_detect_params = NULL;
        parameters.color_adjustment = NULL;
        parameters.sigmoid_params = NULL;
        pl_renderer_reset_errors(renderer, NULL);
        require(pl_render_image(renderer, &source, &destination, &parameters),
                "full renderer export");
        struct pl_render_errors errors = pl_renderer_get_errors(renderer);
        require(errors.errors == 0 && errors.num_disabled_hooks == 0,
                "renderer error-free export");
        require(pl_tex_download(gpu, pl_tex_transfer_params(
            .tex = output_texture, .ptr = output,
        )), "full renderer frame download");
        save_frame(tests[test], "rendered", output);
        printf("{\"kind\":\"frame\",\"case\":\"%s\",\"pts\":\"0/1\","
               "\"duration\":\"1/24\",\"el_bound\":%s,\"nlq_active\":%s,"
               "\"rpu_parsed\":false,\"direct_dispatch_ok\":true,"
               "\"render_ok\":true,\"render_errors\":%u,\"qualified_fel\":false}\n",
               tests[test], test == 2 ? "false" : "true",
               metadata.nlq_active ? "true" : "false", errors.errors);
        pl_tex_destroy(gpu, &base_texture);
        pl_tex_destroy(gpu, &enhancement_texture);
        pl_tex_destroy(gpu, &output_texture);
    }
    pl_renderer_destroy(&renderer);
    pl_dispatch_destroy(&dispatch);
    pl_vulkan_destroy(&vk);
    pl_log_destroy(&log);
    require(fflush(stdout) == 0, "flushing per-frame observations");
    return EXIT_SUCCESS;
}
