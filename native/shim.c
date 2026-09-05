/* Vixen's C ABI adapters for inline QuickJS helpers and opaque stb types. */
#include <stdint.h>
#include <stddef.h>
#include <string.h>
#include <stdlib.h>
#include "quickjs-2024-01-13/quickjs.h"
#include "stb_truetype.h"

/* Non-inline wrappers over QuickJS static-inline helpers so Odin can link them.
 * NOTE: on 64-bit upstream QuickJS, JSValue is 16 bytes {union, tag}
 * (NaN boxing is 32-bit-only); values cross the FFI as the real struct. */
void spike_js_free(JSContext *ctx, JSValue v) {
    JS_FreeValue(ctx, v);
}

int spike_js_is_exception(JSValue v) {
    return JS_IsException(v);
}

const char *spike_js_to_cstring(JSContext *ctx, JSValue v, size_t *plen) {
    return JS_ToCStringLen2(ctx, plen, v, 0);
}

void spike_js_free_cstring(JSContext *ctx, const char *ptr) {
    JS_FreeCString(ctx, ptr);
}

/* Value constructors over static-inline helpers. */
JSValue spike_js_dup(JSContext *ctx, JSValue v) { return JS_DupValue(ctx, v); }
JSValue spike_js_new_bool(JSContext *ctx, int b) { return JS_NewBool(ctx, b); }
JSValue spike_js_new_int32(JSContext *ctx, int32_t i) { return JS_NewInt32(ctx, i); }
JSValue spike_js_new_int64(JSContext *ctx, int64_t i) { return JS_NewInt64(ctx, i); }
JSValue spike_js_new_float64(JSContext *ctx, double d) { return JS_NewFloat64(ctx, d); }
JSValue spike_js_null(void) { return JS_NULL; }
JSValue spike_js_undefined(void) { return JS_UNDEFINED; }
int spike_js_is_null(JSValue v) { return JS_IsNull(v); }
int spike_js_is_undefined(JSValue v) { return JS_IsUndefined(v); }
JSValue spike_js_throw_type_error(JSContext *ctx, const char *msg) {
    return JS_ThrowTypeError(ctx, "%s", msg);
}

/* Opaque stb_truetype fontinfo without exposing its layout to Odin. */
void *spike_stbtt_alloc(void) {
    void *p = calloc(1, sizeof(stbtt_fontinfo));
    return p;
}

void spike_stbtt_free(void *p) {
    free(p);
}

void spike_free(void *p) {
    free(p);
}
