#import "bridge.h"

#import <AppKit/AppKit.h>
#import <ApplicationServices/ApplicationServices.h>
#import <dlfcn.h>
#import <stdatomic.h>

// _AXUIElementGetWindow is private. Resolve it dynamically so the bridge still
// links if the symbol disappears. The private capability layer will eventually
// isolate this further behind explicit capability probing.
typedef AXError (*rovr_ax_get_window_fn)(AXUIElementRef element, CGWindowID *window_id);
static rovr_ax_get_window_fn g_ax_get_window = NULL;

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

static uint32_t rovr_display_for_rect(CGRect rect) {
    CGDirectDisplayID displays[8] = {0};
    uint32_t count = 0;
    if (CGGetDisplaysWithRect(rect, 8, displays, &count) != kCGErrorSuccess || count == 0) {
        return 0;
    }
    return displays[0];
}

static uint32_t rovr_focused_window_id(void) {
    if (!g_ax_get_window) return 0;
    NSRunningApplication *frontmost = NSWorkspace.sharedWorkspace.frontmostApplication;
    if (!frontmost) return 0;

    AXUIElementRef app = AXUIElementCreateApplication(frontmost.processIdentifier);
    if (!app) return 0;

    CFTypeRef focused = NULL;
    AXError error = AXUIElementCopyAttributeValue(app, kAXFocusedWindowAttribute, &focused);
    CFRelease(app);
    if (error != kAXErrorSuccess || !focused) return 0;

    CGWindowID window_id = 0;
    g_ax_get_window((AXUIElementRef)focused, &window_id);
    CFRelease(focused);
    return window_id;
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

    CFTypeRef value = NULL;
    AXError error = AXUIElementCopyAttributeValue(app, kAXWindowsAttribute, &value);
    CFRelease(app);
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
            break;
        }
    }
    CFRelease(windows);
    return result;
}

int rovr_bridge_init(void) {
    g_ax_get_window = (rovr_ax_get_window_fn)dlsym(RTLD_DEFAULT, "_AXUIElementGetWindow");
    CGDisplayRegisterReconfigurationCallback(rovr_display_reconfiguration_callback, NULL);
    return 0;
}

uint64_t rovr_bridge_capabilities(void) {
    uint64_t capabilities = ROVR_CAP_OBSERVE_WINDOWS;
    if (g_ax_get_window) {
        capabilities |= ROVR_CAP_SET_WINDOW_FRAME;
        capabilities |= ROVR_CAP_FOCUS_WINDOW;
    }
    return capabilities;
}

int rovr_bridge_enumerate_windows(rovr_window_callback callback, void *context) {
    if (!callback) return 1;

    @autoreleasepool {
        const uint32_t focused_window_id = rovr_focused_window_id();
        CFArrayRef list = CGWindowListCopyWindowInfo(
            kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
            kCGNullWindowID);
        if (!list) return 2;

        CFIndex count = CFArrayGetCount(list);
        for (CFIndex i = 0; i < count; i++) {
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
            window.display_id = rovr_display_for_rect(bounds);
            window.focused = focused_window_id == window.id ? 1 : 0;
            window.x = bounds.origin.x;
            window.y = bounds.origin.y;
            window.width = bounds.size.width;
            window.height = bounds.size.height;

            rovr_copy_cf_string(CFDictionaryGetValue(entry, kCGWindowOwnerName), window.app, sizeof(window.app));
            rovr_copy_cf_string(CFDictionaryGetValue(entry, kCGWindowName), window.title, sizeof(window.title));

            NSRunningApplication *application =
                [NSRunningApplication runningApplicationWithProcessIdentifier:owner_pid];
            if (application.bundleIdentifier) {
                [application.bundleIdentifier getCString:window.bundle_id
                                               maxLength:sizeof(window.bundle_id)
                                                encoding:NSUTF8StringEncoding];
            }

            callback(&window, context);
        }

        CFRelease(list);
        return 0;
    }
}

int rovr_bridge_enumerate_displays(rovr_display_callback callback, void *context) {
    if (!callback) return 1;

    CGDirectDisplayID displays[32] = {0};
    uint32_t count = 0;
    if (CGGetActiveDisplayList(32, displays, &count) != kCGErrorSuccess) return 2;

    const CGDirectDisplayID main_display = CGMainDisplayID();
    for (uint32_t i = 0; i < count; i++) {
        CGRect frame = CGDisplayBounds(displays[i]);
        rovr_bridge_display display = {
            .id = displays[i],
            .focused = displays[i] == main_display ? 1 : 0,
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

int rovr_bridge_needs_refresh(void) {
    return atomic_exchange(&g_needs_refresh, 0);
}
