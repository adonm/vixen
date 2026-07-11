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
static uint32_t (*const buffer_release_fn)(uint64_t) = &vixen_buffer_release;

int main(void) {
    return abi_version_fn == NULL || open_fn == NULL || destroy_fn == NULL ||
           command_fn == NULL || poll_event_fn == NULL ||
           wait_event_fn == NULL || buffer_release_fn == NULL;
}
