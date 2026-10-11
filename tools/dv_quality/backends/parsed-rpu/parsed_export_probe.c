// Actual libdovi parser with synthetic textures: not an encoded HEVC association proof.
#include <float.h>
#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <libdovi/rpu_parser.h>
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

// Bounded parser-to-shader projection. Only the generated identity P7 controls
// are admitted here; these guards are not a production profile capability.
static void map_parsed_rpu(const char *path, struct pl_dovi_metadata *out)
{
    FILE *file = fopen(path, "rb");
    require(file != NULL, "RPU input exists");
    uint8_t bytes[65537];
    size_t size = fread(bytes, 1, sizeof(bytes), file);
    bool read_ok = !ferror(file);
    int close_ok = fclose(file);
    require(read_ok && close_ok == 0 && size > 0 && size <= 65536, "bounded RPU read");
    DoviRpuOpaque *rpu = dovi_parse_unspec62_nalu(bytes, size);
    require(rpu != NULL, "RPU parser allocation");
    const char *error = dovi_rpu_get_error(rpu);
    if (error) {
        fprintf(stderr, "RPU parser rejected input: %s\n", error);
        dovi_rpu_free(rpu);
        exit(EXIT_FAILURE);
    }
    const DoviRpuDataHeader *header = dovi_rpu_get_header(rpu);
    require(header != NULL, "parsed RPU header");
    require(header->guessed_profile == 7 && header->el_type &&
            strcmp(header->el_type, "FEL") == 0, "synthetic P7 FEL profile");
    require(!header->disable_residual_flag && !header->use_prev_vdr_rpu_flag &&
            header->vdr_dm_metadata_present_flag && header->coefficient_data_type == 0 &&
            header->coefficient_log2_denom == 23 && header->bl_bit_depth_minus8 == 2 &&
            header->el_bit_depth_minus8 == 2 && header->vdr_bit_depth_minus8 == 4,
            "bounded fresh P7 control header subset");
    const DoviRpuDataMapping *mapping = dovi_rpu_get_data_mapping(rpu);
    const DoviVdrDmData *dm = dovi_rpu_get_vdr_dm_data(rpu);
    require(mapping && dm && mapping->nlq && mapping->nlq_method_idc == 0 &&
            mapping->nlq_num_pivots_minus2 == 0, "LINEAR_DZ mapping and DM metadata");
    require(dm->signal_eotf == 65535 && dm->signal_full_range_flag == 1,
            "PQ full-range reconstructed control representation");
    memset(out, 0, sizeof(*out));
    double denominator = (double) (1ULL << header->coefficient_log2_denom);
    for (int component = 0; component < 3; component++) {
        const DoviReshapingCurve *curve = &mapping->curves[component];
        const DoviPolynomialCurve *poly = curve->polynomial;
        require(curve->mapping_idc == 0 && curve->pivots.len == 2 &&
                curve->pivots.data && curve->pivots.data[0] == 0 &&
                curve->pivots.data[1] == 1023 && poly &&
                poly->poly_order_minus1.len == 1 && poly->poly_order_minus1.data &&
                poly->poly_order_minus1.data[0] == 0 && poly->poly_coef_int.len == 1 &&
                poly->poly_coef.len == 1 && poly->poly_coef_int.list &&
                poly->poly_coef.list && poly->poly_coef_int.list[0] &&
                poly->poly_coef.list[0] && poly->poly_coef_int.list[0]->len == 2 &&
                poly->poly_coef.list[0]->len == 2,
                "single identity polynomial segment");
        const int64_t *integer = poly->poly_coef_int.list[0]->data;
        const uint64_t *fraction = poly->poly_coef.list[0]->data;
        require(integer && fraction && integer[0] == 0 && integer[1] == 1 &&
                fraction[0] == 0 && fraction[1] == 0,
                "identity polynomial coefficient control");
        out->comp[component].num_pivots = 2;
        out->comp[component].pivots[1] = curve->pivots.data[1] / 1023.0f;
        out->comp[component].poly_coeffs[0][1] =
            integer[1] + fraction[1] / denominator;
        const DoviRpuDataNlq *nlq = mapping->nlq;
        // This libplacebo revision does not apply vdr_in_max clipping. Reject
        // controls that require it rather than pretending the operation exists.
        require(nlq->vdr_in_max_int[component] == 1 &&
                nlq->vdr_in_max[component] == 0,
                "unsupported bounded vdr_in_max residual clipping");
        require(nlq->nlq_offset[component] <= 1023 &&
                nlq->linear_deadzone_slope_int[component] == 0 &&
                nlq->linear_deadzone_threshold_int[component] == 0 &&
                nlq->linear_deadzone_slope[component] < (1ULL << 23) &&
                nlq->linear_deadzone_threshold[component] < (1ULL << 23),
                "bounded fractional NLQ parameters");
        double slope = nlq->linear_deadzone_slope[component];
        double threshold = nlq->linear_deadzone_threshold[component];
        out->nlq[component].offset = nlq->nlq_offset[component] / 1023.0f;
        out->nlq[component].deadzone_slope = 1023.0 * slope / denominator;
        out->nlq[component].deadzone_threshold = (threshold - slope / 2) / denominator;
    }
    const int16_t nonlinear[] = {
        dm->ycc_to_rgb_coef0, dm->ycc_to_rgb_coef1, dm->ycc_to_rgb_coef2,
        dm->ycc_to_rgb_coef3, dm->ycc_to_rgb_coef4, dm->ycc_to_rgb_coef5,
        dm->ycc_to_rgb_coef6, dm->ycc_to_rgb_coef7, dm->ycc_to_rgb_coef8,
    };
    const int16_t linear[] = {
        dm->rgb_to_lms_coef0, dm->rgb_to_lms_coef1, dm->rgb_to_lms_coef2,
        dm->rgb_to_lms_coef3, dm->rgb_to_lms_coef4, dm->rgb_to_lms_coef5,
        dm->rgb_to_lms_coef6, dm->rgb_to_lms_coef7, dm->rgb_to_lms_coef8,
    };
    const uint32_t offsets[] = {
        dm->ycc_to_rgb_offset0, dm->ycc_to_rgb_offset1, dm->ycc_to_rgb_offset2,
    };
    for (int component = 0; component < 3; component++) {
        out->nonlinear_offset[component] = offsets[component] / (double) (1ULL << 28);
        for (int column = 0; column < 3; column++) {
            out->nonlinear.m[component][column] = nonlinear[3 * component + column] / 8192.0f;
            out->linear.m[component][column] = linear[3 * component + column] / 16384.0f;
        }
    }
    out->nlq_active = true;
    printf("{\"kind\":\"parsed_rpu\",\"bytes\":%zu,\"profile\":7,\"el_type\":\"FEL\","
           "\"parser_error\":false,\"bl_depth\":10,\"el_depth\":10,\"vdr_depth\":12,"
           "\"mapping_segments\":1,\"nlq_method\":\"LINEAR_DZ\",\"creative_l2_count\":%zu,"
           "\"creative_l8_count\":%zu,\"creative_trims_applied\":false}\n",
           size, dm->dm_data.level2.len, dm->dm_data.level8.len);
    dovi_rpu_free_vdr_dm_data(dm);
    dovi_rpu_free_data_mapping(mapping);
    dovi_rpu_free_header(header);
    dovi_rpu_free(rpu);
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

int main(int argc, char **argv)
{
    require(argc == 2 || argc == 3, "usage: parsed_export_probe RPU.nal [require-missing-el]");
    struct pl_dovi_metadata parsed;
    map_parsed_rpu(argv[1], &parsed);
    if (argc == 3) {
        require(strcmp(argv[2], "require-missing-el") != 0, "missing EL rejected for requested residual reconstruction");
        require(false, "unknown control mode");
    }
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
           "\"rpu_input\":\"libdovi parsed UNSPEC62 NAL; guarded synthetic subset\"}\n",
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
                        (component == 0 ? 400.0f + 2.0f * x + y : 512.0f) / 1023.0f;
                    int shifted_x = test == 3 ? (x + 1) % WIDTH : x;
                    enhancement[4 * pixel + component] =
                        (test == 0 ? 512.0f :
                         (component == 0 ? ((shifted_x + y) % 2 ? 528.0f : 496.0f) : 512.0f)) / 1023.0f;
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

        struct pl_dovi_metadata metadata = parsed;
        metadata.nlq_active = test != 4;
        struct pl_color_repr representation = {
            .sys = PL_COLOR_SYSTEM_DOLBYVISION,
            .levels = PL_COLOR_LEVELS_FULL,
            .bits = {.sample_depth = 10, .color_depth = 10},
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
                .bits = {.sample_depth = 10, .color_depth = 10},
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
               "\"rpu_parsed\":true,\"direct_dispatch_ok\":true,"
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
