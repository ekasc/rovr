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

// _AXUIElementGetWindow is private. Resolve it dynamically so the bridge still
// links if the symbol disappears. The private capability layer will eventually
// isolate this further behind explicit capability probing.
typedef AXError (*rovr_ax_get_window_fn)(AXUIElementRef element, CGWindowID *window_id);
static rovr_ax_get_window_fn g_ax_get_window = NULL;

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
            window.space_id = rovr_space_id_for_window(window.id);
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

int rovr_bridge_enumerate_spaces(rovr_space_callback callback, void *context) {
    if (!callback) return 1;
    if (!g_sls_copy_managed_display_spaces || !g_sls_main_connection) return 1;

    int cid = g_sls_main_connection();
    CFArrayRef display_spaces = g_sls_copy_managed_display_spaces(cid);
    if (!display_spaces) return 2;

    CGDirectDisplayID active_displays[32] = {0};
    uint32_t active_display_count = 0;
    CGGetActiveDisplayList(32, active_displays, &active_display_count);

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
        bool display_focused = false;
        for (uint32_t i = 0; i < active_display_count; i++) {
            if (active_displays[i] == display_id) {
                display_focused = true;
                break;
            }
        }

        uint64_t active_sid = 0;
        if (display_focused && g_sls_managed_display_get_current_space) {
            active_sid = g_sls_managed_display_get_current_space(cid, uuid);
        }

        CFIndex space_count = CFArrayGetCount(spaces_ref);
        for (CFIndex s = 0; s < space_count; s++) {
            CFDictionaryRef space_ref = (CFDictionaryRef)CFArrayGetValueAtIndex(spaces_ref, s);
            CFNumberRef sid_ref = (CFNumberRef)CFDictionaryGetValue(space_ref, CFSTR("id64"));
            if (!sid_ref) continue;
            uint64_t sid = 0;
            CFNumberGetValue(sid_ref, kCFNumberSInt64Type, &sid);
            rovr_bridge_space space = {
                .id = sid,
                .display_id = display_id,
                .type = g_sls_space_get_type ? g_sls_space_get_type(cid, sid) : -1,
                .focused = sid == active_sid ? 1 : 0,
            };
            callback(&space, context);
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

    int cid = g_sls_main_connection();
    CFStringRef target_uuid = g_sls_copy_managed_display_for_space(cid, space_id);
    if (!target_uuid) return 2;
    uint64_t current_sid = g_sls_managed_display_get_current_space(cid, target_uuid);
    CFRelease(target_uuid);
    if (current_sid == space_id) return 0;

    int current_index = rovr_mission_control_index(cid, current_sid);
    int target_index = rovr_mission_control_index(cid, space_id);
    if (current_index == 0 || target_index == 0) return 3;

    int delta = target_index - current_index;
    float sign = delta > 0 ? 1.0f : -1.0f;
    CGEventRef event = CGEventCreate(NULL);
    if (!event) return 4;
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
    return 0;
}
