#pragma once

#include <stdint.h>

#define ROVR_APP_MAX 256
#define ROVR_TITLE_MAX 512
#define ROVR_BUNDLE_MAX 256

#define ROVR_CAP_OBSERVE_WINDOWS (1ULL << 0)
#define ROVR_CAP_SET_WINDOW_FRAME (1ULL << 1)
#define ROVR_CAP_FOCUS_WINDOW (1ULL << 2)

typedef struct rovr_bridge_window {
    uint32_t id;
    int32_t pid;
    uint32_t display_id;
    uint8_t focused;
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
    double x;
    double y;
    double width;
    double height;
} rovr_bridge_display;

typedef void (*rovr_window_callback)(const rovr_bridge_window *window, void *context);
typedef void (*rovr_display_callback)(const rovr_bridge_display *display, void *context);

int rovr_bridge_init(void);
uint64_t rovr_bridge_capabilities(void);
int rovr_bridge_enumerate_windows(rovr_window_callback callback, void *context);
int rovr_bridge_enumerate_displays(rovr_display_callback callback, void *context);
int rovr_bridge_set_window_frame(uint32_t window_id, double x, double y, double width, double height);
int rovr_bridge_focus_window(uint32_t window_id);
int rovr_bridge_needs_refresh(void);
