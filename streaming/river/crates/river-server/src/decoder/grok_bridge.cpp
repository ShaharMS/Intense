// Compile against the same Grok headers as the packaged shared library.
// Rust sees only this small ABI, never Grok's version-dependent structures.
#include <grok.h>
#include <cstddef>
#include <cstdint>
#include <new>

struct RiverGrokHandle { grk_object* codec; grk_image* image; };
struct RiverGrokPlane {
    const void* data;
    uint32_t width, height, stride, x, y;
    uint8_t precision, signed_samples, sample_bytes;
};

static bool requires_color_pipeline(const grk_image* image) {
    return (image->color_space != GRK_CLRSPC_UNKNOWN && image->color_space != GRK_CLRSPC_SRGB
            && image->color_space != GRK_CLRSPC_GRAY)
        || (image->meta && (image->meta->color.icc_profile_len
            || image->meta->color.palette || image->meta->color.channel_definition));
}

extern "C" {
void river_grok_initialize(uint32_t threads) {
    grk_initialize(nullptr, threads, nullptr);
}

void river_grok_release(RiverGrokHandle* handle) {
    if (!handle) return;
    grk_object_unref(handle->codec);
    delete handle;
}

RiverGrokHandle* river_grok_decode(const char* path, const uint8_t* bytes, size_t length,
    uint32_t x0, uint32_t y0, uint32_t x1, uint32_t y1, uint8_t reduction, uint16_t layers) {
    grk_object* codec = nullptr;
    try {
        grk_stream_params stream{};
        stream.is_read_stream = true;
        if (bytes) {
            stream.buf = const_cast<uint8_t*>(bytes);
            stream.buf_len = length;
            stream.stream_len = length;
        } else {
            if (!path) return nullptr;
            size_t i = 0;
            for (; path[i] && i + 1 < sizeof(stream.file); ++i) stream.file[i] = path[i];
            if (path[i]) return nullptr;
        }
        grk_decompress_parameters params{};
        params.core.reduce = reduction;
        params.core.layers_to_decompress = layers;
        params.dw_x0 = x0; params.dw_y0 = y0;
        params.dw_x1 = x1; params.dw_y1 = y1;
        codec = grk_decompress_init(&stream, &params);
        if (!codec) return nullptr;
        grk_header_info header{};
        if (!grk_decompress_read_header(codec, &header)) {
            grk_object_unref(codec); return nullptr;
        }
        // These layouts already require the CLI conversion pipeline. Decide
        // from header metadata before spending time decoding the same ROI twice.
        if (requires_color_pipeline(&header.header_image)) {
            grk_object_unref(codec); return nullptr;
        }
        if (!grk_decompress(codec, nullptr)) {
            grk_object_unref(codec); return nullptr;
        }
        auto* image = grk_decompress_get_image(codec);
        if (!image || !image->numcomps || image->numcomps > 16) {
            grk_object_unref(codec); return nullptr;
        }
        // This bridge exposes component samples, not the CLI's colour
        // conversion pipeline. Reject layouts that need that pipeline.
        if (requires_color_pipeline(image)) {
            grk_object_unref(codec); return nullptr;
        }
        return new RiverGrokHandle{codec, image};
    } catch (...) {
        if (codec) grk_object_unref(codec);
        return nullptr;
    }
}

uint16_t river_grok_components(const RiverGrokHandle* handle) {
    return handle ? handle->image->numcomps : 0;
}

bool river_grok_plane(const RiverGrokHandle* handle, uint16_t index, RiverGrokPlane* out) {
    if (!handle || !out || index >= handle->image->numcomps) return false;
    const auto& plane = handle->image->comps[index];
    if (!plane.data || plane.dx != 1 || plane.dy != 1 || plane.prec > 16) return false;
    *out = {plane.data, plane.w, plane.h, plane.stride, plane.x0, plane.y0,
        plane.prec, static_cast<uint8_t>(plane.sgnd),
        static_cast<uint8_t>(plane.data_type == GRK_INT_16 ? 2 : 4)};
    return true;
}
}
