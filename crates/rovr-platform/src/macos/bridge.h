#pragma once

#include <stdint.h>

#define ROVR_APP_MAX 256
#define ROVR_TITLE_MAX 512
#define ROVR_BUNDLE_MAX 256

#define ROVR_CAP_OBSERVE_WINDOWS (1ULL << 0)
#define ROVR_CAP_SET_WINDOW_FRAME (1ULL << 1)
#define ROVR_CAP_FOCUS_WINDOW (1ULL << 2)
#define ROVR_CAP_OBSERVE_SPACES (1ULL << 3)
#define ROVR_CAP_MOVE_WINDOW_TO_SPACE (1ULL << 4)
#define ROVR_CAP_FOCUS_SPACE (1ULL << 5)

typedef struct rovr_bridge_window {
    uint32_t id;
    int32_t pid;
    uint32_t display_id;
    uint64_t space_id;
    uint8_t focused;
    uint8_t minimized;
    uint8_t fullscreen;
    uint8_t managed;
    double x;
    double y;
    double width;
    double height;
    char app[ROVR_APP_MAX];
    char title[ROVR_TITLE_MAX];
    char bundle_id[ROVR_BUNDLE_MAX];
} rovr_bridge_window;

typedef struct rovr_bridge_display {
    uint32_t id;
    uint8_t focused;
    uint8_t is_main;
    double x;
    double y;
    double width;
    double height;
} rovr_bridge_display;

typedef void (*rovr_window_callback)(const rovr_bridge_window *window, void *context);
typedef struct rovr_bridge_ax_window {
    uint32_t id;
    uint8_t focused;
    uint8_t minimized;
    uint8_t fullscreen;
    uint8_t managed;
} rovr_bridge_ax_window;
typedef void (*rovr_ax_window_callback)(const rovr_bridge_ax_window *window, void *context);
typedef void (*rovr_display_callback)(const rovr_bridge_display *display, void *context);
typedef struct rovr_bridge_space {
    uint64_t id;
    uint32_t display_id;
    int32_t type;
    uint8_t focused;
    uint32_t position;
} rovr_bridge_space;

typedef void (*rovr_space_callback)(const rovr_bridge_space *space, void *context);

int rovr_bridge_init(void);
uint64_t rovr_bridge_capabilities(void);
int rovr_bridge_enumerate_window_candidates(rovr_window_callback callback, void *context);
int rovr_bridge_refine_windows_for_pid(int32_t pid, rovr_ax_window_callback callback, void *context);
int rovr_bridge_enumerate_displays(rovr_display_callback callback, void *context);
int rovr_bridge_set_window_frame(uint32_t window_id, double x, double y, double width, double height);
int rovr_bridge_focus_window(uint32_t window_id);
int rovr_bridge_needs_refresh(void);
int rovr_bridge_enumerate_spaces(rovr_space_callback callback, void *context);
int rovr_bridge_move_window_to_space(uint32_t window_id, uint64_t space_id);
int rovr_bridge_focus_space(uint64_t space_id);
int rovr_bridge_focus_space_step(uint64_t target_space_id, int32_t delta);
uint64_t rovr_bridge_current_space_for_space(uint64_t space_id);
uint32_t rovr_bridge_display_for_space(uint64_t space_id);
int rovr_bridge_set_window_minimized(uint32_t window_id, int minimized);
int32_t rovr_bridge_window_pid(uint32_t window_id);
uint64_t rovr_bridge_window_space_id(uint32_t window_id);
int32_t rovr_bridge_dock_pid(void);

// AX event trampoline registration (rovr-platform calls this once at init;
// the callback is invoked on the main thread for created/focused events).
typedef void (*rovr_ax_event_trampoline_fn)(int event_kind, uint32_t window_id);
void rovr_bridge_install_event_handlers(rovr_ax_event_trampoline_fn callback);
