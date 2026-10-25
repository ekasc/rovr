// Standalone check for the payload's OS-build gate parse.
//
// The SA payload gates its per-build offsets on a build string extracted from
// `operatingSystemVersionString`. The first implementation used the non-range
// getCString: overload, which copies the WHOLE string ("Version 27.2 (Build
// 26B5101f)") into a 16-byte buffer and silently produced "Version 27.2 (" —
// so `os_build_is("26B5101f")` was always false, every build-gated offset was
// skipped, and the payload kept failing to resolve Dock internals while the
// install reported a clean handshake.
//
// This runs the extraction logic verbatim against the live system and fails
// unless it produces the expected build id. Run it after touching the parse:
//   clang -framework Foundation scripts/verify-os-build-gate.m -o /tmp/gate && /tmp/gate
#import <Foundation/Foundation.h>
#include <stdio.h>
#include <string.h>

static char g_os_build[16] = "";

int main(void) {
    @autoreleasepool {
        NSString *build = nil;
        NSString *vs = [NSProcessInfo processInfo].operatingSystemVersionString;
        if (vs != nil) {
            NSRange open = [vs rangeOfString:@"(Build "];
            if (open.location != NSNotFound) {
                NSUInteger start = open.location + open.length;
                NSRange rest = NSMakeRange(start, [vs length] - start);
                NSRange close = [vs rangeOfString:@")" options:0 range:rest];
                if (close.location != NSNotFound && close.location > start) {
                    build = [vs substringWithRange:NSMakeRange(start, close.location - start)];
                }
            }
        }
        if (build != nil && [build length] < sizeof(g_os_build)) {
            [build getCString:g_os_build
                   maxLength:sizeof(g_os_build)
                    encoding:NSUTF8StringEncoding];
        }

        printf("version string : %s\n", vs.UTF8String ?: "(nil)");
        printf("extracted build: '%s'\n", g_os_build);

        // The payload's own gate, as implemented in src/payload.m.
        int gate = g_os_build[0] != '\0' && strcmp(g_os_build, "26B5101f") == 0;
        printf("os_build_is(26B5101f): %s\n", gate ? "YES" : "NO");

        if (!gate) {
            fprintf(stderr, "\nFAIL: the build gate is off, so the 26B5101f offsets\n"
                            "      in vendor/arm64_payload.m will never be selected.\n");
            return 1;
        }
        printf("\nOK: build gate selects the 26B5101f offsets.\n");
    }
    return 0;
}
