#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <wayland-client.h>

#include "wlr-virtual-pointer-unstable-v1-client-protocol.h"

static struct zwlr_virtual_pointer_manager_v1 *manager;
static struct wl_seat *seat;
static struct wl_output *output;

static void registry_global(
    void *data,
    struct wl_registry *registry,
    uint32_t name,
    const char *interface,
    uint32_t version) {
  (void)data;
  if (strcmp(interface, zwlr_virtual_pointer_manager_v1_interface.name) == 0) {
    manager = wl_registry_bind(
        registry,
        name,
        &zwlr_virtual_pointer_manager_v1_interface,
        version < 2 ? version : 2);
  } else if (strcmp(interface, wl_seat_interface.name) == 0) {
    seat = wl_registry_bind(
        registry,
        name,
        &wl_seat_interface,
        version < 7 ? version : 7);
  } else if (strcmp(interface, wl_output_interface.name) == 0 && output == NULL) {
    output = wl_registry_bind(
        registry,
        name,
        &wl_output_interface,
        version < 4 ? version : 4);
  }
}

static void registry_global_remove(
    void *data,
    struct wl_registry *registry,
    uint32_t name) {
  (void)data;
  (void)registry;
  (void)name;
}

static const struct wl_registry_listener registry_listener = {
    .global = registry_global,
    .global_remove = registry_global_remove,
};

static uint32_t event_time(void) {
  struct timespec now;
  if (clock_gettime(CLOCK_MONOTONIC, &now) != 0) {
    perror("clock_gettime");
    exit(1);
  }
  return (uint32_t)(now.tv_sec * 1000u + now.tv_nsec / 1000000u);
}

static void sleep_millis(long millis) {
  struct timespec delay = {
      .tv_sec = millis / 1000,
      .tv_nsec = (millis % 1000) * 1000000,
  };
  while (nanosleep(&delay, &delay) != 0 && errno == EINTR) {
  }
}

static uint32_t parse_u32(const char *value, const char *name) {
  char *end = NULL;
  errno = 0;
  unsigned long parsed = strtoul(value, &end, 10);
  if (errno != 0 || end == value || *end != '\0' || parsed > UINT32_MAX) {
    fprintf(stderr, "invalid %s: %s\n", name, value);
    exit(2);
  }
  return (uint32_t)parsed;
}

static double parse_double(const char *value) {
  char *end = NULL;
  errno = 0;
  double parsed = strtod(value, &end);
  if (errno != 0 || end == value || *end != '\0') {
    fprintf(stderr, "invalid wheel delta: %s\n", value);
    exit(2);
  }
  return parsed;
}

int main(int argc, char **argv) {
  int click = argc == 4 && strcmp(argv[1], "click") == 0;
  int wheel = argc == 5 && strcmp(argv[1], "wheel") == 0;
  if (!click && !wheel) {
    fprintf(stderr, "usage: %s click X Y | %s wheel X Y DELTA\n", argv[0], argv[0]);
    return 2;
  }
  uint32_t x = parse_u32(argv[2], "x");
  uint32_t y = parse_u32(argv[3], "y");
  double delta = wheel ? parse_double(argv[4]) : 0;

  struct wl_display *display = wl_display_connect(NULL);
  if (display == NULL) {
    fprintf(stderr, "failed to connect to Wayland display\n");
    return 1;
  }
  struct wl_registry *registry = wl_display_get_registry(display);
  wl_registry_add_listener(registry, &registry_listener, NULL);
  if (wl_display_roundtrip(display) < 0 || manager == NULL || seat == NULL || output == NULL) {
    fprintf(stderr, "compositor does not expose virtual pointer, seat, and output globals\n");
    return 1;
  }

  struct zwlr_virtual_pointer_v1 *pointer =
      zwlr_virtual_pointer_manager_v1_create_virtual_pointer_with_output(
          manager, seat, output);
  uint32_t time = event_time();
  if (click) {
    zwlr_virtual_pointer_v1_motion_absolute(pointer, time, x, y, 1280, 720);
    zwlr_virtual_pointer_v1_frame(pointer);
    wl_display_roundtrip(display);
    sleep_millis(20);
    zwlr_virtual_pointer_v1_button(
        pointer, time, 0x110, WL_POINTER_BUTTON_STATE_PRESSED);
    zwlr_virtual_pointer_v1_frame(pointer);
    zwlr_virtual_pointer_v1_button(
        pointer, event_time(), 0x110, WL_POINTER_BUTTON_STATE_RELEASED);
    zwlr_virtual_pointer_v1_frame(pointer);
  } else {
    zwlr_virtual_pointer_v1_motion_absolute(pointer, time, x, y, 1280, 720);
    zwlr_virtual_pointer_v1_frame(pointer);
    wl_display_roundtrip(display);
    sleep_millis(20);
    zwlr_virtual_pointer_v1_axis_source(pointer, WL_POINTER_AXIS_SOURCE_WHEEL);
    zwlr_virtual_pointer_v1_axis(
        pointer,
        event_time(),
        WL_POINTER_AXIS_VERTICAL_SCROLL,
        wl_fixed_from_double(delta));
    zwlr_virtual_pointer_v1_frame(pointer);
  }
  if (wl_display_roundtrip(display) < 0) {
    fprintf(stderr, "virtual pointer request failed\n");
    return 1;
  }
  sleep_millis(100);

  zwlr_virtual_pointer_v1_destroy(pointer);
  zwlr_virtual_pointer_manager_v1_destroy(manager);
  wl_seat_destroy(seat);
  wl_output_destroy(output);
  wl_registry_destroy(registry);
  wl_display_disconnect(display);
  return 0;
}
