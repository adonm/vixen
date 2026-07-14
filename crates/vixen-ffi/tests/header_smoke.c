#include "../include/vixen.h"

#include <stddef.h>

_Static_assert(sizeof(VixenHandle) == sizeof(uint64_t), "handle width");
_Static_assert(offsetof(VixenBuffer, token) == 0, "buffer token offset");
_Static_assert(offsetof(VixenBuffer, ptr) == sizeof(uint64_t),
               "buffer pointer offset");
_Static_assert(offsetof(VixenBuffer, len) ==
                   sizeof(uint64_t) + sizeof(const uint8_t *),
               "buffer length offset");
_Static_assert(sizeof(VixenBuffer) >= sizeof(uint64_t) +
                                          sizeof(const uint8_t *) +
                                          sizeof(size_t),
                "buffer size");
_Static_assert(offsetof(VixenFrame, token) == 0, "frame token offset");
_Static_assert(offsetof(VixenFrame, ptr) == sizeof(uint64_t),
               "frame pointer offset");
_Static_assert(offsetof(VixenFrame, len) ==
                   sizeof(uint64_t) + sizeof(const uint8_t *),
               "frame length offset");
_Static_assert(offsetof(VixenFrame, width) ==
                   sizeof(uint64_t) + sizeof(const uint8_t *) + sizeof(size_t),
               "frame width offset");
_Static_assert(offsetof(VixenFrame, height) ==
                   offsetof(VixenFrame, width) + sizeof(uint32_t),
               "frame height offset");
_Static_assert(offsetof(VixenFrame, row_stride) ==
                   offsetof(VixenFrame, height) + sizeof(uint32_t),
               "frame row stride offset");
_Static_assert(offsetof(VixenFrame, frame_id) ==
                   offsetof(VixenFrame, row_stride) + sizeof(size_t),
               "frame id offset");
_Static_assert(offsetof(VixenFrame, context_id) ==
                   offsetof(VixenFrame, frame_id) + sizeof(uint64_t),
               "frame context id offset");
_Static_assert(offsetof(VixenFrame, document_id) ==
                   offsetof(VixenFrame, context_id) + sizeof(uint64_t),
               "frame document id offset");
_Static_assert(sizeof(VixenFrame) >=
                   offsetof(VixenFrame, document_id) + sizeof(uint64_t),
               "frame size");
_Static_assert(VIXEN_MAX_FRAME_DIMENSION == 4096u, "frame dimension limit");
_Static_assert(VIXEN_MAX_FRAME_BYTES == 67108864u, "frame byte limit");
_Static_assert(VIXEN_MAX_OUTSTANDING_FRAMES == 3u, "frame retention limit");

static uint32_t (*const abi_version_fn)(void) = &vixen_abi_version;
static uint32_t (*const open_fn)(const uint8_t *, size_t, VixenHandle *,
                                 VixenBuffer *) = &vixen_open;
static uint32_t (*const destroy_fn)(VixenHandle) = &vixen_destroy;
static uint32_t (*const command_fn)(VixenHandle, const uint8_t *, size_t,
                                    VixenBuffer *) = &vixen_command;
static uint32_t (*const poll_event_fn)(VixenHandle, VixenBuffer *) =
    &vixen_poll_event;
static uint32_t (*const wait_event_fn)(VixenHandle, uint64_t, VixenBuffer *) =
    &vixen_wait_event;
static uint32_t (*const renderer_poll_fn)(VixenHandle, uint64_t, VixenBuffer *) =
    &vixen_renderer_poll;
static uint32_t (*const renderer_respond_fn)(VixenHandle, const uint8_t *,
                                             size_t, VixenBuffer *) =
    &vixen_renderer_respond;
static uint32_t (*const renderer_shutdown_fn)(VixenHandle, VixenBuffer *) =
    &vixen_renderer_shutdown;
static uint32_t (*const renderer_submit_fn)(VixenHandle, const uint8_t *,
                                            size_t, VixenBuffer *) =
    &vixen_renderer_submit;
static uint32_t (*const buffer_release_fn)(uint64_t) = &vixen_buffer_release;
static uint32_t (*const capture_frame_fn)(VixenHandle, uint64_t, uint64_t,
                                          uint32_t, uint32_t, VixenFrame *,
                                          VixenBuffer *) = &vixen_capture_frame;
static uint32_t (*const frame_release_fn)(uint64_t) = &vixen_frame_release;

int main(void) {
    return abi_version_fn == NULL || open_fn == NULL || destroy_fn == NULL ||
           command_fn == NULL || poll_event_fn == NULL ||
           wait_event_fn == NULL || renderer_poll_fn == NULL ||
           renderer_respond_fn == NULL || buffer_release_fn == NULL ||
           renderer_shutdown_fn == NULL ||
           renderer_submit_fn == NULL ||
           capture_frame_fn == NULL || frame_release_fn == NULL;
}
