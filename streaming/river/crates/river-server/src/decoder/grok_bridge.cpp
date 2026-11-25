// Compile against the same Grok headers as the packaged shared library.
// Rust sees only this small ABI, never Grok's version-dependent structures.
#include <grok.h>
#include <cstddef>
#include <chrono>
#include <cstdio>
#include <cstdint>
#include <new>
#include <memory>
#include <algorithm>

using ReadAt = int32_t (*)(void*, uint8_t*, uint64_t, uint64_t);
struct OriginalInput {
    void* context;
    ReadAt read_at;
    uint64_t length, offset;
    static size_t read(uint8_t* out, size_t n, void* opaque) {
        auto& s = *static_cast<OriginalInput*>(opaque);
        n = static_cast<size_t>(std::min<uint64_t>(n, s.length - s.offset));
        if (!n || !s.read_at(s.context, out, s.offset, n)) return 0;
        s.offset += n;
        return n;
    }
    static bool seek(uint64_t position, void* opaque) {
        auto& s = *static_cast<OriginalInput*>(opaque);
        if (position > s.length) return false;
        s.offset = position;
        return true;
    }
};
struct RiverGrokHandle { grk_object* codec; grk_image* image; double decompress_ms; std::unique_ptr<OriginalInput> original; };
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

// Grok installs process-wide handlers. Only associate callbacks on the thread
// currently executing this decode with its failure; workers can serve other tiles.
static thread_local bool collecting = false;
static thread_local char messages[4096];
static thread_local size_t used = 0;
static thread_local char failure_text[4352];
static void capture(const char* level, const char* msg) {
    if (!collecting || !msg || used >= sizeof(messages)-1) return;
    int count = std::snprintf(messages+used, sizeof(messages)-used, " [%s] %s", level, msg);
    if (count > 0) used += std::min(static_cast<size_t>(count), sizeof(messages)-1-used);
}
static void error_message(const char* msg, void*) { capture("error", msg); }
static void warn_message(const char* msg, void*) { capture("warning", msg); }
static void info_message(const char* msg, void*) { capture("info", msg); }
static void debug_message(const char* msg, void*) { capture("debug", msg); }
static void trace_message(const char* msg, void*) { capture("trace", msg); }
static thread_local const char* last_failure = "";
static RiverGrokHandle* failed(const char* reason) {
    std::snprintf(failure_text,sizeof(failure_text),"%s%s%s",reason,collecting ? messages : "",
                  collecting && used==sizeof(messages)-1 ? " [driver text clipped]" : "");
    last_failure = failure_text;
    return nullptr;
}

extern "C" {
const char* river_grok_last_failure() { return last_failure; }
#ifdef GRK_RIVER_CODEWORDS
void river_grok_block_session_v2(const void* session, grk_river_block_fn access) {
    grk_river_block_session_v2(session, access);
}
#endif
#ifdef GRK_RIVER_BLOCK_CACHE
void river_grok_block_session(const void* session, grk_river_block_fn access) {
    grk_river_block_session(session, access);
}
#endif

void river_grok_enable_diagnostics(bool debug) {
    grk_msg_handlers h{};
    h.error_callback = error_message;
    h.warn_callback = warn_message;
    if (debug) {
        h.info_callback = info_message;
        h.debug_callback = debug_message;
        h.trace_callback = trace_message;
    }
    grk_set_msg_handlers(h);
}

void river_grok_initialize(uint32_t threads) {
    grk_initialize(nullptr, threads, nullptr);
}

void river_grok_release(RiverGrokHandle* handle) {
    if (!handle) return;
    grk_object_unref(handle->codec);
    delete handle;
}

static RiverGrokHandle* decode_impl(std::unique_ptr<OriginalInput> original, const char* path, const uint8_t* bytes, size_t length,
    uint32_t x0, uint32_t y0, uint32_t x1, uint32_t y1, uint8_t reduction, uint16_t layers) {
    grk_object* codec = nullptr;
    last_failure = "";
    used = 0;
    messages[0] = 0;
    collecting = true;
    struct EndCapture { ~EndCapture() { collecting = false; } } end_capture;
    try {
        grk_stream_params stream{};
        stream.is_read_stream = true;
        if (original) {
            stream.stream_len = original->length;
            stream.double_buffer_len = stream.initial_double_buffer_len = 65536;
            stream.user_data = original.get();
            stream.read_fn = OriginalInput::read;
            stream.seek_fn = OriginalInput::seek;
        } else if (bytes) {
            stream.buf = const_cast<uint8_t*>(bytes);
            stream.buf_len = length;
            stream.stream_len = length;
        } else {
            if (!path) return failed("missing input path");
            size_t i = 0;
            for (; path[i] && i + 1 < sizeof(stream.file); ++i) stream.file[i] = path[i];
            if (path[i]) return failed("input path exceeds Grok buffer");
        }
        grk_decompress_parameters params{};
        params.core.reduce = reduction;
        params.core.layers_to_decompress = layers;
        params.dw_x0 = x0; params.dw_y0 = y0;
        params.dw_x1 = x1; params.dw_y1 = y1;
#ifdef GRK_RIVER_BLOCK_CACHE
        struct ResetOriginal {
            ~ResetOriginal() { grk_river_original_session(nullptr, nullptr, 0); }
        } reset;
        if (original) grk_river_original_session(original->context, original->read_at, original->length);
#endif
        codec = grk_decompress_init(&stream, &params);
        if (!codec) return failed("grk_decompress_init");
        grk_header_info header{};
        if (!grk_decompress_read_header(codec, &header)) {
            grk_object_unref(codec); return failed("grk_decompress_read_header");
        }
        // These layouts already require the CLI conversion pipeline. Decide
        // from header metadata before spending time decoding the same ROI twice.
        if (requires_color_pipeline(&header.header_image)) {
            grk_object_unref(codec); return failed("header requires CLI color pipeline");
        }
        auto started = std::chrono::steady_clock::now();
        if (!grk_decompress(codec, nullptr)) {
            grk_object_unref(codec); return failed("grk_decompress");
        }
        double decompress_ms = std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now() - started).count();
        auto* image = grk_decompress_get_image(codec);
        if (!image || !image->numcomps || image->numcomps > 16) {
            grk_object_unref(codec); return failed("grk_decompress_get_image returned missing or unsupported components");
        }
        // This bridge exposes component samples, not the CLI's colour
        // conversion pipeline. Reject layouts that need that pipeline.
        if (requires_color_pipeline(image)) {
            grk_object_unref(codec); return failed("decoded image requires CLI color pipeline");
        }
        return new RiverGrokHandle{codec, image, decompress_ms, std::move(original)};
    } catch (...) {
        if (codec) grk_object_unref(codec);
        return failed("exception in Grok bridge");
    }
}

RiverGrokHandle* river_grok_decode(const char* path, const uint8_t* bytes, size_t length,
    uint32_t x0, uint32_t y0, uint32_t x1, uint32_t y1, uint8_t reduction, uint16_t layers) {
    return decode_impl(nullptr, path, bytes, length, x0, y0, x1, y1, reduction, layers);
}
#ifdef GRK_RIVER_BLOCK_CACHE
RiverGrokHandle* river_grok_decode_original(void* context, ReadAt read_at, uint64_t length,
    uint32_t x0, uint32_t y0, uint32_t x1, uint32_t y1, uint8_t reduction, uint16_t layers) {
    auto cursor = std::unique_ptr<OriginalInput>(new(std::nothrow) OriginalInput{context,read_at,length,0});
    if (!cursor) return failed("cannot allocate original-source reader");
    return decode_impl(std::move(cursor), nullptr, nullptr, 0, x0, y0, x1, y1, reduction, layers);
}
#endif

double river_grok_decompress_ms(const RiverGrokHandle* handle) { return handle->decompress_ms; }

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
