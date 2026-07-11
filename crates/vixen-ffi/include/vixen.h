#ifndef VIXEN_H
#define VIXEN_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ABI and input limits. Lengths are bytes, not characters. */
#define VIXEN_ABI_VERSION 1u
#define VIXEN_MAX_PROFILE_PATH_BYTES 4096u
#define VIXEN_MAX_MESSAGE_BYTES 65536u
#define VIXEN_MAX_OUTPUT_BYTES 1048576u
#define VIXEN_MAX_OUTSTANDING_BUFFERS 64u
#define VIXEN_MAX_WAIT_MILLISECONDS 60000u

/* Stable return statuses. Only VIXEN_STATUS_OK means an operation succeeded. */
#define VIXEN_STATUS_OK 0u
#define VIXEN_STATUS_NO_EVENT 1u
#define VIXEN_STATUS_INVALID_ARGUMENT 2u
#define VIXEN_STATUS_INVALID_UTF8 3u
#define VIXEN_STATUS_INPUT_TOO_LARGE 4u
#define VIXEN_STATUS_INVALID_COMMAND 5u
#define VIXEN_STATUS_UNKNOWN_HANDLE 6u
#define VIXEN_STATUS_BROWSER_ERROR 7u
#define VIXEN_STATUS_UNKNOWN_BUFFER 8u
#define VIXEN_STATUS_PANIC 9u
#define VIXEN_STATUS_INTERNAL_ERROR 10u
#define VIXEN_STATUS_OUTPUT_TOO_LARGE 11u
#define VIXEN_STATUS_BUFFER_LIMIT 12u

/*
 * Opaque process-local token. Zero is never valid. Tokens are monotonically
 * allocated and not reused. They are values, not pointers, and callers must not
 * inspect or modify them.
 */
typedef uint64_t VixenHandle;

/*
 * The sole output allocation contract. token == 0, ptr == NULL, len == 0 means
 * no output. Otherwise ptr addresses len immutable UTF-8 JSON bytes owned by
 * Rust until vixen_buffer_release(token) succeeds. Do not write, free, resize,
 * retain ptr after release, or reconstruct a Rust/C allocation layout. Release
 * by token only. Unknown, zero, and already released tokens fail safely.
 * Every allocation is capped at VIXEN_MAX_OUTPUT_BYTES before registration.
 * At most VIXEN_MAX_OUTSTANDING_BUFFERS allocations may be retained process-wide.
 */
typedef struct VixenBuffer {
    uint64_t token;
    const uint8_t *ptr;
    size_t len;
} VixenBuffer;

/*
 * Threading and ownership:
 *
 * - vixen_open creates exactly one browser-scoped controller, which owns one
 *   BrowserCore handle and is the sole consumer of its ordered event stream.
 * - Functions are callable from arbitrary native threads. Calls on one handle
 *   serialize; different handles may progress independently. A blocking wait
 *   holds that handle's serialization lock for its duration.
 * - Do not concurrently destroy a handle with another call on that handle.
 *   Destroy is explicit; zero, unknown, and repeated destruction fail safely.
 * - There are no callbacks. Commands and events are copied across this ABI.
 * - Input pointers must address their declared readable byte ranges for the
 *   duration of the call. Output pointers must be writable for the call.
 *   Null input/output pointers fail when detectable; arbitrary invalid non-null
 *   pointers cannot be validated by C or Rust and violate this contract.
 * - Every exported function contains Rust panic containment. PANIC and
 *   INTERNAL_ERROR mean no browser outcome should be assumed.
 */

/*
 * JSON wire v1:
 *
 * All command objects require exactly the listed fields. Unknown fields,
 * unknown tags, non-integer ids, zero context ids, and versions other than 1
 * fail closed with INVALID_COMMAND. The entire command is bounded by
 * VIXEN_MAX_MESSAGE_BYTES before copying or JSON parsing.
 *
 *   {"v":1,"type":"load_profile_session"}
 *   {"v":1,"type":"save_current_profile_session"}
 *   {"v":1,"type":"browser_snapshot"}
 *   {"v":1,"type":"create_context"}
 *   {"v":1,"type":"close_context","context_id":U64}
 *   {"v":1,"type":"activate_context","context_id":U64}
 *   {"v":1,"type":"navigate","context_id":U64,"url":STRING}
 *   {"v":1,"type":"reload","context_id":U64}
 *   {"v":1,"type":"stop","context_id":U64}
 *   {"v":1,"type":"traverse_history","context_id":U64,"delta":I32}
 *   {"v":1,"type":"context_state","context_id":U64}
 *
 * Successful commands return:
 *   {"v":1,"type":"response","response":{"type":TAG,...}}
 * Response TAG is accepted, profile_session, browser_snapshot,
 * context_created, navigation_accepted, or context_state. Profile sessions
 * carry tabs and active_index. Snapshots carry active_context_id and contexts.
 * Context states carry context_id, main_frame_id, document_id,
 * runtime_context_id, active_navigation_id, url, title, history_length,
 * history_index, can_go_back, can_go_forward, is_loading, and load_progress.
 * Optional values are JSON null.
 *
 * Delivered events return:
 *   {"v":1,"type":"event","sequence":U64,"event":{"type":TAG,...}}
 * sequence starts at 1 and increases per successfully delivered event on one
 * handle. Event TAG is browsing_context_created, browsing_context_closed,
 * active_browsing_context_changed, navigation_requested, navigation_started,
 * navigation_redirected, navigation_phase_changed, runtime_context_destroyed,
 * document_discarded, navigation_committed, runtime_context_created,
 * runtime_effects, dom_content_loaded, document_load_completed,
 * navigation_cancelled, navigation_failed, browsing_context_state_changed,
 * download, or diagnostic. Each projection retains all typed ids/generations
 * from BrowserEvent. Navigation kind, phase, cancellation reason, runtime
 * effect, download, diagnostic scope/category, and BrowserError values use
 * lower_snake_case tags and explicit fields, never Rust Debug formatting.
 *
 * Failures from functions with out_json return:
 *   {"v":1,"type":"error","error":{"code":STRING,"message":STRING}}
 * BrowserCore errors retain their stable browser.* code. A null out_json cannot
 * receive an error buffer. On NO_EVENT, out_json is the all-zero descriptor.
 * Oversized output fails with OUTPUT_TOO_LARGE and ffi.output-too-large.
 * If callers retain too many allocations, BUFFER_LIMIT may have no JSON output;
 * release an earlier token before retrying.
 */

/* Returns VIXEN_ABI_VERSION. Zero is reserved for a contained panic. */
uint32_t vixen_abi_version(void);

/*
 * Opens one UTF-8 profile path. On success writes a nonzero handle and an
 * "opened" JSON buffer. out_handle is reset to zero before opening.
 */
uint32_t vixen_open(const uint8_t *profile_path,
                    size_t profile_path_len,
                    VixenHandle *out_handle,
                    VixenBuffer *out_json);

/* Destroys one handle. This function has no output buffer. */
uint32_t vixen_destroy(VixenHandle handle);

/* Dispatches one bounded UTF-8 JSON command and returns response/error JSON. */
uint32_t vixen_command(VixenHandle handle,
                       const uint8_t *message,
                       size_t message_len,
                       VixenBuffer *out_json);

/* Nonblocking event consume. NO_EVENT writes the all-zero descriptor. */
uint32_t vixen_poll_event(VixenHandle handle, VixenBuffer *out_json);

/* Timeout-bounded event consume. timeout_milliseconds may be zero. */
uint32_t vixen_wait_event(VixenHandle handle,
                          uint64_t timeout_milliseconds,
                          VixenBuffer *out_json);

/* Releases an output allocation by token. Safe failure is UNKNOWN_BUFFER. */
uint32_t vixen_buffer_release(uint64_t token);

#ifdef __cplusplus
}
#endif

#endif /* VIXEN_H */
