//
// Rovr scripting-addition payload — injected into Dock.app.
//
// Listens on the ROVR-OWNED socket /tmp/rovr-<uid>/sa.sock and executes
// primitive SkyLight / Dock-internal operations ONLY. This payload contains
// no layout policy, no config parsing, no workspace logic, and never talks to
// yabai's socket.
//
// :Attribution
//
// The Dock-internals resolution (pattern scanning, function-pointer calling
// conventions) and the primitive operation implementations are adapted from
// yabai's src/osax/payload.m and src/osax/{arm64,x64}_payload.m
// (MIT © 2019 Åsmund Vikane, https://github.com/asmvik/yabai).
// Upstream MIT notice preserved in vendor/ and docs/YABAI_PORT.md.
//

#import <Foundation/Foundation.h>

#include <mach-o/getsect.h>
#include <mach-o/dyld.h>
#include <mach/mach.h>
#include <mach/mach_vm.h>
#include <mach/vm_page_size.h>
#include <objc/message.h>
#include <objc/runtime.h>

#include <CoreGraphics/CoreGraphics.h>
#include <sys/types.h>
#include <sys/stat.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>
#include <dlfcn.h>
#include <pthread.h>
#include <stdlib.h>
#include <string.h>
#include <stdio.h>

#include "common.h"

#ifdef __x86_64__
#include "../vendor/x64_payload.m"
#elif __arm64__
#include "../vendor/arm64_payload.m"
#include <ptrauth.h>
#endif

#define unpack(v) memcpy(&v, message, sizeof(v)); message += sizeof(v)

#define SA_SOCKET_BUFF_LEN 0x1000

extern int SLSMainConnectionID(void);
extern CGError SLSGetConnectionPSN(int cid, ProcessSerialNumber *psn);
extern CGError SLSGetWindowAlpha(int cid, uint32_t wid, float *alpha);
extern CGError SLSSetWindowAlpha(int cid, uint32_t wid, float alpha);
extern CGError SLSSetWindowTags(int cid, uint32_t wid, uint64_t *tags, size_t tag_size);
extern CGError SLSClearWindowTags(int cid, uint32_t wid, uint64_t *tags, size_t tag_size);
extern CGError SLSGetWindowBounds(int cid, uint32_t wid, CGRect *frame);
extern CGError SLSGetWindowTransform(int cid, uint32_t wid, CGAffineTransform *t);
extern CGError SLSSetWindowTransform(int cid, uint32_t wid, CGAffineTransform t);
extern void SLSManagedDisplaySetCurrentSpace(int cid, CFStringRef display_ref, uint64_t sid);
extern uint64_t SLSManagedDisplayGetCurrentSpace(int cid, CFStringRef display_ref);
extern CFStringRef SLSCopyManagedDisplayForSpace(int cid, uint64_t sid);
extern void SLSShowSpaces(int cid, CFArrayRef space_list);
extern void SLSHideSpaces(int cid, CFArrayRef space_list);
extern CGError SLSSetWindowSubLevel(int cid, uint32_t wid, int level);

//
// Single-slot opacity-fade context. A repair-pass simplification versus
// upstream's per-window table: concurrent fades to DIFFERENT windows degrade
// to a direct alpha set instead of piling up threads.
//
static pthread_mutex_t fade_lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_t fade_thread;
static volatile uint32_t fade_wid = 0;
static volatile float fade_alpha = 0.0f;
static volatile float fade_duration = 0.0f;
static volatile bool fade_skip = false;

static id dock_spaces;
static id dp_desktop_picture_manager;
static uint64_t add_space_fp;
static uint64_t remove_space_fp;
static uint64_t move_space_fp;
static bool macOSSequoia;

static pthread_t daemon_thread;
static int daemon_sockfd;

// ---- Dock-internals resolution machinery (adapted from upstream payload.m) ----

typedef void (*remove_space_call)(id space, id display_space, id dock_spaces, uint64_t space_id1, uint64_t space_id2);

static uint64_t static_base_address(void)
{
    const struct segment_command_64 *command = getsegbyname("__TEXT");
    uint64_t addr = command->vmaddr;
    return addr;
}

static uint64_t image_slide(void)
{
    char path[1024];
    uint32_t size = sizeof(path);

    if (_NSGetExecutablePath(path, &size) != 0) {
        return -1;
    }

    for (uint32_t i = 0; i < _dyld_image_count(); i++) {
        if (strcmp(_dyld_get_image_name(i), path) == 0) {
            return _dyld_get_image_vmaddr_slide(i);
        }
    }

    return 0;
}

// Scan bounded by the host's __TEXT size: in Dock the region is large, but
// if the payload is ever loaded into another host the scan must not walk off
// the end of mapped memory.
static uint64_t g_scan_base = 0;
static uint64_t g_scan_limit = 0;

static uint64_t hex_find_seq(uint64_t baddr, const char *c_pattern)
{
    if (!baddr || !c_pattern || baddr == UINT64_MAX || !g_scan_base || !g_scan_limit) return 0;
    uint64_t scan_end = 0;
    if (__builtin_add_overflow(g_scan_base, g_scan_limit, &scan_end) ||
        baddr < g_scan_base || baddr >= scan_end) return 0;

    uint64_t addr = baddr;
    uint64_t pattern_length = (strlen(c_pattern) + 1) / 3;
    if (pattern_length == 0 || pattern_length > scan_end - baddr) return 0;
    char buffer_a[pattern_length];
    char buffer_b[pattern_length];
    memset(buffer_a, 0, sizeof(buffer_a));
    memset(buffer_b, 0, sizeof(buffer_b));

    char *pattern = (char *) c_pattern + 1;
    for (int i = 0; i < pattern_length; ++i) {
        char c = pattern[-1];
        if (c == '?') {
            buffer_b[i] = 1;
        } else {
            int temp = c <= '9' ? 0 : 9;
            temp = (temp + c) << 0x4;
            c = pattern[0];
            int temp2 = c <= '9' ? 0xd0 : 0xc9;
            buffer_a[i] = temp2 + c + temp;
        }
        pattern += 3;
    }

    uint64_t available = scan_end - baddr;
    uint64_t scan_length = available < 0x1286a0 ? available : 0x1286a0;
    if (scan_length < pattern_length) return 0;

loop:
    for (uint64_t counter = 0; counter < pattern_length; ++counter) {
        if (counter >= scan_end - addr) return 0;
        if ((buffer_b[counter] == 0) && (((char *)addr)[counter] != buffer_a[counter])) {
            if (addr - baddr >= scan_length - pattern_length) return 0;
            addr++;
            goto loop;
        }
    }

    return addr;
}

#if __arm64__
uint64_t decode_adrp_add(uint64_t addr, uint64_t offset)
{
    uint32_t adrp_instr = *(uint32_t *) addr;

    uint32_t immlo = (0x60000000 & adrp_instr) >> 29;
    uint32_t immhi = (0xffffe0 & adrp_instr) >> 3;

    int32_t value = (immhi | immlo) << 12;
    int64_t value_64 = value;

    uint32_t add_instr = *(uint32_t *) (addr + 4);
    uint64_t imm12 = (add_instr & 0x3ffc00) >> 10;

    if (add_instr & 0xc00000) {
        imm12 <<= 12;
    }

    return (offset & 0xfffffffffffff000) + value_64 + imm12;
}
#endif

static bool verify_os_version(NSOperatingSystemVersion os_version)
{
    NSLog(@"[rovr-sa] checking for macOS %ld.%ld.%ld compatibility!", os_version.majorVersion, os_version.minorVersion, os_version.patchVersion);

#ifdef __x86_64__
    if (os_version.majorVersion == 11 || os_version.majorVersion == 12 ||
        os_version.majorVersion == 13 || os_version.majorVersion == 14) {
        return true;
    } else if (os_version.majorVersion == 15 || os_version.majorVersion == 26) {
        macOSSequoia = true;
        return true;
    }
#elif __arm64__
    if (os_version.majorVersion == 12 || os_version.majorVersion == 13 ||
        os_version.majorVersion == 14) {
        return true;
    } else if (os_version.majorVersion == 15 || os_version.majorVersion == 26) {
        macOSSequoia = true;
        return true;
    }
#endif

    NSLog(@"[rovr-sa] spaces functionality is only supported on macOS Monterey 12+, Ventura 13+, Sonoma 14+, Sequoia 15+ and Tahoe 26+");
    return false;
}

// Size of the mapped VM region containing addr (0 on failure). Unlike
// getsegbyname (unreliable from a dlopen'd image), this always reflects what
// is actually mapped.
static uint64_t region_size_at(uint64_t addr)
{
    mach_vm_address_t address = (mach_vm_address_t) addr;
    mach_vm_size_t size = 0;
    vm_region_basic_info_data_64_t info;
    mach_msg_type_number_t count = VM_REGION_BASIC_INFO_COUNT_64;
    mach_port_t object_name = MACH_PORT_NULL;
    if (mach_vm_region(mach_task_self(), &address, &size, VM_REGION_BASIC_INFO_64,
                       (vm_region_info_t) &info, &count, &object_name) != KERN_SUCCESS) {
        return 0;
    }
    return (uint64_t) size;
}

static uint64_t scan_address_for_offset(uint64_t offset)
{
    if (!g_scan_base || offset >= g_scan_limit) return UINT64_MAX;
    uint64_t address = 0;
    if (__builtin_add_overflow(g_scan_base, offset, &address)) return UINT64_MAX;
    return address;
}

static void init_instances()
{
    NSOperatingSystemVersion os_version = [[NSProcessInfo processInfo] operatingSystemVersion];
    if (!verify_os_version(os_version)) return;

    uint64_t baseaddr = static_base_address() + image_slide();

    {
        const struct segment_command_64 *text = getsegbyname("__TEXT");
        uint64_t seg_size = text ? (uint64_t) text->filesize : 0;
        // Belt and braces: clamp to whatever is actually mapped at baseaddr.
        uint64_t mapped = region_size_at(baseaddr);
        g_scan_base = baseaddr;
        g_scan_limit = mapped > 0 ? (seg_size ? (seg_size < mapped ? seg_size : mapped) : mapped) : 0;
    }

    uint64_t dock_spaces_addr = hex_find_seq(scan_address_for_offset(get_dock_spaces_offset(os_version)), get_dock_spaces_pattern(os_version));
    if (dock_spaces_addr == 0) {
        dock_spaces = nil;
        NSLog(@"[rovr-sa] could not locate pointer to dock.spaces! spaces functionality will not work!");
    } else {
#ifdef __x86_64__
        uint32_t dock_spaces_offset = *(int32_t *)dock_spaces_addr;
        dock_spaces = [(*(id *)(dock_spaces_addr + dock_spaces_offset + 0x4)) retain];
#elif __arm64__
        uint64_t dock_spaces_offset = decode_adrp_add(dock_spaces_addr, dock_spaces_addr - baseaddr);
        dock_spaces = [(*(id *)(baseaddr + dock_spaces_offset)) retain];
#endif
    }

    uint64_t dppm_addr = hex_find_seq(scan_address_for_offset(get_dppm_offset(os_version)), get_dppm_pattern(os_version));
    if (dppm_addr == 0) {
        dp_desktop_picture_manager = nil;
        NSLog(@"[rovr-sa] could not locate pointer to dppm! moving spaces will not work!");
    } else {
#ifdef __x86_64__
        uint32_t dppm_offset = *(int32_t *)dppm_addr;
        dp_desktop_picture_manager = [(*(id *)(dppm_addr + dppm_offset + 0x4)) retain];
#elif __arm64__
        uint64_t dppm_offset = decode_adrp_add(dppm_addr, dppm_addr - baseaddr);
        dp_desktop_picture_manager = [(*(id *)(baseaddr + dppm_offset)) retain];
#endif

        //
        // @hack (upstream): in Sonoma, DPDesktopPictureManager is initialized
        // and swapped to an alternate storage location 8 bytes earlier.
        //
#ifdef __x86_64__
        if (dp_desktop_picture_manager == nil) {
            dp_desktop_picture_manager = [(*(id *)(dppm_addr + dppm_offset + 0x4 - 0x8)) retain];
        }
#elif __arm64__
        if (dp_desktop_picture_manager == nil) {
            dp_desktop_picture_manager = [(*(id *)(baseaddr + dppm_offset - 0x8)) retain];
        }
#endif
    }

    uint64_t add_space_addr = hex_find_seq(scan_address_for_offset(get_add_space_offset(os_version)), get_add_space_pattern(os_version));
    if (add_space_addr == 0x0) {
        NSLog(@"[rovr-sa] failed to get pointer to addSpace function..");
        add_space_fp = 0;
    } else {
#ifdef __x86_64__
        add_space_fp = add_space_addr;
#elif __arm64__
        add_space_fp = (uint64_t) ptrauth_sign_unauthenticated((void *) add_space_addr, ptrauth_key_asia, 0);
#endif
    }

    uint64_t remove_space_addr = hex_find_seq(scan_address_for_offset(get_remove_space_offset(os_version)), get_remove_space_pattern(os_version));
    if (remove_space_addr == 0x0) {
        NSLog(@"[rovr-sa] failed to get pointer to removeSpace function..");
        remove_space_fp = 0;
    } else {
#ifdef __x86_64__
        remove_space_fp = remove_space_addr;
#elif __arm64__
        remove_space_fp = (uint64_t) ptrauth_sign_unauthenticated((void *) remove_space_addr, ptrauth_key_asia, 0);
#endif
    }

    uint64_t move_space_addr = hex_find_seq(scan_address_for_offset(get_move_space_offset(os_version)), get_move_space_pattern(os_version));
    if (move_space_addr == 0x0) {
        NSLog(@"[rovr-sa] failed to get pointer to moveSpace function..");
        move_space_fp = 0;
    } else {
#ifdef __x86_64__
        move_space_fp = move_space_addr;
#elif __arm64__
        move_space_fp = (uint64_t) ptrauth_sign_unauthenticated((void *) move_space_addr, ptrauth_key_asia, 0);
#endif
    }
}

// ---- Dock model helpers (upstream implementations, verbatim semantics) ----

static inline id get_ivar_value(id instance, const char *name)
{
    id result = nil;
    object_getInstanceVariable(instance, name, (void **) &result);
    return result;
}

static inline void set_ivar_value(id instance, const char *name, id value)
{
    object_setInstanceVariable(instance, name, value);
}

static inline uint64_t get_space_id(id space)
{
    return ((uint64_t (*)(id, SEL)) objc_msgSend)(space, @selector(spid));
}

static inline id space_for_display_with_id(CFStringRef display_uuid, uint64_t space_id)
{
    NSArray *spaces_for_display = ((NSArray *(*)(id, SEL, CFStringRef)) objc_msgSend)(dock_spaces, @selector(spacesForDisplay:), display_uuid);
    for (id space in spaces_for_display) {
        if (space_id == get_space_id(space)) {
            return space;
        }
    }
    return nil;
}

static inline id display_space_for_display_uuid(CFStringRef display_uuid)
{
    id result = nil;

    NSArray *display_spaces = get_ivar_value(dock_spaces, "_displaySpaces");
    if (display_spaces != nil) {
        for (id display_space in display_spaces) {
            id display_source_space = get_ivar_value(display_space, "_currentSpace");
            uint64_t sid = get_space_id(display_source_space);
            CFStringRef uuid = SLSCopyManagedDisplayForSpace(SLSMainConnectionID(), sid);
            bool match = CFEqual(uuid, display_uuid);
            CFRelease(uuid);
            if (match) {
                result = display_space;
                break;
            }
        }
    }

    return result;
}

static id display_space_for_space_with_id(uint64_t space_id)
{
    NSArray *display_spaces = get_ivar_value(dock_spaces, "_displaySpaces");
    if (display_spaces != nil) {
        for (id display_space in display_spaces) {
            id display_source_space = get_ivar_value(display_space, "_currentSpace");
            if (get_space_id(display_source_space) == space_id) {
                return display_space;
            }
        }
    }
    return nil;
}

static void do_space_move(char *message)
{
    if (dock_spaces == nil || dp_desktop_picture_manager == nil || move_space_fp == 0) return;

    uint64_t source_space_id, dest_space_id, source_prev_space_id;
    unpack(source_space_id);
    unpack(dest_space_id);
    unpack(source_prev_space_id);

    bool focus_dest_space;
    unpack(focus_dest_space);

    CFStringRef source_display_uuid = SLSCopyManagedDisplayForSpace(SLSMainConnectionID(), source_space_id);
    id source_space = space_for_display_with_id(source_display_uuid, source_space_id);
    id source_display_space = display_space_for_display_uuid(source_display_uuid);

    CFStringRef dest_display_uuid = SLSCopyManagedDisplayForSpace(SLSMainConnectionID(), dest_space_id);
    id dest_space = space_for_display_with_id(dest_display_uuid, dest_space_id);
    unsigned dest_display_id = ((unsigned (*)(id, SEL, id)) objc_msgSend)(dock_spaces, @selector(displayIDForSpace:), dest_space);
    id dest_display_space = display_space_for_display_uuid(dest_display_uuid);

    if (source_prev_space_id) {
        NSArray *ns_source_space = @[ @(source_space_id) ];
        NSArray *ns_dest_space = @[ @(source_prev_space_id) ];
        id new_source_space = space_for_display_with_id(source_display_uuid, source_prev_space_id);
        SLSShowSpaces(SLSMainConnectionID(), (__bridge CFArrayRef) ns_dest_space);
        SLSHideSpaces(SLSMainConnectionID(), (__bridge CFArrayRef) ns_source_space);
        SLSManagedDisplaySetCurrentSpace(SLSMainConnectionID(), source_display_uuid, source_prev_space_id);
        set_ivar_value(source_display_space, "_currentSpace", [new_source_space retain]);
        [ns_dest_space release];
        [ns_source_space release];
    }

    asm__call_move_space(source_space, dest_space, dest_display_uuid, dock_spaces, move_space_fp);

    dispatch_sync(dispatch_get_main_queue(), ^{
        ((void (*)(id, SEL, id, unsigned, CFStringRef)) objc_msgSend)(dp_desktop_picture_manager, @selector(moveSpace:toDisplay:displayUUID:), source_space, dest_display_id, dest_display_uuid);
    });

    if (focus_dest_space) {
        uint64_t new_source_space_id = SLSManagedDisplayGetCurrentSpace(SLSMainConnectionID(), source_display_uuid);
        id new_source_space = space_for_display_with_id(source_display_uuid, new_source_space_id);
        set_ivar_value(source_display_space, "_currentSpace", [new_source_space retain]);

        NSArray *ns_dest_monitor_space = @[ @(dest_space_id) ];
        SLSHideSpaces(SLSMainConnectionID(), (__bridge CFArrayRef) ns_dest_monitor_space);
        SLSManagedDisplaySetCurrentSpace(SLSMainConnectionID(), dest_display_uuid, source_space_id);
        set_ivar_value(dest_display_space, "_currentSpace", [source_space retain]);
        [ns_dest_monitor_space release];
    }

    CFRelease(source_display_uuid);
    CFRelease(dest_display_uuid);
}

static void do_space_destroy(char *message)
{
    if (dock_spaces == nil || remove_space_fp == 0) return;

    uint64_t space_id;
    unpack(space_id);

    CFStringRef display_uuid = SLSCopyManagedDisplayForSpace(SLSMainConnectionID(), space_id);
    uint64_t active_space_id = SLSManagedDisplayGetCurrentSpace(SLSMainConnectionID(), display_uuid);

    id space = space_for_display_with_id(display_uuid, space_id);
    id display_space = display_space_for_display_uuid(display_uuid);

    dispatch_sync(dispatch_get_main_queue(), ^{
        ((remove_space_call) remove_space_fp)(space, display_space, dock_spaces, space_id, space_id);
    });

    if (active_space_id == space_id) {
        uint64_t dest_space_id = SLSManagedDisplayGetCurrentSpace(SLSMainConnectionID(), display_uuid);
        id dest_space = space_for_display_with_id(display_uuid, dest_space_id);
        set_ivar_value(display_space, "_currentSpace", [dest_space retain]);
    }

    CFRelease(display_uuid);
}

static void do_space_create(char *message)
{
    if (dock_spaces == nil || add_space_fp == 0) return;

    uint64_t space_id;
    unpack(space_id);

    CFStringRef __block display_uuid = SLSCopyManagedDisplayForSpace(SLSMainConnectionID(), space_id);
    dispatch_sync(dispatch_get_main_queue(), ^{
        id new_space = macOSSequoia
                     ? [[objc_getClass("ManagedSpace") alloc] init]
                     : [[objc_getClass("Dock.ManagedSpace") alloc] init];
        id display_space = display_space_for_display_uuid(display_uuid);
        asm__call_add_space(new_space, display_space, add_space_fp);
        CFRelease(display_uuid);
    });
}

static bool do_space_focus(char *message)
{
    if (dock_spaces == nil) return false;

    uint64_t dest_space_id;
    unpack(dest_space_id);
    if (!dest_space_id) return false;
    bool accepted = false;
    {
        CFStringRef dest_display = SLSCopyManagedDisplayForSpace(SLSMainConnectionID(), dest_space_id);
        if (!dest_display) return false;
        id source_space = macOSSequoia
                        ? ((id (*)(id, SEL, CFStringRef)) objc_msgSend)(dock_spaces, @selector(currentSpaceForDisplayUUID:), dest_display)
                        : ((id (*)(id, SEL, CFStringRef)) objc_msgSend)(dock_spaces, @selector(currentSpaceforDisplayUUID:), dest_display);
        uint64_t source_space_id = get_space_id(source_space);

        if (source_space_id == dest_space_id) {
            accepted = true;
        } else {
            id dest_space = space_for_display_with_id(dest_display, dest_space_id);
            if (dest_space != nil) {
                id display_space = display_space_for_space_with_id(source_space_id);
                if (display_space != nil) {
                    NSArray *ns_source_space = @[ @(source_space_id) ];
                    NSArray *ns_dest_space = @[ @(dest_space_id) ];
                    SLSShowSpaces(SLSMainConnectionID(), (__bridge CFArrayRef) ns_dest_space);
                    SLSHideSpaces(SLSMainConnectionID(), (__bridge CFArrayRef) ns_source_space);
                    SLSManagedDisplaySetCurrentSpace(SLSMainConnectionID(), dest_display, dest_space_id);
                    set_ivar_value(display_space, "_currentSpace", [dest_space retain]);
                    accepted = true;
                    [ns_dest_space release];
                    [ns_source_space release];
                }
            }
        }

        CFRelease(dest_display);
    }
    return accepted;
}

static void do_window_scale(char *message)
{
    uint32_t wid;
    unpack(wid);
    if (!wid) return;

    CGRect frame = {};
    SLSGetWindowBounds(SLSMainConnectionID(), wid, &frame);
    CGAffineTransform original_transform = CGAffineTransformMakeTranslation(-frame.origin.x, -frame.origin.y);

    CGAffineTransform current_transform;
    SLSGetWindowTransform(SLSMainConnectionID(), wid, &current_transform);

    if (CGAffineTransformEqualToTransform(current_transform, original_transform)) {
        float dx, dy, dw, dh;
        unpack(dx);
        unpack(dy);
        unpack(dw);
        unpack(dh);

        int target_width  = dw / 4;
        int target_height = target_width / (frame.size.width/frame.size.height);

        float x_scale = frame.size.width/target_width;
        float y_scale = frame.size.height/target_height;

        CGFloat transformed_x = -(dx+dw) + (frame.size.width * (1/x_scale));
        CGFloat transformed_y = -dy;

        CGAffineTransform scale = CGAffineTransformConcat(CGAffineTransformIdentity, CGAffineTransformMakeScale(x_scale, y_scale));
        CGAffineTransform transform = CGAffineTransformTranslate(scale, transformed_x, transformed_y);
        SLSSetWindowTransform(SLSMainConnectionID(), wid, transform);
    } else {
        SLSSetWindowTransform(SLSMainConnectionID(), wid, original_transform);
    }
}

static void do_window_opacity(char *message)
{
    uint32_t wid;
    unpack(wid);
    if (!wid) return;

    float alpha;
    unpack(alpha);

    pthread_mutex_lock(&fade_lock);
    bool interrupting_self = fade_wid == wid && fade_thread != (pthread_t) 0;
    pthread_mutex_unlock(&fade_lock);

    if (interrupting_self) {
        // An in-flight fade targets this window: tell it to land immediately.
        pthread_mutex_lock(&fade_lock);
        fade_alpha = alpha;
        fade_duration = 0.0f;
        __asm__ __volatile__ ("" ::: "memory");
        fade_skip = true;
        pthread_mutex_unlock(&fade_lock);
    } else {
        SLSSetWindowAlpha(SLSMainConnectionID(), wid, alpha);
    }
}

static void *window_fade_thread_proc(void *data)
{
    uint32_t wid = (uint32_t)(uintptr_t) data;
entry:;
    float start_alpha;
    float end_alpha;
    float duration;
    bool skip;
    pthread_mutex_lock(&fade_lock);
    end_alpha = fade_alpha;
    duration = fade_duration;
    skip = fade_skip;
    pthread_mutex_unlock(&fade_lock);

    SLSGetWindowAlpha(SLSMainConnectionID(), wid, &start_alpha);

    if (skip) {
        SLSSetWindowAlpha(SLSMainConnectionID(), wid, end_alpha);
        goto done;
    }

    {
        int frame_duration = 8;
        int total_duration = (int)((duration * 1000.0f));
        int frame_count = (int)(((float) total_duration / (float) frame_duration) + 1.0f);

        for (int frame_index = 1; frame_index <= frame_count; ++frame_index) {
            pthread_mutex_lock(&fade_lock);
            skip = fade_skip;
            end_alpha = fade_alpha;
            pthread_mutex_unlock(&fade_lock);

            if (skip) {
                SLSSetWindowAlpha(SLSMainConnectionID(), wid, end_alpha);
                goto done;
            }

            float t = (float) frame_index / (float) frame_count;
            if (t < 0.0f) t = 0.0f;
            if (t > 1.0f) t = 1.0f;

            float alpha = (1.0f - t) * start_alpha + t * end_alpha;
            SLSSetWindowAlpha(SLSMainConnectionID(), wid, alpha);

            usleep(frame_duration*1000);
        }
    }

done:;
    pthread_mutex_lock(&fade_lock);
    fade_wid = 0;
    fade_thread = (pthread_t) 0;
    fade_skip = false;
    pthread_mutex_unlock(&fade_lock);
    return NULL;
}

static void do_window_opacity_fade(char *message)
{
    uint32_t wid;
    unpack(wid);
    if (!wid) return;

    float alpha, duration;
    unpack(alpha);
    unpack(duration);

    pthread_mutex_lock(&fade_lock);
    bool busy_for_other = fade_wid != 0 && fade_wid != wid;
    pthread_mutex_unlock(&fade_lock);

    if (busy_for_other) {
        // Single-slot policy: never queue fades behind each other.
        SLSSetWindowAlpha(SLSMainConnectionID(), wid, alpha);
        return;
    }

    pthread_mutex_lock(&fade_lock);
    bool already_fading = fade_wid == wid;
    fade_wid = wid;
    fade_alpha = alpha;
    fade_duration = duration;
    __asm__ __volatile__ ("" ::: "memory");
    fade_skip = already_fading; // restart cleanly when retargeting same window
    bool start = !already_fading;
    pthread_t existing = fade_thread;
    pthread_mutex_unlock(&fade_lock);

    if (start) {
        if (pthread_create(&fade_thread, NULL, &window_fade_thread_proc, (void *)(uintptr_t) wid) == 0) {
            pthread_detach(fade_thread);
        } else {
            pthread_mutex_lock(&fade_lock);
            fade_wid = 0;
            fade_thread = (pthread_t) 0;
            pthread_mutex_unlock(&fade_lock);
            SLSSetWindowAlpha(SLSMainConnectionID(), wid, alpha);
        }
    } else {
        (void) existing;
    }
}

static void do_window_layer(char *message)
{
    uint32_t wid;
    unpack(wid);
    if (!wid) return;

    int layer;
    unpack(layer);

    SLSSetWindowSubLevel(SLSMainConnectionID(), wid, CGWindowLevelForKey(layer));
}

static void do_window_sticky(char *message)
{
    uint32_t wid;
    unpack(wid);
    if (!wid) return;

    bool value;
    unpack(value);

    uint64_t tags = (1 << 11);
    if (value == 1) {
        SLSSetWindowTags(SLSMainConnectionID(), wid, &tags, 64);
    } else {
        SLSClearWindowTags(SLSMainConnectionID(), wid, &tags, 64);
    }
}

static void do_window_shadow(char *message)
{
    uint32_t wid;
    unpack(wid);
    if (!wid) return;

    bool value;
    unpack(value);

    uint64_t tags = (1 << 3);
    if (value == 1) {
        SLSClearWindowTags(SLSMainConnectionID(), wid, &tags, 64);
    } else {
        SLSSetWindowTags(SLSMainConnectionID(), wid, &tags, 64);
    }
}

static void do_handshake(int sockfd)
{
    uint32_t attrib = 0;

    // Capability bits report what ACTUALLY resolved in this Dock process.
    if (dock_spaces != nil)                attrib |= OSAX_ATTRIB_DOCK_SPACES;
    if (dp_desktop_picture_manager != nil) attrib |= OSAX_ATTRIB_DPPM;
    if (add_space_fp)                      attrib |= OSAX_ATTRIB_ADD_SPACE;
    if (remove_space_fp)                   attrib |= OSAX_ATTRIB_REM_SPACE;
    if (move_space_fp)                     attrib |= OSAX_ATTRIB_MOV_SPACE;
    if (dock_spaces != nil)                attrib |= OSAX_ATTRIB_FOCUS_SPACE;
    attrib |= OSAX_ATTRIB_WINDOW_OPACITY | OSAX_ATTRIB_WINDOW_LAYER |
              OSAX_ATTRIB_WINDOW_STICKY | OSAX_ATTRIB_WINDOW_SHADOW |
              OSAX_ATTRIB_WINDOW_SCALE;

    char bytes[BUFSIZ] = {};
    int version_length = strlen(ROVR_SA_VERSION);
    int attrib_length = sizeof(uint32_t);
    int bytes_length = version_length + 1 + attrib_length;

    memcpy(bytes, ROVR_SA_VERSION, version_length);
    memcpy(bytes + version_length + 1, &attrib, attrib_length);
    bytes[version_length] = '\0';
    bytes[bytes_length] = '\n';

    send(sockfd, bytes, bytes_length+1, 0);
}

static size_t expected_message_length(enum sa_opcode op)
{
    switch (op) {
    case SA_OPCODE_HANDSHAKE: return 1;
    case SA_OPCODE_SPACE_FOCUS:
    case SA_OPCODE_SPACE_CREATE:
    case SA_OPCODE_SPACE_DESTROY: return 1 + sizeof(uint64_t);
    case SA_OPCODE_SPACE_MOVE: return 1 + sizeof(uint64_t) * 3 + sizeof(uint8_t);
    case SA_OPCODE_WINDOW_OPACITY:
    case SA_OPCODE_WINDOW_OPACITY_FADE: return 1 + sizeof(uint32_t) + sizeof(float) * 2;
    case SA_OPCODE_WINDOW_LAYER: return 1 + sizeof(uint32_t) + sizeof(int32_t);
    case SA_OPCODE_WINDOW_STICKY:
    case SA_OPCODE_WINDOW_SHADOW: return 1 + sizeof(uint32_t) + sizeof(uint8_t);
    case SA_OPCODE_WINDOW_SCALE: return 1 + sizeof(uint32_t) + sizeof(float) * 4;
    default: return 0;
    }
}

static uint8_t handle_message(int sockfd, char *message, size_t length)
{
    enum sa_opcode op = (enum sa_opcode)(uint8_t)*message++;
    if (expected_message_length(op) != length) return SA_STATUS_BAD_FRAME;
    if (op != SA_OPCODE_HANDSHAKE) {
        uint64_t identifier = 0;
        memcpy(&identifier, message, op == SA_OPCODE_SPACE_MOVE ||
               op == SA_OPCODE_SPACE_FOCUS || op == SA_OPCODE_SPACE_CREATE ||
               op == SA_OPCODE_SPACE_DESTROY ? sizeof(uint64_t) : sizeof(uint32_t));
        if (identifier == 0) return SA_STATUS_INVALID;
    }
    switch (op) {
    case SA_OPCODE_HANDSHAKE: do_handshake(sockfd); return SA_STATUS_OK;
    case SA_OPCODE_SPACE_FOCUS:
        if (dock_spaces == nil) return SA_STATUS_UNSUPPORTED;
        if (!do_space_focus(message)) return SA_STATUS_INVALID;
        break;
    case SA_OPCODE_SPACE_CREATE:
        if (dock_spaces == nil || add_space_fp == 0) return SA_STATUS_UNSUPPORTED;
        do_space_create(message); break;
    case SA_OPCODE_SPACE_DESTROY:
        if (dock_spaces == nil || remove_space_fp == 0) return SA_STATUS_UNSUPPORTED;
        do_space_destroy(message); break;
    case SA_OPCODE_SPACE_MOVE:
        if (dock_spaces == nil || dp_desktop_picture_manager == nil || move_space_fp == 0)
            return SA_STATUS_UNSUPPORTED;
        do_space_move(message); break;
    case SA_OPCODE_WINDOW_OPACITY: do_window_opacity(message); break;
    case SA_OPCODE_WINDOW_OPACITY_FADE: do_window_opacity_fade(message); break;
    case SA_OPCODE_WINDOW_LAYER: do_window_layer(message); break;
    case SA_OPCODE_WINDOW_STICKY: do_window_sticky(message); break;
    case SA_OPCODE_WINDOW_SHADOW: do_window_shadow(message); break;
    case SA_OPCODE_WINDOW_SCALE: do_window_scale(message); break;
    default: return SA_STATUS_UNSUPPORTED;
    }
    return SA_STATUS_OK;
}

static bool read_message(int sockfd, char *message, size_t *message_length)
{
    int16_t length = 0;
    int bytes_read = 0;

    do {
        int cur_read = read(sockfd, ((char *) &length) + bytes_read, sizeof(int16_t)-bytes_read);
        if (cur_read <= 0) break;

        bytes_read += cur_read;
    } while (bytes_read < (int) sizeof(int16_t));

    if (length > 2 && length <= SA_SOCKET_BUFF_LEN) {
        bytes_read = 0;
        int bytes_to_read = length - sizeof(int16_t);

        do {
            int cur_read = read(sockfd, message+bytes_read, bytes_to_read-bytes_read);
            if (cur_read <= 0) break;

            bytes_read += cur_read;
        } while (bytes_read < bytes_to_read);

        if (bytes_read == bytes_to_read) {
            *message_length = (size_t)bytes_to_read;
            return true;
        }
        return false;
    }

    return false;
}

static void *handle_connection(void *unused)
{
    for (;;) {
        int sockfd = accept(daemon_sockfd, NULL, 0);
        if (sockfd == -1) continue;

        uid_t peer_uid = UINT32_MAX;
        gid_t peer_gid = 0;
        if (getpeereid(sockfd, &peer_uid, &peer_gid) != 0 ||
            (peer_uid != getuid() && peer_uid != 0)) {
            shutdown(sockfd, SHUT_RDWR);
            close(sockfd);
            continue;
        }
        struct timeval timeout = { .tv_sec = 2, .tv_usec = 0 };
        setsockopt(sockfd, SOL_SOCKET, SO_RCVTIMEO, &timeout, sizeof(timeout));
        setsockopt(sockfd, SOL_SOCKET, SO_SNDTIMEO, &timeout, sizeof(timeout));

        char message[SA_SOCKET_BUFF_LEN] = {0};
        size_t message_length = 0;
        if (read_message(sockfd, message, &message_length)) {
            enum sa_opcode op = (enum sa_opcode)(uint8_t)message[0];
            uint8_t status = handle_message(sockfd, message, message_length);
            if (op != SA_OPCODE_HANDSHAKE) send(sockfd, &status, sizeof(status), 0);
        } else {
            uint8_t status = SA_STATUS_BAD_FRAME;
            send(sockfd, &status, sizeof(status), 0);
        }

        shutdown(sockfd, SHUT_RDWR);
        close(sockfd);
    }

    return NULL;
}

static bool ensure_runtime_dir(char *runtime_dir)
{
    struct stat st = {0};
    if (lstat(runtime_dir, &st) == 0) {
        return S_ISDIR(st.st_mode) && !S_ISLNK(st.st_mode) &&
               st.st_uid == getuid() && (st.st_mode & 0077) == 0;
    }
    if (mkdir(runtime_dir, 0700) != 0) return false;
    return chmod(runtime_dir, 0700) == 0;
}

static bool start_daemon(char *runtime_dir, char *socket_path)
{
    if (!ensure_runtime_dir(runtime_dir)) return false;
    struct sockaddr_un socket_address = {0};
    socket_address.sun_family = AF_UNIX;
    snprintf(socket_address.sun_path, sizeof(socket_address.sun_path), "%s", socket_path);
    unlink(socket_path);

    if ((daemon_sockfd = socket(AF_UNIX, SOCK_STREAM, 0)) == -1) {
        return false;
    }

    if (bind(daemon_sockfd, (struct sockaddr *) &socket_address, sizeof(socket_address)) == -1) {
        return false;
    }

    if (chmod(socket_path, 0600) != 0) {
        return false;
    }

    if (listen(daemon_sockfd, SOMAXCONN) == -1) {
        return false;
    }

    init_instances();
    pthread_create(&daemon_thread, NULL, &handle_connection, NULL);

    return true;
}

__attribute__((constructor))
void load_payload(void)
{
    NSLog(@"[rovr-sa] loaded payload..");

    // Rovr keys the socket on UID (stable under launchd even when $USER is
    // unset or differs); $USER is not consulted at all.
    uid_t uid = getuid();

    char runtime_dir[255];
    char socket_file[255];
    snprintf(runtime_dir, sizeof(runtime_dir), SA_RUNTIME_DIR_FMT, uid);
    snprintf(socket_file, sizeof(socket_file), SA_SOCKET_PATH_FMT, uid);

    if (start_daemon(runtime_dir, socket_file)) {
        NSLog(@"[rovr-sa] now listening on %s..", socket_file);
    } else {
        NSLog(@"[rovr-sa] failed to start socket listener..");
    }
}
