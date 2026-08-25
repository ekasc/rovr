// Minimal scratch host for Rovr SA protocol interop testing.
//
// dlopen()s the payload dylib given as argv[1] so its constructor runs
// (socket bind + listener thread) OUTSIDE Dock.app, then idles forever.
// In this host the Dock-internals pattern scans cannot resolve, so the
// payload must degrade honestly: cosmetic capability bits set, space
// bits absent, space opcodes NAKed with SA_STATUS_UNSUPPORTED.
//
// Build: clang -O2 -o host host.c
// Usage: ./host /path/to/librovr_sa_payload.dylib

#include <dlfcn.h>
#include <signal.h>
#include <stdio.h>
#include <unistd.h>

int main(int argc, char **argv)
{
    if (argc != 2) {
        fprintf(stderr, "usage: %s <payload.dylib>\n", argv[0]);
        return 2;
    }

    signal(SIGTERM, SIG_DFL);

    void *handle = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
    if (!handle) {
        fprintf(stderr, "dlopen failed: %s\n", dlerror());
        return 1;
    }
    printf("payload loaded, host pid %d\n", getpid());
    fflush(stdout);

    // The payload's listener thread keeps the process alive; pause() just
    // guarantees the main thread never returns (which would unload us).
    for (;;) pause();
}
