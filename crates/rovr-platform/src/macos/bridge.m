#import "bridge.h"

#import <AppKit/AppKit.h>
#import <ApplicationServices/ApplicationServices.h>
#import <dlfcn.h>
#import <mach-o/dyld.h>
#import <mach-o/loader.h>
#import <mach-o/nlist.h>
#import <objc/message.h>
#import <objc/runtime.h>
#import <stdatomic.h>
#import <stdlib.h>
#import <string.h>
#import <signal.h>
#import <errno.h>
#import <pthread.h>

// _AXUIElementGetWindow is private. Resolve it dynamically so the bridge still
// links if the symbol disappears. The private capability layer will eventually
// isolate this further behind explicit capability probing.
typedef AXError (*rovr_ax_get_window_fn)(AXUIElementRef element, CGWindowID *window_id);
static rovr_ax_get_window_fn g_ax_get_window = NULL;

// Accessibility messaging is IPC to another process and may otherwise wait
// indefinitely when that process stops servicing its AX port.
static const float ROVR_AX_MESSAGING_TIMEOUT_SECONDS = 0.5f;

static void rovr_ax_apply_timeout(AXUIElementRef element) {
    if (element) AXUIElementSetMessagingTimeout(element, ROVR_AX_MESSAGING_TIMEOUT_SECONDS);
}

// Private SkyLight symbols. SkyLight.framework is not a linked public
// framework, so every entry point is resolved at runtime with dlsym against
// the default search order. A NULL pointer means the capability is absent on
// this OS build; the bridge reports that honestly through capabilities().
typedef int (*rovr_sls_main_connection_fn)(void);
typedef CFArrayRef (*rovr_sls_copy_managed_display_spaces_fn)(int cid);
typedef CFStringRef (*rovr_sls_copy_managed_display_for_space_fn)(int cid, uint64_t sid);
typedef uint64_t (*rovr_sls_managed_display_get_current_space_fn)(int cid, CFStringRef uuid);
typedef int (*rovr_sls_space_get_type_fn)(int cid, uint64_t sid);
typedef void (*rovr_sls_move_windows_to_managed_space_fn)(int cid, CFArrayRef window_list, uint64_t sid);
typedef CGError (*rovr_sls_space_set_compat_id_fn)(int cid, uint64_t sid, int workspace);
typedef CGError (*rovr_sls_set_window_list_workspace_fn)(int cid, uint32_t *window_list, int window_count, int workspace);
typedef CFArrayRef (*rovr_sls_copy_spaces_for_windows_fn)(int cid, int selector, CFArrayRef window_list);
typedef int64_t (*rovr_sls_perform_async_bridged_op_fn)(void *operation);
typedef CGError (*rovr_sls_set_active_menu_bar_display_identifier_fn)(int cid, CFStringRef uuid, CFStringRef uuid2);
typedef CFStringRef (*rovr_sls_copy_active_menu_bar_display_identifier_fn)(int cid);
typedef bool (*rovr_sls_managed_display_is_animating_fn)(int cid, CFStringRef uuid);
typedef CGError (*rovr_sls_register_notify_fn)(int cid, void *handler, uint32_t event, void *context);

static rovr_sls_main_connection_fn g_sls_main_connection = NULL;
static rovr_sls_copy_managed_display_spaces_fn g_sls_copy_managed_display_spaces = NULL;
static rovr_sls_copy_managed_display_for_space_fn g_sls_copy_managed_display_for_space = NULL;
static rovr_sls_managed_display_get_current_space_fn g_sls_managed_display_get_current_space = NULL;
static rovr_sls_space_get_type_fn g_sls_space_get_type = NULL;
static rovr_sls_move_windows_to_managed_space_fn g_sls_move_windows_to_managed_space = NULL;
static rovr_sls_space_set_compat_id_fn g_sls_space_set_compat_id = NULL;
static rovr_sls_set_window_list_workspace_fn g_sls_set_window_list_workspace = NULL;
static rovr_sls_copy_spaces_for_windows_fn g_sls_copy_spaces_for_windows = NULL;
static rovr_sls_perform_async_bridged_op_fn g_sls_perform_async_bridged_op = NULL;
// Activates a display (yabai: display_manager_set_active_display_id). Needed
// so cross-display Space switches actually bring the other display forward.
static rovr_sls_set_active_menu_bar_display_identifier_fn g_sls_set_active_menu_bar_display_identifier = NULL;
static rovr_sls_copy_active_menu_bar_display_identifier_fn g_sls_copy_active_menu_bar_display_identifier = NULL;
static rovr_sls_managed_display_is_animating_fn g_sls_managed_display_is_animating = NULL;
static rovr_sls_register_notify_fn g_sls_register_notify = NULL;
typedef CFTypeRef (*rovr_sls_window_query_fn)(int cid, CFArrayRef windows, int count);
typedef CFTypeRef (*rovr_sls_window_query_copy_fn)(CFTypeRef query);
typedef int (*rovr_sls_window_iter_count_fn)(CFTypeRef iter);
typedef bool (*rovr_sls_window_iter_advance_fn)(CFTypeRef iter);
typedef uint32_t (*rovr_sls_window_iter_parent_fn)(CFTypeRef iter);
typedef int (*rovr_sls_window_iter_level_fn)(CFTypeRef iter);
typedef CGError (*rovr_sls_get_menu_autohide_fn)(int cid, int *enabled);
typedef CGError (*rovr_sls_get_revealed_menu_bounds_fn)(CGRect *rect, int cid, uint64_t sid);
typedef CGError (*rovr_sls_get_display_menubar_height_fn)(uint32_t did, uint32_t *height);
typedef CGError (*rovr_sls_get_dock_rect_fn)(int cid, CGRect *rect, int *reason);
typedef Boolean (*rovr_core_dock_autohide_fn)(void);
typedef void (*rovr_core_dock_orient_fn)(int *orientation, int *pinning);
typedef CGError (*rovr_sls_request_notifications_fn)(int cid, uint32_t *window_list, int window_count);
static rovr_sls_window_query_fn g_sls_window_query = NULL;
static rovr_sls_window_query_copy_fn g_sls_window_query_copy = NULL;
static rovr_sls_window_iter_count_fn g_sls_window_iter_count = NULL;
static rovr_sls_window_iter_advance_fn g_sls_window_iter_advance = NULL;
static rovr_sls_window_iter_parent_fn g_sls_window_iter_parent = NULL;
static rovr_sls_window_iter_level_fn g_sls_window_iter_level = NULL;
static rovr_sls_get_menu_autohide_fn g_sls_get_menu_autohide = NULL;
static rovr_sls_get_revealed_menu_bounds_fn g_sls_get_revealed_menu = NULL;
static rovr_sls_get_display_menubar_height_fn g_sls_get_menubar_height = NULL;
static rovr_sls_get_dock_rect_fn g_sls_get_dock_rect = NULL;
static rovr_core_dock_autohide_fn g_core_dock_autohide = NULL;
static rovr_core_dock_orient_fn g_core_dock_orient = NULL;
static rovr_sls_request_notifications_fn g_sls_request_notifications = NULL;

static _Atomic int g_needs_refresh = 0;

static void rovr_display_reconfiguration_callback(
    CGDirectDisplayID display,
    CGDisplayChangeSummaryFlags flags,
    void *userinfo) {
    (void)display;
    (void)flags;
    (void)userinfo;
    atomic_store(&g_needs_refresh, 1);
}

static void rovr_copy_cf_string(CFStringRef value, char *buffer, size_t capacity) {
    if (!buffer || capacity == 0) return;
    buffer[0] = '\0';
    if (!value) return;
    CFStringGetCString(value, buffer, capacity, kCFStringEncodingUTF8);
}

static uint32_t rovr_active_display_id(void) {
    if (!g_sls_main_connection || !g_sls_copy_active_menu_bar_display_identifier) return 0;
    CFStringRef uuid = g_sls_copy_active_menu_bar_display_identifier(g_sls_main_connection());
    if (!uuid) return 0;
    CFUUIDRef parsed = CFUUIDCreateFromString(NULL, uuid);
    CFRelease(uuid);
    if (!parsed) return 0;
    uint32_t display_id = CGDisplayGetDisplayIDFromUUID(parsed);
    CFRelease(parsed);
    return display_id;
}

static uint32_t rovr_display_for_rect(CGRect rect) {
    CGDirectDisplayID displays[8] = {0};
    uint32_t count = 0;
    if (CGGetDisplaysWithRect(rect, 8, displays, &count) != kCGErrorSuccess || count == 0) {
        return 0;
    }
    return displays[0];
}

// yabai's AX_ENHANCED_UI_WORKAROUND: Electron/Chromium set
// AXEnhancedUserInterface=true on themselves and then misapply position and
// size writes while it is active. For apps in that state, flip it off around
// the mutation and restore it afterwards. Apps without the flag are untouched.
static bool rovr_ax_get_enhanced_ui(AXUIElementRef app) {
    Boolean result = false;
    CFTypeRef value = NULL;
    if (AXUIElementCopyAttributeValue(app, CFSTR("AXEnhancedUserInterface"), &value) == kAXErrorSuccess) {
        if (value && CFGetTypeID(value) == CFBooleanGetTypeID()) {
            result = CFBooleanGetValue((CFBooleanRef)value);
        }
        if (value) CFRelease(value);
    }
    return result;
}

static void rovr_ax_set_enhanced_ui(AXUIElementRef app, bool on) {
    AXUIElementSetAttributeValue(app, CFSTR("AXEnhancedUserInterface"), on ? kCFBooleanTrue : kCFBooleanFalse);
}

// ---- Push-based window discovery (AXObserver) ---------------------------
// Per-app AXObserver subscriptions: kAXCreatedNotification fires at window
// CREATION regardless of which Space it lands on, which is how the daemon
// learns about windows that kAXWindowsAttribute (space-filtered) hides.
// Destruction is deliberately NOT subscribed here: the periodic snapshot
// reconciles removals within one interval, and per-window destroyed
// notifications would require tracking every AX element lifetime.

enum {
    ROVR_EVENT_WINDOW_CREATED = 1,
    ROVR_EVENT_WINDOW_FOCUSED = 2,
    ROVR_EVENT_WINDOW_DESTROYED = 4,
};

typedef void (*rovr_ax_event_trampoline_fn)(int event_kind, uint32_t window_id);

static rovr_ax_event_trampoline_fn g_event_trampoline = NULL;

#define ROVR_OBSERVER_MAX 128
struct observed_app {
    pid_t pid;
    AXUIElementRef app;
    AXObserverRef observer;
};
static struct observed_app g_observed_apps[ROVR_OBSERVER_MAX];
static int g_observed_app_count = 0;
static pthread_mutex_t g_observers_lock = PTHREAD_MUTEX_INITIALIZER;

void rovr_bridge_install_event_handlers(rovr_ax_event_trampoline_fn callback) {
    g_event_trampoline = callback;
}

static void rovr_ax_notification_handler(AXObserverRef observer, AXUIElementRef element, CFStringRef notification, void *context) {
    if (!g_event_trampoline || !element) return;
    int kind;
    if (CFStringCompare(notification, kAXCreatedNotification, 0) == kCFCompareEqualTo) {
        kind = ROVR_EVENT_WINDOW_CREATED;
    } else if (CFStringCompare(notification, kAXFocusedWindowChangedNotification, 0) == kCFCompareEqualTo) {
        kind = ROVR_EVENT_WINDOW_FOCUSED;
    } else if (CFStringCompare(notification, kAXUIElementDestroyedNotification, 0) == kCFCompareEqualTo) {
        kind = ROVR_EVENT_WINDOW_DESTROYED;
    } else {
        return;
    }
    CGWindowID gid = 0;
    bool have_gid = g_ax_get_window &&
        g_ax_get_window(element, &gid) == kAXErrorSuccess && gid != 0;
    // The window id only refines WHICH window changed; the refresh signal
    // itself must fire even when the lookup fails (some apps fail
    // _AXUIElementGetWindow, e.g. error -25212), otherwise those focus
    // changes are silently lost and the daemon serves stale state.
    g_event_trampoline(kind, have_gid ? (uint32_t)gid : 0);
}

// SLS connection notify handler (yabai mission_control.c: connection_handler, MIT, Åsmund Vikane).
// Space create/destroy (1327/1328), window ordered/destroyed (808/804), mission control enter (1204).
static void rovr_sls_connection_handler(uint32_t type, void *data, size_t data_length, void *context, int cid) {
    (void)data;
    (void)data_length;
    (void)context;
    (void)cid;
    if (type == 1327 || type == 1328 || type == 808 || type == 804 || type == 1204) {
        atomic_store(&g_needs_refresh, 1);
        if (g_event_trampoline) g_event_trampoline(1, 0);
    }
}

static bool rovr_observer_registered_for_pid(pid_t pid) {
    pthread_mutex_lock(&g_observers_lock);
    for (int i = 0; i < g_observed_app_count; ++i) {
        if (g_observed_apps[i].pid == pid) {
            pthread_mutex_unlock(&g_observers_lock);
            return true;
        }
    }
    pthread_mutex_unlock(&g_observers_lock);
    return false;
}

// Idempotent; called from the observation worker. The observer's run-loop
// source is attached to the MAIN thread's loop, where the AppKit event loop
// already runs, so callbacks are delivered there.
static void rovr_observe_app(pid_t pid) {
    if (rovr_observer_registered_for_pid(pid)) return;
    AXUIElementRef app = AXUIElementCreateApplication(pid);
    rovr_ax_apply_timeout(app);
    AXObserverRef observer = NULL;
    if (!app || AXObserverCreate(pid, rovr_ax_notification_handler, &observer) != kAXErrorSuccess || !observer) {
        if (app) CFRelease(app);
        return;
    }
    bool any = false;
    const CFStringRef notifications[] = { kAXCreatedNotification, kAXFocusedWindowChangedNotification, kAXUIElementDestroyedNotification };
    for (unsigned long i = 0; i < sizeof(notifications) / sizeof(notifications[0]); ++i) {
        AXError err = AXObserverAddNotification(observer, app, notifications[i], (void *)(intptr_t)pid);
        if (err == kAXErrorSuccess || err == kAXErrorNotificationAlreadyRegistered) any = true;
    }
    if (!any) {
        CFRelease(observer);
        CFRelease(app);
        return;
    }
    pthread_mutex_lock(&g_observers_lock);
    if (g_observed_app_count >= ROVR_OBSERVER_MAX) {
        pthread_mutex_unlock(&g_observers_lock);
        CFRelease(observer);
        CFRelease(app);
        return;
    }
    CFRunLoopAddSource(CFRunLoopGetMain(), AXObserverGetRunLoopSource(observer), kCFRunLoopDefaultMode);
    g_observed_apps[g_observed_app_count++] = (struct observed_app){
        .pid = pid, .app = app, .observer = observer,
    };
    pthread_mutex_unlock(&g_observers_lock);
}

// Drop observer entries whose process has exited (called each snapshot).
static void rovr_prune_observers(void) {
    struct observed_app removed[ROVR_OBSERVER_MAX];
    int removed_count = 0;
    pthread_mutex_lock(&g_observers_lock);
    for (int i = g_observed_app_count - 1; i >= 0; --i) {
        pid_t pid = g_observed_apps[i].pid;
        if (kill(pid, 0) == 0 || errno != ESRCH) continue;
        removed[removed_count++] = g_observed_apps[i];
        g_observed_apps[i] = g_observed_apps[g_observed_app_count - 1];
        g_observed_app_count--;
    }
    pthread_mutex_unlock(&g_observers_lock);
    for (int i = 0; i < removed_count; i++) {
        CFRunLoopRemoveSource(CFRunLoopGetMain(), AXObserverGetRunLoopSource(removed[i].observer), kCFRunLoopDefaultMode);
        CFRunLoopSourceInvalidate(AXObserverGetRunLoopSource(removed[i].observer));
        AXObserverRemoveNotification(removed[i].observer, removed[i].app, kAXCreatedNotification);
        AXObserverRemoveNotification(removed[i].observer, removed[i].app, kAXFocusedWindowChangedNotification);
        AXObserverRemoveNotification(removed[i].observer, removed[i].app, kAXUIElementDestroyedNotification);
        CFRelease(removed[i].observer);
        CFRelease(removed[i].app);
    }
}

// Electron/Chromium accessibility enablement: these apps hide their AX tree
// Electron/Chromium accessibility enablement: these apps hide their AX tree
// until an assistive client sets this flag. Remember which pids we have
// already asked so each snapshot does not re-send it (bounded table).
static pid_t g_manual_ax_pids[64] = {0};
static int g_manual_ax_count = 0;
static pthread_mutex_t g_manual_ax_lock = PTHREAD_MUTEX_INITIALIZER;

static bool rovr_manual_ax_already_requested(pid_t pid) {
    bool found = false;
    pthread_mutex_lock(&g_manual_ax_lock);
    for (int i = 0; i < g_manual_ax_count; ++i) {
        if (g_manual_ax_pids[i] == pid) {
            found = true;
            break;
        }
    }
    pthread_mutex_unlock(&g_manual_ax_lock);
    return found;
}

static void rovr_ax_enable_manual_accessibility(AXUIElementRef app) {
    // NOTE: deliberately NOT setting AXEnhancedUserInterface here — when an
    // app has it enabled, frame mutations misapply unless toggled off around
    // the operation (see rovr_bridge_set_window_frame).
    AXUIElementSetAttributeValue(app, CFSTR("AXManualAccessibility"), kCFBooleanTrue);
    pid_t pid = 0;
    if (AXUIElementGetPid(app, &pid) == kAXErrorSuccess) {
        pthread_mutex_lock(&g_manual_ax_lock);
        bool found = false;
        for (int i = 0; i < g_manual_ax_count; ++i) {
            if (g_manual_ax_pids[i] == pid) {
                found = true;
                break;
            }
        }
        if (!found && g_manual_ax_count < 64) {
            g_manual_ax_pids[g_manual_ax_count++] = pid;
        }
        pthread_mutex_unlock(&g_manual_ax_lock);
    }
}

static AXUIElementRef rovr_ax_window_for_id(uint32_t target_id, pid_t *resolved_pid) {
    CFArrayRef window_info = CGWindowListCopyWindowInfo(
        kCGWindowListOptionAll | kCGWindowListExcludeDesktopElements,
        kCGNullWindowID);
    if (!window_info) return NULL;

    pid_t target_pid = 0;
    CFIndex count = CFArrayGetCount(window_info);
    for (CFIndex i = 0; i < count; i++) {
        CFDictionaryRef entry = CFArrayGetValueAtIndex(window_info, i);
        CFNumberRef window_number = CFDictionaryGetValue(entry, kCGWindowNumber);
        int window_id = 0;
        if (window_number && CFNumberGetValue(window_number, kCFNumberIntType, &window_id) &&
            (uint32_t)window_id == target_id) {
            CFNumberRef owner_pid = CFDictionaryGetValue(entry, kCGWindowOwnerPID);
            if (owner_pid) CFNumberGetValue(owner_pid, kCFNumberIntType, &target_pid);
            break;
        }
    }
    CFRelease(window_info);
    if (!target_pid || !g_ax_get_window) return NULL;

    if (resolved_pid) *resolved_pid = target_pid;

    AXUIElementRef app = AXUIElementCreateApplication(target_pid);
    if (!app) return NULL;
    rovr_ax_apply_timeout(app);

    CFTypeRef value = NULL;
    AXError error = AXUIElementCopyAttributeValue(app, kAXWindowsAttribute, &value);
    // Electron/Chromium: an empty window list usually means their AX tree is
    // gated; request manual accessibility and retry once.
    if (error == kAXErrorSuccess && value && CFGetTypeID(value) == CFArrayGetTypeID() &&
        CFArrayGetCount((CFArrayRef)value) == 0 && !rovr_manual_ax_already_requested(target_pid)) {
        rovr_ax_enable_manual_accessibility(app);
        CFRelease(value);
        value = NULL;
        usleep(100000); // give the app a moment to build its AX tree
        error = AXUIElementCopyAttributeValue(app, kAXWindowsAttribute, &value);
    }
    if (error != kAXErrorSuccess || !value || CFGetTypeID(value) != CFArrayGetTypeID()) {
        if (value) CFRelease(value);
        return NULL;
    }

    CFArrayRef windows = (CFArrayRef)value;
    AXUIElementRef result = NULL;
    for (CFIndex i = 0; i < CFArrayGetCount(windows); i++) {
        AXUIElementRef window = (AXUIElementRef)CFArrayGetValueAtIndex(windows, i);
        CGWindowID candidate_id = 0;
        if (g_ax_get_window(window, &candidate_id) == kAXErrorSuccess && candidate_id == target_id) {
            result = (AXUIElementRef)CFRetain(window);
            rovr_ax_apply_timeout(result);
            break;
        }
    }
    CFRelease(windows);

    // Some apps (e.g. WezTerm nightly) expose no kAXWindowsAttribute array at
    // all and ignore AXManualAccessibility, but still report their main/focused
    // window. Match those against the target id so observation and the
    // fullscreen/close button presses keep working for them. (Uses `app`,
    // hence this runs before the release below.)
    if (!result) {
        const CFStringRef single_attrs[2] = { kAXMainWindowAttribute, kAXFocusedWindowAttribute };
        for (int a = 0; a < 2 && !result; ++a) {
            CFTypeRef single = NULL;
            if (AXUIElementCopyAttributeValue(app, single_attrs[a], &single) != kAXErrorSuccess || !single) {
                if (single) CFRelease(single);
                continue;
            }
            if (CFGetTypeID(single) == AXUIElementGetTypeID()) {
                CGWindowID candidate_id = 0;
                if (g_ax_get_window((AXUIElementRef)single, &candidate_id) == kAXErrorSuccess &&
                    candidate_id == target_id) {
                    result = (AXUIElementRef)CFRetain(single);
                    rovr_ax_apply_timeout(result);
                }
            }
            CFRelease(single);
        }
    }

    CFRelease(app);
    return result;
}

static uint32_t rovr_display_for_space(uint64_t sid) {
    if (!g_sls_copy_managed_display_for_space || !g_sls_main_connection || sid == 0) return 0;
    int cid = g_sls_main_connection();
    CFStringRef uuid = g_sls_copy_managed_display_for_space(cid, sid);
    if (!uuid) return 0;
    CFUUIDRef parsed = CFUUIDCreateFromString(NULL, uuid);
    CFRelease(uuid);
    if (!parsed) return 0;
    uint32_t did = CGDisplayGetDisplayIDFromUUID(parsed);
    CFRelease(parsed);
    return did;
}

// Space -> display map built once per snapshot. Resolving each window's
// display through SLSCopyManagedDisplayForSpace individually costs one
// private-API round trip PER WINDOW and measurably slowed every snapshot;
// one SLSCopyManagedDisplaySpaces pass gives every mapping at once.
#define ROVR_SPACE_DISPLAY_MAX 128
struct space_display_entry {
    uint64_t sid;
    uint32_t did;
};

static int rovr_build_space_display_map(struct space_display_entry *out, int capacity) {
    if (!out || capacity <= 0 || !g_sls_copy_managed_display_spaces || !g_sls_main_connection) {
        return 0;
    }
    int cid = g_sls_main_connection();
    CFArrayRef display_spaces = g_sls_copy_managed_display_spaces(cid);
    if (!display_spaces) return 0;
    int count = 0;
    CFIndex display_count = CFArrayGetCount(display_spaces);
    for (CFIndex d = 0; d < display_count && count < capacity; d++) {
        CFDictionaryRef display_ref =
            (CFDictionaryRef)CFArrayGetValueAtIndex(display_spaces, d);
        CFStringRef uuid =
            (CFStringRef)CFDictionaryGetValue(display_ref, CFSTR("Display Identifier"));
        CFArrayRef spaces_ref =
            (CFArrayRef)CFDictionaryGetValue(display_ref, CFSTR("Spaces"));
        if (!uuid || !spaces_ref) continue;
        uint32_t did = 0;
        CFUUIDRef parsed = CFUUIDCreateFromString(NULL, uuid);
        if (parsed) {
            did = CGDisplayGetDisplayIDFromUUID(parsed);
            CFRelease(parsed);
        }
        if (did == 0) continue;
        CFIndex space_count = CFArrayGetCount(spaces_ref);
        for (CFIndex s = 0; s < space_count && count < capacity; s++) {
            CFDictionaryRef space_ref =
                (CFDictionaryRef)CFArrayGetValueAtIndex(spaces_ref, s);
            CFNumberRef sid_ref = (CFNumberRef)CFDictionaryGetValue(space_ref, CFSTR("id64"));
            if (!sid_ref) continue;
            uint64_t sid = 0;
            CFNumberGetValue(sid_ref, kCFNumberSInt64Type, &sid);
            out[count].sid = sid;
            out[count].did = did;
            count++;
        }
    }
    CFRelease(display_spaces);
    return count;
}

static uint32_t rovr_lookup_display_for_space(
    const struct space_display_entry *map, int map_len, uint64_t sid) {
    if (sid == 0) return 0;
    for (int i = 0; i < map_len; i++) {
        if (map[i].sid == sid) return map[i].did;
    }
    return 0;
}

// wid -> sid map built once per snapshot. The previous approach called
// SLSCopySpacesForWindows once PER WINDOW (one private-API round trip each);
// with ~100 windows that alone made every snapshot take 200-500 ms, which is
// the visible delay between a window spawning and Rovr tiling it. yabai's
// approach: SLSCopyWindowsWithOptionsAndTags per SPACE (a handful of calls)
// gives every on-screen window's space in one shot. Windows not found
// (minimized / transient) fall back to the old per-window call.
typedef CFArrayRef (*rovr_sls_copy_windows_with_options_and_tags_fn)(
    int cid, uint32_t owner, CFArrayRef spaces, uint32_t options,
    uint64_t *set_tags, uint64_t *clear_tags);
static rovr_sls_copy_windows_with_options_and_tags_fn g_sls_copy_windows_with_options_and_tags = NULL;

#define ROVR_WINDOW_SPACE_MAX 1024
struct window_space_entry {
    uint32_t wid;
    uint64_t sid;
};

static int rovr_build_window_space_map(struct window_space_entry *out, int capacity) {
    if (!out || capacity <= 0 || !g_sls_main_connection ||
        !g_sls_copy_windows_with_options_and_tags) {
        return 0;
    }
    int cid = g_sls_main_connection();

    struct space_display_entry spaces[ROVR_SPACE_DISPLAY_MAX];
    // Reuse the space list builder: we only need the sids here.
    int space_count = rovr_build_space_display_map(spaces, ROVR_SPACE_DISPLAY_MAX);
    if (space_count <= 0) return 0;

    int count = 0;
    for (int s = 0; s < space_count && count < capacity; s++) {
        CFNumberRef sid_ref =
            CFNumberCreate(NULL, kCFNumberSInt64Type, &spaces[s].sid);
        if (!sid_ref) continue;
        const void *sid_values[1] = { sid_ref };
        CFArrayRef space_list =
            CFArrayCreate(NULL, sid_values, 1, &kCFTypeArrayCallBacks);
        CFRelease(sid_ref);
        if (!space_list) continue;

        // 0x2 = on-screen windows only (yabai's non-minimized option).
        // set/clear tags must be real addresses — passing NULL segfaults
        // inside SkyLight (yabai always passes locals).
        uint64_t set_tags = 0;
        uint64_t clear_tags = 0;
        CFArrayRef space_windows = g_sls_copy_windows_with_options_and_tags(
            cid, 0, space_list, 0x2, &set_tags, &clear_tags);
        CFRelease(space_list);
        if (!space_windows) continue;

        CFIndex w_count = CFArrayGetCount(space_windows);
        for (CFIndex i = 0; i < w_count && count < capacity; i++) {
            CFNumberRef wid_ref = (CFNumberRef)CFArrayGetValueAtIndex(space_windows, i);
            uint32_t wid = 0;
            if (wid_ref && CFNumberGetValue(wid_ref, kCFNumberIntType, &wid) && wid != 0) {
                out[count].wid = wid;
                out[count].sid = spaces[s].sid;
                count++;
            }
        }
        CFRelease(space_windows);
    }
    return count;
}

static uint64_t rovr_lookup_space_for_window(
    const struct window_space_entry *map, int map_len, uint32_t wid) {
    if (wid == 0) return 0;
    for (int i = 0; i < map_len; i++) {
        if (map[i].wid == wid) return map[i].sid;
    }
    return 0;
}

static uint64_t rovr_space_id_for_window(uint32_t wid) {
    if (!g_sls_copy_spaces_for_windows || !g_sls_main_connection) return 0;
    int cid = g_sls_main_connection();
    CFNumberRef wid_ref = CFNumberCreate(NULL, kCFNumberSInt32Type, &wid);
    if (!wid_ref) return 0;
    const void *values[1] = { wid_ref };
    CFArrayRef window_list = CFArrayCreate(NULL, values, 1, &kCFTypeArrayCallBacks);
    CFRelease(wid_ref);
    if (!window_list) return 0;
    CFArrayRef space_list = g_sls_copy_spaces_for_windows(cid, 0x7, window_list);
    CFRelease(window_list);
    if (!space_list) return 0;
    uint64_t sid = 0;
    if (CFArrayGetCount(space_list) > 0) {
        CFNumberRef sid_ref = (CFNumberRef)CFArrayGetValueAtIndex(space_list, 0);
        CFNumberGetValue(sid_ref, kCFNumberSInt64Type, &sid);
    }
    CFRelease(space_list);
    return sid;
}

// The Space a window lives on, 0 if unknown (SLS unavailable, window
// minimized or transient). Used for cross-space window focus.
uint64_t rovr_bridge_window_space_id(uint32_t window_id) {
    return rovr_space_id_for_window(window_id);
}

int32_t rovr_bridge_window_pid(uint32_t target_id) {
    CFArrayRef window_info = CGWindowListCopyWindowInfo(
        kCGWindowListOptionAll | kCGWindowListExcludeDesktopElements,
        kCGNullWindowID);
    if (!window_info) return 0;
    int32_t result = 0;
    CFIndex count = CFArrayGetCount(window_info);
    for (CFIndex i = 0; i < count; i++) {
        CFDictionaryRef entry = CFArrayGetValueAtIndex(window_info, i);
        CFNumberRef number = CFDictionaryGetValue(entry, kCGWindowNumber);
        int window_id = 0;
        if (!number || !CFNumberGetValue(number, kCFNumberIntType, &window_id) ||
            (uint32_t)window_id != target_id) continue;
        CFNumberRef pid = CFDictionaryGetValue(entry, kCGWindowOwnerPID);
        if (pid) CFNumberGetValue(pid, kCFNumberSInt32Type, &result);
        break;
    }
    CFRelease(window_info);
    return result;
}

// 0 = false, 1 = true, 2 = unknown (AX unavailable / race / attribute missing).
static int rovr_ax_bool_for_window(AXUIElementRef window, CFStringRef attribute) {
    if (!window || !attribute) return 2;
    CFTypeRef value = NULL;
    AXError err = AXUIElementCopyAttributeValue(window, attribute, &value);
    if (err != kAXErrorSuccess || !value) {
        if (value) CFRelease(value);
        return 2;
    }
    int result = 2;
    if (CFGetTypeID(value) == CFBooleanGetTypeID()) {
        result = CFBooleanGetValue((CFBooleanRef)value) ? 1 : 0;
    }
    CFRelease(value);
    return result;
}

// 1 = attribute present with a non-NULL value (any type), 0 = supported but
// NULL / unsupported, 2 = query failed. Used for element-valued attributes
// like the window-control buttons, which are not booleans.
static int rovr_ax_present_for_window(AXUIElementRef window, CFStringRef attribute) {
    if (!window || !attribute) return 2;
    CFTypeRef value = NULL;
    AXError err = AXUIElementCopyAttributeValue(window, attribute, &value);
    if (err != kAXErrorSuccess) return 0;
    if (!value) return 0;
    CFRelease(value);
    return 1;
}

static int rovr_ax_managed_for_window(AXUIElementRef window) {
    if (!window) return 2;
    CFTypeRef role = NULL;
    if (AXUIElementCopyAttributeValue(window, kAXRoleAttribute, &role) == kAXErrorSuccess && role) {
        BOOL is_window = (CFStringCompare((CFStringRef)role, CFSTR("AXWindow"), 0) == kCFCompareEqualTo);
        CFRelease(role);
        if (!is_window) return 0;
    } else {
        if (role) CFRelease(role);
        return 2;
    }
    CFTypeRef subrole = NULL;
    if (AXUIElementCopyAttributeValue(window, kAXSubroleAttribute, &subrole) == kAXErrorSuccess && subrole) {
        BOOL floating = NO;
        if (CFStringCompare((CFStringRef)subrole, CFSTR("AXDialog"), 0) == kCFCompareEqualTo ||
            CFStringCompare((CFStringRef)subrole, CFSTR("AXSystemDialog"), 0) == kCFCompareEqualTo ||
            CFStringCompare((CFStringRef)subrole, CFSTR("AXFloatingWindow"), 0) == kCFCompareEqualTo ||
            CFStringCompare((CFStringRef)subrole, CFSTR("AXSystemFloatingWindow"), 0) == kCFCompareEqualTo ||
            CFStringCompare((CFStringRef)subrole, CFSTR("AXPopover"), 0) == kCFCompareEqualTo) {
            floating = YES;
        }
        CFRelease(subrole);
        if (floating) return 0;
    } else {
        if (subrole) CFRelease(subrole);
    }
    // Toast/HUD filter: notification banners and overlays (e.g. browser web
    // notification toasts, Chromium popup overlays) can present as plain
    // AXWindows with NO floating subrole. Two independent signals mark them:
    //   1. not activatable — a toast cannot take keyboard focus;
    //   2. no window-control buttons at all (close/minimize/zoom all absent
    //      or unsupported) — borderless chrome-less overlay windows.
    // Signal 2 matters because Chromium apps do not expose "AXActivatable"
    // at all (kAXErrorAttributeUnsupported on every window). A window that
    // exposes ANY control button keeps the verdict above; unknowns stay
    // honest instead of guessing.
    if (rovr_ax_bool_for_window(window, CFSTR("AXActivatable")) == 0) return 0;
    int close = rovr_ax_present_for_window(window, kAXCloseButtonAttribute);
    int minimize = rovr_ax_present_for_window(window, kAXMinimizeButtonAttribute);
    int zoom = rovr_ax_present_for_window(window, kAXZoomButtonAttribute);
    if (close == 0 && minimize == 0 && zoom == 0) return 0;
    return 1;
}

static int rovr_mission_control_index(int cid, uint64_t sid) {
    CFArrayRef display_spaces = g_sls_copy_managed_display_spaces(cid);
    if (!display_spaces) return 0;
    int desktop_cnt = 1;
    CFIndex display_count = CFArrayGetCount(display_spaces);
    for (CFIndex d = 0; d < display_count; d++) {
        CFDictionaryRef display_ref = (CFDictionaryRef)CFArrayGetValueAtIndex(display_spaces, d);
        CFArrayRef spaces_ref = (CFArrayRef)CFDictionaryGetValue(display_ref, CFSTR("Spaces"));
        if (!spaces_ref) continue;
        CFIndex space_count = CFArrayGetCount(spaces_ref);
        for (CFIndex s = 0; s < space_count; s++) {
            CFDictionaryRef space_ref = (CFDictionaryRef)CFArrayGetValueAtIndex(spaces_ref, s);
            CFNumberRef sid_ref = (CFNumberRef)CFDictionaryGetValue(space_ref, CFSTR("id64"));
            if (sid_ref) {
                uint64_t candidate = 0;
                CFNumberGetValue(sid_ref, kCFNumberSInt64Type, &candidate);
                if (sid == candidate) {
                    CFRelease(display_spaces);
                    return desktop_cnt;
                }
            }
            desktop_cnt++;
        }
    }
    CFRelease(display_spaces);
    return 0;
}

// Resolve local-linkage symbols that dlsym cannot return. Adapted from
// yabai's src/misc/macho_dlsym.h (MIT, © 2019 Åsmund Vikane): walk the dyld
// image list for SkyLight, then scan its symtab for the mangled name.
static void *rovr_macho_find_symbol(const char *target_image, const char *target_symbol) {
    uint32_t image_count = _dyld_image_count();
    for (uint32_t i = 0; i < image_count; i++) {
        const char *image_name = _dyld_get_image_name(i);
        if (!image_name || strcmp(image_name, target_image) != 0) continue;

        uintptr_t slide = _dyld_get_image_vmaddr_slide(i);
        const struct mach_header_64 *header =
            (const struct mach_header_64 *)_dyld_get_image_header(i);
        if (!header) return NULL;

        const struct segment_command_64 *linkedit = NULL;
        const struct symtab_command *symtab = NULL;
        uintptr_t offset = sizeof(struct mach_header_64);
        for (uint32_t c = 0; c < header->ncmds; c++) {
            const struct load_command *cmd = (const struct load_command *)((uintptr_t)header + offset);
            if (cmd->cmd == LC_SEGMENT_64) {
                const struct segment_command_64 *segment = (const struct segment_command_64 *)cmd;
                if (strcmp(segment->segname, SEG_LINKEDIT) == 0) linkedit = segment;
            } else if (cmd->cmd == LC_SYMTAB) {
                symtab = (const struct symtab_command *)cmd;
            }
            offset += cmd->cmdsize;
        }
        if (!linkedit || !symtab) return NULL;

        uintptr_t base = (uintptr_t)(linkedit->vmaddr - linkedit->fileoff) + slide;
        const char *strings = (const char *)(base + symtab->stroff);
        const struct nlist_64 *symbols = (const struct nlist_64 *)(base + symtab->symoff);
        for (uint32_t s = 0; s < symtab->nsyms; s++) {
            if (strcmp(strings + symbols[s].n_un.n_strx, target_symbol) == 0) {
                return (void *)(symbols[s].n_value + slide);
            }
        }
        return NULL;
    }
    return NULL;
}

static const char *ROVR_SKYLIGHT_PATH =
    "/System/Library/PrivateFrameworks/SkyLight.framework/Versions/A/SkyLight";

int rovr_bridge_init(void) {
    g_ax_get_window = (rovr_ax_get_window_fn)dlsym(RTLD_DEFAULT, "_AXUIElementGetWindow");
    g_sls_main_connection = (rovr_sls_main_connection_fn)dlsym(RTLD_DEFAULT, "SLSMainConnectionID");
    g_sls_copy_managed_display_spaces =
        (rovr_sls_copy_managed_display_spaces_fn)dlsym(RTLD_DEFAULT, "SLSCopyManagedDisplaySpaces");
    g_sls_copy_managed_display_for_space =
        (rovr_sls_copy_managed_display_for_space_fn)dlsym(RTLD_DEFAULT, "SLSCopyManagedDisplayForSpace");
    g_sls_managed_display_get_current_space =
        (rovr_sls_managed_display_get_current_space_fn)dlsym(RTLD_DEFAULT, "SLSManagedDisplayGetCurrentSpace");
    g_sls_space_get_type = (rovr_sls_space_get_type_fn)dlsym(RTLD_DEFAULT, "SLSSpaceGetType");
    g_sls_move_windows_to_managed_space =
        (rovr_sls_move_windows_to_managed_space_fn)dlsym(RTLD_DEFAULT, "SLSMoveWindowsToManagedSpace");
    g_sls_space_set_compat_id = (rovr_sls_space_set_compat_id_fn)dlsym(RTLD_DEFAULT, "SLSSpaceSetCompatID");
    g_sls_set_window_list_workspace =
        (rovr_sls_set_window_list_workspace_fn)dlsym(RTLD_DEFAULT, "SLSSetWindowListWorkspace");
    g_sls_copy_spaces_for_windows =
        (rovr_sls_copy_spaces_for_windows_fn)dlsym(RTLD_DEFAULT, "SLSCopySpacesForWindows");
    g_sls_copy_windows_with_options_and_tags =
        (rovr_sls_copy_windows_with_options_and_tags_fn)dlsym(
            RTLD_DEFAULT, "SLSCopyWindowsWithOptionsAndTags");
    g_sls_set_active_menu_bar_display_identifier =
        (rovr_sls_set_active_menu_bar_display_identifier_fn)dlsym(
            RTLD_DEFAULT, "SLSSetActiveMenuBarDisplayIdentifier");
    g_sls_copy_active_menu_bar_display_identifier =
        (rovr_sls_copy_active_menu_bar_display_identifier_fn)dlsym(
            RTLD_DEFAULT, "SLSCopyActiveMenuBarDisplayIdentifier");
    g_sls_managed_display_is_animating =
        (rovr_sls_managed_display_is_animating_fn)dlsym(RTLD_DEFAULT, "SLSManagedDisplayIsAnimating");
    g_sls_window_query = (rovr_sls_window_query_fn)dlsym(RTLD_DEFAULT, "SLSWindowQueryWindows");
    g_sls_window_query_copy = (rovr_sls_window_query_copy_fn)dlsym(RTLD_DEFAULT, "SLSWindowQueryResultCopyWindows");
    g_sls_window_iter_count = (rovr_sls_window_iter_count_fn)dlsym(RTLD_DEFAULT, "SLSWindowIteratorGetCount");
    g_sls_window_iter_advance = (rovr_sls_window_iter_advance_fn)dlsym(RTLD_DEFAULT, "SLSWindowIteratorAdvance");
    g_sls_window_iter_parent = (rovr_sls_window_iter_parent_fn)dlsym(RTLD_DEFAULT, "SLSWindowIteratorGetParentID");
    g_sls_window_iter_level = (rovr_sls_window_iter_level_fn)dlsym(RTLD_DEFAULT, "SLSWindowIteratorGetLevel");
    g_sls_get_menu_autohide = (rovr_sls_get_menu_autohide_fn)dlsym(RTLD_DEFAULT, "SLSGetMenuBarAutohideEnabled");
    g_sls_get_revealed_menu = (rovr_sls_get_revealed_menu_bounds_fn)dlsym(RTLD_DEFAULT, "SLSGetRevealedMenuBarBounds");
    g_sls_get_menubar_height = (rovr_sls_get_display_menubar_height_fn)dlsym(RTLD_DEFAULT, "SLSGetDisplayMenubarHeight");
    g_sls_get_dock_rect = (rovr_sls_get_dock_rect_fn)dlsym(RTLD_DEFAULT, "SLSGetDockRectWithReason");
    g_core_dock_autohide = (rovr_core_dock_autohide_fn)dlsym(RTLD_DEFAULT, "CoreDockGetAutoHideEnabled");
    g_core_dock_orient = (rovr_core_dock_orient_fn)dlsym(RTLD_DEFAULT, "CoreDockGetOrientationAndPinning");
    g_sls_request_notifications = (rovr_sls_request_notifications_fn)dlsym(RTLD_DEFAULT, "SLSRequestNotificationsForWindows");
    g_sls_register_notify =
        (rovr_sls_register_notify_fn)dlsym(RTLD_DEFAULT, "SLSRegisterConnectionNotifyProc");
    if (g_sls_register_notify && g_sls_main_connection) {
        int cid = g_sls_main_connection();
        g_sls_register_notify(cid, (void *)rovr_sls_connection_handler, 1327, NULL);
        g_sls_register_notify(cid, (void *)rovr_sls_connection_handler, 1328, NULL);
        g_sls_register_notify(cid, (void *)rovr_sls_connection_handler, 808, NULL);
        g_sls_register_notify(cid, (void *)rovr_sls_connection_handler, 804, NULL);
        g_sls_register_notify(cid, (void *)rovr_sls_connection_handler, 1204, NULL);
    }
    g_sls_perform_async_bridged_op = (rovr_sls_perform_async_bridged_op_fn)rovr_macho_find_symbol(
        ROVR_SKYLIGHT_PATH,
        "__ZL54SLSPerformAsynchronousBridgedWindowManagementOperationP47SLSAsynchronousBridgedWindowManagementOperation");
    CGDisplayRegisterReconfigurationCallback(rovr_display_reconfiguration_callback, NULL);
    return 0;
}

uint64_t rovr_bridge_capabilities(void) {
    uint64_t capabilities = ROVR_CAP_OBSERVE_WINDOWS;
    if (g_ax_get_window) {
        capabilities |= ROVR_CAP_SET_WINDOW_FRAME;
        capabilities |= ROVR_CAP_FOCUS_WINDOW;
    }
    // Honest TCC visibility: doctor surfaces whether this process is trusted
    // for AX at all — a missing grant otherwise looks identical to every app
    // having an empty AX window list.
    if (!AXIsProcessTrusted()) {
        capabilities &= ~(ROVR_CAP_SET_WINDOW_FRAME | ROVR_CAP_FOCUS_WINDOW);
    }
    if (g_sls_copy_managed_display_spaces) {
        capabilities |= ROVR_CAP_OBSERVE_SPACES;
    }
    if (g_sls_move_windows_to_managed_space ||
        (g_sls_space_set_compat_id && g_sls_set_window_list_workspace)) {
        capabilities |= ROVR_CAP_MOVE_WINDOW_TO_SPACE;
    }
    // Focus-space via CGEvent gesture synthesis needs no SkyLight symbols, but
    // it is only meaningful once spaces can be observed.
    if (g_sls_copy_managed_display_spaces && g_sls_copy_managed_display_for_space &&
        g_sls_managed_display_get_current_space) {
        capabilities |= ROVR_CAP_FOCUS_SPACE;
    }
    // Create/destroy space are SA-only; reported false until the SA client
    // probes the payload.
    return capabilities;
}

int rovr_bridge_enumerate_window_candidates(rovr_window_callback callback, void *context) {
    if (!callback) return 1;

    #define ROVR_ENUM_MAX_WINDOWS 512
    @autoreleasepool {
        CFArrayRef list = CGWindowListCopyWindowInfo(
            kCGWindowListOptionAll | kCGWindowListExcludeDesktopElements,
            kCGNullWindowID);
        if (!list) return 2;

        struct space_display_entry space_displays[ROVR_SPACE_DISPLAY_MAX];
        const int space_display_count =
            rovr_build_space_display_map(space_displays, ROVR_SPACE_DISPLAY_MAX);
        struct window_space_entry window_spaces[ROVR_WINDOW_SPACE_MAX];
        const int window_space_count =
            rovr_build_window_space_map(window_spaces, ROVR_WINDOW_SPACE_MAX);

        int emitted = 0;
        uint32_t emitted_ids[ROVR_ENUM_MAX_WINDOWS] = {0};
        CFIndex count = CFArrayGetCount(list);
        for (CFIndex i = 0; i < count && emitted < ROVR_ENUM_MAX_WINDOWS; i++) {
            CFDictionaryRef entry = CFArrayGetValueAtIndex(list, i);
            CFNumberRef layer_number = CFDictionaryGetValue(entry, kCGWindowLayer);
            int layer = 0;
            if (layer_number) CFNumberGetValue(layer_number, kCFNumberIntType, &layer);
            if (layer != 0) continue;

            int window_id = 0;
            int owner_pid = 0;
            CGRect bounds = CGRectZero;
            CFNumberRef number = CFDictionaryGetValue(entry, kCGWindowNumber);
            CFNumberRef pid = CFDictionaryGetValue(entry, kCGWindowOwnerPID);
            CFDictionaryRef bounds_dict = CFDictionaryGetValue(entry, kCGWindowBounds);
            if (!number || !pid || !bounds_dict) continue;
            if (!CFNumberGetValue(number, kCFNumberIntType, &window_id)) continue;
            if (!CFNumberGetValue(pid, kCFNumberIntType, &owner_pid)) continue;
            if (!CGRectMakeWithDictionaryRepresentation(bounds_dict, &bounds)) continue;
            if (bounds.size.width <= 1.0 || bounds.size.height <= 1.0) continue;

            rovr_bridge_window window = {0};
            window.id = (uint32_t)window_id;
            window.pid = owner_pid;
            window.space_id = rovr_lookup_space_for_window(
                window_spaces, window_space_count, window.id);
            uint32_t space_display = rovr_lookup_display_for_space(
                space_displays, space_display_count, window.space_id);
            window.display_id = space_display != 0 ? space_display : rovr_display_for_rect(bounds);
            window.x = bounds.origin.x;
            window.y = bounds.origin.y;
            window.width = bounds.size.width;
            window.height = bounds.size.height;
            window.minimized = 2;
            window.fullscreen = 2;
            window.managed = 2;
            rovr_copy_cf_string(CFDictionaryGetValue(entry, kCGWindowOwnerName),
                                window.app, sizeof(window.app));
            rovr_copy_cf_string(CFDictionaryGetValue(entry, kCGWindowName),
                                window.title, sizeof(window.title));
            NSRunningApplication *application =
                [NSRunningApplication runningApplicationWithProcessIdentifier:owner_pid];
            if (application.bundleIdentifier) {
                [application.bundleIdentifier getCString:window.bundle_id
                                               maxLength:sizeof(window.bundle_id)
                                                encoding:NSUTF8StringEncoding];
            }
            callback(&window, context);
            emitted_ids[emitted] = window.id;
            emitted++;
        }
        // SLS window notifications on Sequoia+ (yabai update_window_notifications, MIT) — enables 804/808 events.
        if (emitted > 0 && g_sls_request_notifications && g_sls_main_connection) {
            NSOperatingSystemVersion v = [[NSProcessInfo processInfo] operatingSystemVersion];
            if (v.majorVersion >= 15) {
                int cid = g_sls_main_connection();
                g_sls_request_notifications(cid, emitted_ids, emitted);
            }
        }
        CFRelease(list);
        return 0;
    }
}

int rovr_bridge_refine_windows_for_pid(
    int32_t pid,
    rovr_ax_window_callback callback,
    void *context) {
    if (pid <= 0 || !callback || !g_ax_get_window) return 1;

    #define ROVR_AX_MAX_WINDOWS 512
    @autoreleasepool {
        rovr_prune_observers();
        rovr_observe_app(pid);
        AXUIElementRef app = AXUIElementCreateApplication(pid);
        if (!app) return 2;
        rovr_ax_apply_timeout(app);

        uint32_t ids[ROVR_AX_MAX_WINDOWS];
        AXUIElementRef elements[ROVR_AX_MAX_WINDOWS];
        int ax_count = 0;
        CFTypeRef value = NULL;
        bool got_windows =
            AXUIElementCopyAttributeValue(app, kAXWindowsAttribute, &value) == kAXErrorSuccess &&
            value && CFGetTypeID(value) == CFArrayGetTypeID();
        if (got_windows && CFArrayGetCount((CFArrayRef)value) == 0 &&
            !rovr_manual_ax_already_requested(pid)) {
            rovr_ax_enable_manual_accessibility(app);
        }
        if (got_windows) {
            CFArrayRef windows = (CFArrayRef)value;
            for (CFIndex i = 0;
                 i < CFArrayGetCount(windows) && ax_count < ROVR_AX_MAX_WINDOWS;
                 i++) {
                AXUIElementRef window = (AXUIElementRef)CFArrayGetValueAtIndex(windows, i);
                if (!window || CFGetTypeID(window) != AXUIElementGetTypeID()) continue;
                rovr_ax_apply_timeout(window);
                CGWindowID gid = 0;
                if (g_ax_get_window(window, &gid) == kAXErrorSuccess && gid != 0) {
                    ids[ax_count] = (uint32_t)gid;
                    elements[ax_count++] = (AXUIElementRef)CFRetain(window);
                }
            }
            CFRelease(value);
        } else {
            if (value) CFRelease(value);
            if (!rovr_manual_ax_already_requested(pid)) {
                rovr_ax_enable_manual_accessibility(app);
            }
        }

        uint32_t focused_id = 0;
        CFTypeRef focused = NULL;
        if (AXUIElementCopyAttributeValue(app, kAXFocusedWindowAttribute, &focused) ==
                kAXErrorSuccess && focused &&
            CFGetTypeID(focused) == AXUIElementGetTypeID()) {
            g_ax_get_window((AXUIElementRef)focused, &focused_id);
        }

        if (ax_count == 0) {
            const CFStringRef attrs[2] = { kAXMainWindowAttribute, kAXFocusedWindowAttribute };
            for (int a = 0; a < 2 && ax_count < ROVR_AX_MAX_WINDOWS; a++) {
                CFTypeRef single = NULL;
                if (AXUIElementCopyAttributeValue(app, attrs[a], &single) != kAXErrorSuccess ||
                    !single) {
                    if (single) CFRelease(single);
                    continue;
                }
                if (CFGetTypeID(single) == AXUIElementGetTypeID()) {
                    rovr_ax_apply_timeout((AXUIElementRef)single);
                    CGWindowID gid = 0;
                    if (g_ax_get_window((AXUIElementRef)single, &gid) == kAXErrorSuccess &&
                        gid != 0) {
                        bool seen = false;
                        for (int i = 0; i < ax_count; i++) {
                            if (ids[i] == (uint32_t)gid) { seen = true; break; }
                        }
                        if (!seen) {
                            ids[ax_count] = (uint32_t)gid;
                            elements[ax_count++] = (AXUIElementRef)CFRetain(single);
                        }
                    }
                }
                CFRelease(single);
            }
        }
        if (focused) CFRelease(focused);

        for (int i = 0; i < ax_count; i++) {
            rovr_bridge_ax_window refinement = {
                .id = ids[i],
                .focused = ids[i] == focused_id ? 1 : 0,
                .minimized = (uint8_t)rovr_ax_bool_for_window(
                    elements[i], kAXMinimizedAttribute),
                .fullscreen = (uint8_t)rovr_ax_bool_for_window(
                    elements[i], CFSTR("AXFullScreen")),
                .managed = (uint8_t)rovr_ax_managed_for_window(elements[i]),
            };
            callback(&refinement, context);
            CFRelease(elements[i]);
        }
        CFRelease(app);
        return 0;
    }
}

// Adapted from yabai display_bounds_constrained (MIT, Åsmund Vikane).
// Usable area excluding dock/menubar/notch. Tries private SLS path for accuracy
// (hidden menubar + notch, dock orientation) and falls back to NSScreen visibleFrame.
static int rovr_display_notch_height(uint32_t did) {
    if (!CGDisplayIsBuiltin(did)) return 0;
    if (@available(macos 12.0, *)) {
        for (NSScreen *screen in [NSScreen screens]) {
            NSNumber *num = screen.deviceDescription[@"NSScreenNumber"];
            if (num && [num unsignedIntValue] == did) {
                return (int)round(screen.safeAreaInsets.top);
            }
        }
    }
    return 0;
}

static bool rovr_menu_bar_hidden(void) {
    if (g_sls_get_menu_autohide && g_sls_main_connection) {
        int enabled = 0;
        if (g_sls_get_menu_autohide(g_sls_main_connection(), &enabled) == kCGErrorSuccess) return enabled != 0;
    }
    return false;
}

static bool rovr_dock_hidden(void) {
    if (g_core_dock_autohide) return g_core_dock_autohide();
    return false;
}

static CGRect rovr_display_constrained_bounds(CGDirectDisplayID did) {
    CGRect frame = CGDisplayBounds(did);
    bool canUseSLS = g_sls_main_connection && g_sls_get_dock_rect;
    if (canUseSLS) {
        int cid = g_sls_main_connection();
        if (rovr_menu_bar_hidden()) {
            int notch = rovr_display_notch_height(did);
            if (notch > 0) {
                frame.origin.y += notch;
                frame.size.height -= notch;
            }
        } else {
            uint64_t sid = 0;
            if (g_sls_copy_managed_display_spaces) {
                CFArrayRef spaces = g_sls_copy_managed_display_spaces(cid);
                if (spaces) {
                    CFIndex dc = CFArrayGetCount(spaces);
                    for (CFIndex d = 0; d < dc; d++) {
                        CFDictionaryRef dr = CFArrayGetValueAtIndex(spaces, d);
                        CFStringRef uuid = CFDictionaryGetValue(dr, CFSTR("Display Identifier"));
                        CFArrayRef sarr = CFDictionaryGetValue(dr, CFSTR("Spaces"));
                        if (!uuid || !sarr) continue;
                        CFUUIDRef pu = CFUUIDCreateFromString(NULL, uuid);
                        uint32_t curDid = pu ? CGDisplayGetDisplayIDFromUUID(pu) : 0;
                        if (pu) CFRelease(pu);
                        if (curDid == did && CFArrayGetCount(sarr) > 0) {
                            CFDictionaryRef sr = CFArrayGetValueAtIndex(sarr, 0);
                            CFNumberRef sidRef = CFDictionaryGetValue(sr, CFSTR("id64"));
                            if (sidRef) CFNumberGetValue(sidRef, kCFNumberSInt64Type, &sid);
                            break;
                        }
                    }
                    CFRelease(spaces);
                }
            }
            CGRect menu = {0};
            bool gotMenu = false;
            if (sid != 0 && g_sls_get_revealed_menu) {
                if (g_sls_get_revealed_menu(&menu, cid, sid) == kCGErrorSuccess) gotMenu = true;
            }
            if (!gotMenu && g_sls_get_menubar_height) {
                uint32_t h = 0;
                if (g_sls_get_menubar_height(did, &h) == kCGErrorSuccess && h > 0) {
                    menu.size.height = h;
                    gotMenu = true;
                }
            }
            if (gotMenu && menu.size.height > 0) {
                if (menu.size.height > 1) menu.size.height += 1;
                frame.origin.y += menu.size.height;
                frame.size.height -= menu.size.height;
            } else {
                // Fallback notch already handled? Use visibleFrame height delta.
                for (NSScreen *screen in [NSScreen screens]) {
                    NSNumber *num = screen.deviceDescription[@"NSScreenNumber"];
                    if (num && [num unsignedIntValue] == did) {
                        CGRect vf = [screen visibleFrame];
                        CGFloat delta = (frame.size.height - vf.size.height);
                        if (delta > 0 && delta < 100) {
                            frame.origin.y += delta;
                            frame.size.height -= delta;
                        }
                        break;
                    }
                }
            }
        }
        if (!rovr_dock_hidden() && g_sls_get_dock_rect) {
            CGRect dock = {0}; int reason = 0;
            if (g_sls_get_dock_rect(cid, &dock, &reason) == kCGErrorSuccess) {
                // Use yabai's logic: only subtract dock if did == dock display.
                // Heuristic: if dock rect lies within this display's bounds (before subtraction), apply.
                if (CGRectIntersectsRect(frame, dock) || CGRectContainsRect(CGDisplayBounds(did), dock)) {
                    int orient = 0, pinning = 0;
                    if (g_core_dock_orient) g_core_dock_orient(&orient, &pinning);
                    else orient = 2; // bottom default
                    switch (orient) {
                        case 0: // left
                            if (dock.size.width > 0) { frame.origin.x += dock.size.width; frame.size.width -= dock.size.width; }
                            break;
                        case 1: // right
                            frame.size.width -= dock.size.width; break;
                        case 2: // bottom
                        default: frame.size.height -= dock.size.height; break;
                    }
                }
            }
        }
        // Clamp to positive
        if (frame.size.width < 0) frame.size.width = 0;
        if (frame.size.height < 0) frame.size.height = 0;
        return frame;
    }
    // Fallback: NSScreen visibleFrame (covers dock+menubar+notch on most configs)
    @autoreleasepool {
        for (NSScreen *screen in [NSScreen screens]) {
            NSNumber *num = screen.deviceDescription[@"NSScreenNumber"];
            if (num && [num unsignedIntValue] == did) {
                return [screen visibleFrame];
            }
        }
    }
    return frame;
}

// Adapted from yabai display_manager_display_is_animating (MIT, © Åsmund Vikane).
static bool rovr_display_is_animating(uint32_t did) {
    if (!g_sls_managed_display_is_animating || !g_sls_main_connection) return false;
    CFUUIDRef uuidRef = CGDisplayCreateUUIDFromDisplayID(did);
    if (!uuidRef) return false;
    CFStringRef uuid = CFUUIDCreateString(NULL, uuidRef);
    CFRelease(uuidRef);
    if (!uuid) return false;
    bool result = g_sls_managed_display_is_animating(g_sls_main_connection(), uuid);
    CFRelease(uuid);
    return result;
}

// SLS fallback for background apps where AX returns Unknown. Uses level/parent to decide
// if window is a standard user window (level 0/3/8, parent 0) — yabai window_manager.c / window.c logic (MIT).
static int rovr_sls_managed_for_window(uint32_t wid) {
    if (!g_sls_window_query || !g_sls_window_query_copy || !g_sls_window_iter_count || !g_sls_window_iter_advance || !g_sls_window_iter_parent || !g_sls_window_iter_level || !g_sls_main_connection) return 2;
    int cid = g_sls_main_connection();
    CFNumberRef n = CFNumberCreate(NULL, kCFNumberSInt32Type, &wid);
    if (!n) return 2;
    const void *vals[1] = { n };
    CFArrayRef arr = CFArrayCreate(NULL, vals, 1, &kCFTypeArrayCallBacks);
    CFRelease(n);
    if (!arr) return 2;
    CFTypeRef query = g_sls_window_query(cid, arr, 1);
    CFRelease(arr);
    if (!query) return 2;
    CFTypeRef iter = g_sls_window_query_copy(query);
    CFRelease(query);
    if (!iter) return 2;
    int result = 2;
    if (g_sls_window_iter_count(iter) == 1 && g_sls_window_iter_advance(iter)) {
        int level = g_sls_window_iter_level(iter);
        uint32_t parent = g_sls_window_iter_parent(iter);
        if (parent != 0) result = 0;
        else if (level == 0 || level == 3 || level == 8) result = 1;
        else result = 0;
    }
    CFRelease(iter);
    return result;
}

int rovr_bridge_sls_managed_for_window(uint32_t wid) {
    return rovr_sls_managed_for_window(wid);
}

// Sort displays by center coordinate (x then y) — deterministic arrangement order, yabai display_manager_coordinate_comparator (MIT).
static int rovr_display_cmp(const void *a, const void *b) {
    CGDirectDisplayID da = *(const CGDirectDisplayID *)a;
    CGDirectDisplayID db = *(const CGDirectDisplayID *)b;
    CGRect fa = CGDisplayBounds(da);
    CGRect fb = CGDisplayBounds(db);
    CGPoint ca = CGPointMake(fa.origin.x + fa.size.width * 0.5, fa.origin.y + fa.size.height * 0.5);
    CGPoint cb = CGPointMake(fb.origin.x + fb.size.width * 0.5, fb.origin.y + fb.size.height * 0.5);
    if (ca.x < cb.x) return -1;
    if (ca.x > cb.x) return 1;
    if (ca.y < cb.y) return -1;
    if (ca.y > cb.y) return 1;
    return 0;
}

int rovr_bridge_enumerate_displays(rovr_display_callback callback, void *context) {
    if (!callback) return 1;

    CGDirectDisplayID displays[32] = {0};
    uint32_t count = 0;
    if (CGGetActiveDisplayList(32, displays, &count) != kCGErrorSuccess) return 2;
    if (count > 1) qsort(displays, count, sizeof(CGDirectDisplayID), rovr_display_cmp);

    CGDirectDisplayID active_display = rovr_active_display_id();
    if (active_display == 0) active_display = CGMainDisplayID();
    CGDirectDisplayID main_display = CGMainDisplayID();
    for (uint32_t i = 0; i < count; i++) {
        CGRect frame = rovr_display_constrained_bounds(displays[i]);
        rovr_bridge_display display = {
            .id = displays[i],
            .focused = displays[i] == active_display ? 1 : 0,
            .is_main = displays[i] == main_display ? 1 : 0,
            .x = frame.origin.x,
            .y = frame.origin.y,
            .width = frame.size.width,
            .height = frame.size.height,
        };
        callback(&display, context);
    }
    return 0;
}

int rovr_bridge_set_window_frame(
    uint32_t window_id,
    double x,
    double y,
    double width,
    double height) {
    AXUIElementRef window = rovr_ax_window_for_id(window_id, NULL);
    if (!window) return 1;

    pid_t window_pid = 0;
    AXUIElementGetPid(window, &window_pid);
    __block AXUIElementRef app = window_pid > 0 ? AXUIElementCreateApplication(window_pid) : NULL;
    rovr_ax_apply_timeout(app);
    bool eui_toggled = false;
    if (app && rovr_ax_get_enhanced_ui(app)) {
        rovr_ax_set_enhanced_ui(app, false);
        eui_toggled = true;
    }

    CGPoint position = CGPointMake(x, y);
    CGSize size = CGSizeMake(width, height);
    AXValueRef position_value = AXValueCreate(kAXValueCGPointType, &position);
    AXValueRef size_value = AXValueCreate(kAXValueCGSizeType, &size);
    if (!position_value || !size_value) {
        if (position_value) CFRelease(position_value);
        if (size_value) CFRelease(size_value);
        CFRelease(window);
        return 2;
    }

    AXError position_error = AXUIElementSetAttributeValue(window, kAXPositionAttribute, position_value);
    AXError size_error = AXUIElementSetAttributeValue(window, kAXSizeAttribute, size_value);

    CFRelease(position_value);
    CFRelease(size_value);
    CFRelease(window);
    if (eui_toggled) {
        rovr_ax_set_enhanced_ui(app, true);
    }
    if (app) CFRelease(app);
    return position_error == kAXErrorSuccess && size_error == kAXErrorSuccess ? 0 : 3;
}

int rovr_bridge_focus_window(uint32_t window_id) {
    pid_t pid = 0;
    AXUIElementRef window = rovr_ax_window_for_id(window_id, &pid);
    if (!window) return 1;

    NSRunningApplication *application = [NSRunningApplication runningApplicationWithProcessIdentifier:pid];
    [application activateWithOptions:NSApplicationActivateIgnoringOtherApps];
    AXError error = AXUIElementSetAttributeValue(window, kAXFocusedAttribute, kCFBooleanTrue);
    CFRelease(window);
    return error == kAXErrorSuccess ? 0 : 2;
}

int rovr_bridge_set_window_minimized(uint32_t window_id, int minimized) {
    AXUIElementRef window = rovr_ax_window_for_id(window_id, NULL);
    if (!window) return 1;
    CFBooleanRef value = minimized ? kCFBooleanTrue : kCFBooleanFalse;
    AXError err = AXUIElementSetAttributeValue(window, kAXMinimizedAttribute, value);
    CFRelease(window);
    return err == kAXErrorSuccess ? 0 : 2;
}

// Press a named button child (AXCloseButton / AXFullScreenButton) of a window.
// Shared by close and native-fullscreen toggle. Returns 0 on success.
static int rovr_ax_press_window_button(uint32_t window_id, CFStringRef button_attribute) {
    AXUIElementRef window = rovr_ax_window_for_id(window_id, NULL);
    if (!window) return 1;
    int result = 2;
    AXUIElementRef button = NULL;
    if (AXUIElementCopyAttributeValue(window, button_attribute, (CFTypeRef *)&button) == kAXErrorSuccess && button) {
        rovr_ax_apply_timeout(button);
        if (AXUIElementPerformAction(button, kAXPressAction) == kAXErrorSuccess) {
            result = 0;
        }
    }
    if (button) CFRelease(button);
    CFRelease(window);
    return result;
}

int rovr_bridge_close_window(uint32_t window_id) {
    return rovr_ax_press_window_button(window_id, kAXCloseButtonAttribute);
}

int rovr_bridge_toggle_fullscreen(uint32_t window_id) {
    // AX-only mutation. Focus and transition settling are handled outside the 150ms AX worker
    // (see mod.rs ToggleNativeFullscreen). Keeps the worker deadline tight.
    pid_t pid = 0;
    AXUIElementRef window = rovr_ax_window_for_id(window_id, &pid);
    if (!window) return 1;
    bool is_fullscreen = false;
    CFTypeRef fv = NULL;
    if (AXUIElementCopyAttributeValue(window, CFSTR("AXFullScreen"), &fv) == kAXErrorSuccess && fv) {
        if (CFGetTypeID(fv) == CFBooleanGetTypeID()) is_fullscreen = CFBooleanGetValue((CFBooleanRef)fv);
        CFRelease(fv);
    }
    AXUIElementRef appElem = pid > 0 ? AXUIElementCreateApplication(pid) : NULL;
    if (appElem) rovr_ax_apply_timeout(appElem);
    bool eui = appElem ? rovr_ax_get_enhanced_ui(appElem) : false;
    if (eui) rovr_ax_set_enhanced_ui(appElem, false);
    AXError setErr = AXUIElementSetAttributeValue(window, CFSTR("AXFullScreen"), is_fullscreen ? kCFBooleanFalse : kCFBooleanTrue);
    if (eui) rovr_ax_set_enhanced_ui(appElem, true);
    if (appElem) CFRelease(appElem);
    int result = 0;
    if (setErr != kAXErrorSuccess) {
        AXUIElementRef button = NULL;
        if (AXUIElementCopyAttributeValue(window, kAXFullScreenButtonAttribute, (CFTypeRef *)&button) == kAXErrorSuccess && button) {
            rovr_ax_apply_timeout(button);
            result = (AXUIElementPerformAction(button, kAXPressAction) == kAXErrorSuccess) ? 0 : 3;
            CFRelease(button);
        } else result = 2;
    }
    CFRelease(window);
    return result;
}

int rovr_bridge_is_display_animating(uint32_t display_id) {
    return rovr_display_is_animating(display_id) ? 1 : 0;
}

int32_t rovr_bridge_dock_pid(void) {
    @autoreleasepool {
        NSArray<NSRunningApplication *> *apps = [NSRunningApplication runningApplicationsWithBundleIdentifier:@"com.apple.dock"];
        if (apps.count == 0) return -1;
        NSRunningApplication *dock = apps[0];
        return (int32_t)dock.processIdentifier;
    }
}

int rovr_bridge_needs_refresh(void) {
    return atomic_exchange(&g_needs_refresh, 0);
}

int rovr_bridge_enumerate_spaces(rovr_space_callback callback, void *context) {
    if (!callback) return 1;
    if (!g_sls_copy_managed_display_spaces || !g_sls_main_connection) return 1;

    int cid = g_sls_main_connection();
    CFArrayRef display_spaces = g_sls_copy_managed_display_spaces(cid);
    if (!display_spaces) return 2;

    uint32_t position = 0;

    CFIndex display_count = CFArrayGetCount(display_spaces);
    for (CFIndex d = 0; d < display_count; d++) {
        CFDictionaryRef display_ref = (CFDictionaryRef)CFArrayGetValueAtIndex(display_spaces, d);
        CFStringRef uuid = (CFStringRef)CFDictionaryGetValue(display_ref, CFSTR("Display Identifier"));
        CFArrayRef spaces_ref = (CFArrayRef)CFDictionaryGetValue(display_ref, CFSTR("Spaces"));
        if (!uuid || !spaces_ref) continue;

        uint32_t display_id = 0;
        CFUUIDRef parsed = CFUUIDCreateFromString(NULL, uuid);
        if (parsed) {
            display_id = CGDisplayGetDisplayIDFromUUID(parsed);
            CFRelease(parsed);
        }
        if (display_id == 0) continue;

        uint64_t active_sid = 0;
        if (g_sls_managed_display_get_current_space) {
            active_sid = g_sls_managed_display_get_current_space(cid, uuid);
        }

        CFIndex space_count = CFArrayGetCount(spaces_ref);
        for (CFIndex s = 0; s < space_count; s++) {
            CFDictionaryRef space_ref = (CFDictionaryRef)CFArrayGetValueAtIndex(spaces_ref, s);
            CFNumberRef sid_ref = (CFNumberRef)CFDictionaryGetValue(space_ref, CFSTR("id64"));
            if (!sid_ref) continue;
            uint64_t sid = 0;
            CFNumberGetValue(sid_ref, kCFNumberSInt64Type, &sid);
            int stype = g_sls_space_get_type ? g_sls_space_get_type(cid, sid) : -1;
            rovr_bridge_space space = {
                .id = sid,
                .display_id = display_id,
                .type = stype,
                .focused = sid == active_sid ? 1 : 0,
                .position = position,
                .is_system = stype == 2 ? 1 : 0,
            };
            callback(&space, context);
            position++;
        }
    }

    CFRelease(display_spaces);
    return 0;
}

// macOS 12.7+/13.6+/14.5+/15+ silently ignore SLSMoveWindowsToManagedSpace.
// Yabai gates on the same version ranges; on those systems the SA-free path
// is the compat-workspace workaround. Direct calls only work on older builds.
static bool rovr_use_macos_space_workaround(void) {
    NSOperatingSystemVersion v = [[NSProcessInfo processInfo] operatingSystemVersion];
    if (v.majorVersion == 12 && v.minorVersion >= 7) return true;
    if (v.majorVersion == 13 && v.minorVersion >= 6) return true;
    if (v.majorVersion == 14 && v.minorVersion >= 5) return true;
    return v.majorVersion >= 15;
}

int rovr_bridge_move_window_to_space(uint32_t window_id, uint64_t space_id) {
    if (!g_sls_main_connection) return 1;
    int cid = g_sls_main_connection();

    CFNumberRef wid_ref = CFNumberCreate(NULL, kCFNumberSInt32Type, &window_id);
    if (!wid_ref) return 2;
    const void *values[1] = { wid_ref };
    CFArrayRef window_list = CFArrayCreate(NULL, values, 1, &kCFTypeArrayCallBacks);
    CFRelease(wid_ref);
    if (!window_list) return 2;

    // Yabai's primary modern path: the asynchronous bridged window-management
    // operation. Works on current macOS without the scripting addition.
    if (g_sls_perform_async_bridged_op) {
        Class cls = objc_getClass("SLSBridgedMoveWindowsToManagedSpaceOperation");
        if (cls) {
            SEL sel = sel_registerName("initWithWindows:spaceID:");
            id operation = ((id (*)(id, SEL, id, uint64_t))objc_msgSend)(
                [cls alloc], sel, (__bridge id)window_list, space_id);
            g_sls_perform_async_bridged_op((__bridge void *)operation);
            CFRelease(window_list);
            return 0;
        }
    }

    // Older macOS: the direct call works before the workaround gate.
    if (!rovr_use_macos_space_workaround() && g_sls_move_windows_to_managed_space) {
        g_sls_move_windows_to_managed_space(cid, window_list, space_id);
        CFRelease(window_list);
        return 0;
    }
    CFRelease(window_list);

    // Compat-workspace fallback when neither of the above is available.
    if (g_sls_space_set_compat_id && g_sls_set_window_list_workspace) {
        int workspace = 0x726f7672; // "rovr"
        g_sls_space_set_compat_id(cid, space_id, workspace);
        g_sls_set_window_list_workspace(cid, &window_id, 1, workspace);
        g_sls_space_set_compat_id(cid, space_id, 0);
        return 0;
    }

    return 1;
}

// Space focus via high-velocity dock-swipe gesture synthesis. Adapted from
// yabai's space_manager_focus_space_using_gesture (MIT, copyright Asmund
// Vikane).
// :Attribution
// https://github.com/jurplel/InstantSpaceSwitcher
// https://github.com/thenickdude/wacom-driver-fix/blob/bdfda9a788934c88d09d31ea6a42664b9ba1471e/Readme.md
// Technique first observed in practice, and reverse-engineered from,
// BetterTouchTool.

int rovr_bridge_focus_space(uint64_t space_id) {
    if (!g_sls_copy_managed_display_for_space || !g_sls_managed_display_get_current_space ||
        !g_sls_copy_managed_display_spaces || !g_sls_main_connection) {
        return 1;
    }
    // Guard: posting gesture mid-animation leaves WindowServer between Spaces (blank-screen bug).
    // Adapted from yabai display_manager_display_is_animating check (MIT).
    {
        uint32_t did = rovr_display_for_space(space_id);
        if (did != 0 && rovr_display_is_animating(did)) return 5;
    }

    int cid = g_sls_main_connection();
    CFStringRef target_uuid = g_sls_copy_managed_display_for_space(cid, space_id);
    if (!target_uuid) return 2;
    uint64_t current_sid = g_sls_managed_display_get_current_space(cid, target_uuid);
    if (current_sid == space_id) {
        CFRelease(target_uuid);
        return 0;
    }

    int current_index = rovr_mission_control_index(cid, current_sid);
    int target_index = rovr_mission_control_index(cid, space_id);
    if (current_index == 0 || target_index == 0) {
        CFRelease(target_uuid);
        return 3;
    }

    // Cross-display handling (mirrors yabai's space_manager_focus_space_using_
    // gesture): dock swipes are delivered to the display under the CURSOR, so
    // when the target Space lives on another display, warp the cursor to that
    // display's center first and activate it afterwards — otherwise the swipe
    // navigates the wrong display's stack and nothing happens.
    CFUUIDRef target_uuid_parsed = CFUUIDCreateFromString(NULL, target_uuid);
    uint32_t new_did =
        target_uuid_parsed ? CGDisplayGetDisplayIDFromUUID(target_uuid_parsed) : 0;
    if (target_uuid_parsed) CFRelease(target_uuid_parsed);

    BOOL warp_cursor = NO;
    if (new_did != 0) {
        CGPoint cursor = CGPointZero;
        CGEventRef cursor_event = CGEventCreate(NULL);
        if (cursor_event) {
            cursor = CGEventGetLocation(cursor_event);
            CFRelease(cursor_event);
        }
        CGRect target_bounds = CGDisplayBounds(new_did);
        BOOL cursor_on_target =
            cursor.x >= CGRectGetMinX(target_bounds) && cursor.x < CGRectGetMaxX(target_bounds) &&
            cursor.y >= CGRectGetMinY(target_bounds) && cursor.y < CGRectGetMaxY(target_bounds);
        if (!cursor_on_target) {
            warp_cursor = YES;
            CGWarpMouseCursorPosition(
                CGPointMake(CGRectGetMidX(target_bounds), CGRectGetMidY(target_bounds)));
            // Activate the target display BEFORE posting swipes so its
            // animation starts while it is already key — activating after
            // the animation made every cross-display switch feel a beat
            // slower and let keyboard focus lag behind on the external
            // display (yabai: display_manager_set_active_display_id).
            if (g_sls_set_active_menu_bar_display_identifier) {
                g_sls_set_active_menu_bar_display_identifier(cid, target_uuid, target_uuid);
            }
        }
    }

    int delta = target_index - current_index;
    float sign = delta > 0 ? 1.0f : -1.0f;
    CGEventRef event = CGEventCreate(NULL);
    if (!event) {
        CFRelease(target_uuid);
        return 4;
    }
    CGEventSetIntegerValueField(event, 55, 30);     // kCGSEventDockControl
    CGEventSetIntegerValueField(event, 110, 23);    // kIOHIDEventTypeDockSwipe
    CGEventSetIntegerValueField(event, 123, 1);     // kCGGestureMotionHorizontal
    CGEventSetDoubleValueField(event, 124, sign);   // swipe progress
    CGEventSetDoubleValueField(event, 129, sign * 9999.0);
    int steps = abs(delta);
    for (int i = 0; i < steps; i++) {
        CGEventSetIntegerValueField(event, 132, 1); // phase began
        CGEventPost(kCGSessionEventTap, event);
        CGEventSetIntegerValueField(event, 132, 4); // phase ended
        CGEventPost(kCGSessionEventTap, event);
    }
    CFRelease(event);

    CFRelease(target_uuid);
    return 0;
}

int rovr_bridge_focus_space_step(uint64_t target_space_id, int32_t delta) {
    if (!g_sls_copy_managed_display_for_space || !g_sls_main_connection ||
        target_space_id == 0 || delta == 0) {
        return 1;
    }
    {
        uint32_t did = rovr_display_for_space(target_space_id);
        if (did != 0 && rovr_display_is_animating(did)) return 5;
    }

    int cid = g_sls_main_connection();
    CFStringRef target_uuid =
        g_sls_copy_managed_display_for_space(cid, target_space_id);
    if (!target_uuid) return 2;

    CFUUIDRef target_uuid_parsed = CFUUIDCreateFromString(NULL, target_uuid);
    uint32_t target_did =
        target_uuid_parsed ? CGDisplayGetDisplayIDFromUUID(target_uuid_parsed) : 0;
    if (target_uuid_parsed) CFRelease(target_uuid_parsed);

    if (target_did != 0) {
        CGPoint cursor = CGPointZero;
        CGEventRef cursor_event = CGEventCreate(NULL);
        if (cursor_event) {
            cursor = CGEventGetLocation(cursor_event);
            CFRelease(cursor_event);
        }
        CGRect target_bounds = CGDisplayBounds(target_did);
        BOOL cursor_on_target =
            cursor.x >= CGRectGetMinX(target_bounds) && cursor.x < CGRectGetMaxX(target_bounds) &&
            cursor.y >= CGRectGetMinY(target_bounds) && cursor.y < CGRectGetMaxY(target_bounds);
        if (!cursor_on_target) {
            CGWarpMouseCursorPosition(
                CGPointMake(CGRectGetMidX(target_bounds), CGRectGetMidY(target_bounds)));
            if (g_sls_set_active_menu_bar_display_identifier) {
                g_sls_set_active_menu_bar_display_identifier(cid, target_uuid, target_uuid);
            }
        }
    }

    float sign = delta > 0 ? 1.0f : -1.0f;
    CGEventRef event = CGEventCreate(NULL);
    if (!event) {
        CFRelease(target_uuid);
        return 3;
    }
    CGEventSetIntegerValueField(event, 55, 30);     // kCGSEventDockControl
    CGEventSetIntegerValueField(event, 110, 23);    // kIOHIDEventTypeDockSwipe
    CGEventSetIntegerValueField(event, 123, 1);     // kCGGestureMotionHorizontal
    CGEventSetDoubleValueField(event, 124, sign);   // swipe progress
    CGEventSetDoubleValueField(event, 129, sign * 9999.0);
    int64_t steps = delta < 0 ? -(int64_t)delta : (int64_t)delta;
    for (int64_t i = 0; i < steps; i++) {
        CGEventSetIntegerValueField(event, 132, 1); // phase began
        CGEventPost(kCGSessionEventTap, event);
        CGEventSetIntegerValueField(event, 132, 4); // phase ended
        CGEventPost(kCGSessionEventTap, event);
    }
    CFRelease(event);
    CFRelease(target_uuid);
    return 0;
}

uint64_t rovr_bridge_current_space_for_space(uint64_t space_id) {
    if (!g_sls_copy_managed_display_for_space || !g_sls_managed_display_get_current_space ||
        !g_sls_main_connection || space_id == 0) {
        return 0;
    }
    int cid = g_sls_main_connection();
    CFStringRef uuid = g_sls_copy_managed_display_for_space(cid, space_id);
    if (!uuid) return 0;
    uint64_t result = g_sls_managed_display_get_current_space(cid, uuid);
    CFRelease(uuid);
    return result;
}

// The display a Space lives on. Used by the focus path to detect
// cross-display switches (their settle gates are independent).
uint32_t rovr_bridge_display_for_space(uint64_t space_id) {
    return rovr_display_for_space(space_id);
}

int rovr_bridge_space_is_fullscreen(uint64_t space_id) {
    if (!g_sls_space_get_type || !g_sls_main_connection || space_id == 0) return 0;
    return g_sls_space_get_type(g_sls_main_connection(), space_id) == 4 ? 1 : 0;
}

int rovr_bridge_space_is_system(uint64_t space_id) {
    if (!g_sls_space_get_type || !g_sls_main_connection || space_id == 0) return 0;
    return g_sls_space_get_type(g_sls_main_connection(), space_id) == 2 ? 1 : 0;
}
