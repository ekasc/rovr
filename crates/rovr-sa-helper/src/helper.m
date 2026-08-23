//
// Rovr privileged SA helper — runs as ROOT under launchd (socket-activated
// LaunchDaemon). Injects the FIXED root-owned Rovr payload into Dock.app by
// executing the FIXED root-owned loader. Nothing else.
//
// Security model:
//  - The request frame carries NO pid, NO path, NO command, NO environment.
//    The payload and loader paths are compile-time constants pointing at the
//    root-owned installed artifacts; Dock's pid is resolved HERE, never taken
//    from the caller.
//  - Callers are authenticated with getpeereid(): the peer uid must equal the
//    uid claimed in the request AND the owner of /dev/console (the GUI session
//    user). A process of any other user — or a non-GUI-session root process —
//    is refused. Sockets are in /tmp/rovr-<uid>/ (0700), so an
//    injection is always bound to the requesting console session.
//  - Artifacts are validated before every use: regular files (O_NOFOLLOW),
//    owned by root, expected modes, inside the root-owned install directory,
//    directory itself not writable by group/other.
//  - Event-driven only: launchd starts this helper when a client connects;
//    there is no polling loop anywhere in the privileged layer.
//
// Injection technique is delegated to rovr-sa-loader (adapted from yabai's
// src/osax/loader.m, MIT © 2019 Åsmund Vikane).
//

#import <Cocoa/Cocoa.h>
#import <sys/socket.h>
#import <sys/stat.h>
#import <sys/un.h>
#import <unistd.h>
#import <fcntl.h>
#import <launch.h>
#import <spawn.h>

#import "helper.h"

// Fixed install locations — must match crates/rovr-cli/src/main.rs.
static const char *ROVR_INSTALL_DIR    = "/Library/Application Support/rovr";
static const char *ROVR_PAYLOAD_PATH   = "/Library/Application Support/rovr/librovr_sa_payload.dylib";
static const char *ROVR_LOADER_PATH    = "/Library/Application Support/rovr/rovr-sa-loader";
#define ROVR_HELPER_SOCKET_PATH "/var/run/rovr-sa-helper.sock"

extern char **environ;

static int validate_artifacts(void)
{
    // Install directory: must exist, be a real dir (not symlink), root-owned,
    // and not writable by group/other.
    struct stat dir_st = {0};
    if (lstat(ROVR_INSTALL_DIR, &dir_st) != 0) return 0;
    if (!S_ISDIR(dir_st.st_mode)) return 0;
    if (dir_st.st_uid != 0) return 0;
    if (dir_st.st_mode & (S_IWGRP | S_IWOTH)) return 0;

    const char *paths[2] = { ROVR_PAYLOAD_PATH, ROVR_LOADER_PATH };
    for (int i = 0; i < 2; ++i) {
        struct stat st = {0};
        // lstat: refuse symlinks outright.
        if (lstat(paths[i], &st) != 0) return 0;
        if (S_ISLNK(st.st_mode) || !S_ISREG(st.st_mode)) return 0;
        if (st.st_uid != 0) return 0;
        if (st.st_mode & (S_IWGRP | S_IWOTH)) return 0;
        // Open with O_NOFOLLOW and re-fstat via the fd: closes the
        // swap-after-lstat race on every component of the final path.
        int fd = open(paths[i], O_RDONLY | O_NOFOLLOW);
        if (fd < 0) return 0;
        struct stat fst = {0};
        int ok = (fstat(fd, &fst) == 0 && S_ISREG(fst.st_mode) && fst.st_uid == 0);
        close(fd);
        if (!ok) return 0;
    }
    return 1;
}

static pid_t resolve_dock_pid(void)
{
    NSArray *list = [NSRunningApplication runningApplicationsWithBundleIdentifier:@"com.apple.dock"];
    if (list.count == 1) {
        NSRunningApplication *dock = list[0];
        if ([dock isFinishedLaunching] == YES) {
            return [dock processIdentifier];
        }
    }
    return 0;
}

static uint32_t console_session_uid(void)
{
    struct stat st = {0};
    if (stat("/dev/console", &st) != 0) return UINT32_MAX;
    return (uint32_t)st.st_uid;
}

// Run the fixed loader against the fixed payload with a minimal, fixed
// environment. No caller-controlled argv, env, or cwd.
static int run_injection(pid_t dock_pid)
{
    (void)dock_pid; // informational only; the loader resolves Dock itself too
    posix_spawnattr_t attr = {0};
    if (posix_spawnattr_init(&attr) != 0) return ROVR_SA_ST_INTERNAL;

    // Fixed minimal environment: nothing inherited from the request or daemon.
    char *child_env[] = {
        "PATH=/usr/bin:/bin:/usr/sbin:/sbin",
        NULL,
    };
    char *child_argv[] = {
        (char *)ROVR_LOADER_PATH,
        (char *)ROVR_PAYLOAD_PATH,
        NULL,
    };

    pid_t child = 0;
    int rc = posix_spawn(&child, ROVR_LOADER_PATH, NULL, &attr, child_argv, child_env);
    posix_spawnattr_destroy(&attr);
    if (rc != 0) return ROVR_SA_ST_INJECTION_FAILED;

    // Bounded wait: the loader finishes within ~1 s when it works at all.
    int status = 0;
    for (int i = 0; i < 100; ++i) {
        pid_t got = waitpid(child, &status, WNOHANG);
        if (got == child) {
            if (WIFEXITED(status) && WEXITSTATUS(status) == 0) return ROVR_SA_ST_OK;
            return ROVR_SA_ST_INJECTION_FAILED;
        }
        if (got < 0) return ROVR_SA_ST_INTERNAL;
        usleep(50000); // 50 ms x 100 = 5 s cap
    }
    kill(child, SIGKILL);
    while (waitpid(child, &status, 0) < 0 && errno == EINTR) {}
    return ROVR_SA_ST_INJECTION_FAILED;
}

static void send_response(int fd, uint32_t status, int32_t dock_pid)
{
    rovr_sa_response resp = {
        .magic = ROVR_SA_HELPER_MAGIC,
        .proto = ROVR_SA_HELPER_PROTO,
        .status = status,
        .dock_pid = dock_pid,
    };
    ssize_t off = 0;
    while (off < (ssize_t)sizeof(resp)) {
        ssize_t n = send(fd, ((char *)&resp) + off, sizeof(resp) - off, 0);
        if (n <= 0) return;
        off += n;
    }
}

static void handle_connection(int fd)
{
    // Authenticate before reading any caller-controlled bytes. The listener
    // checks this immediately after accept too; repeating here keeps this
    // function safe if reused.
    uid_t peer_uid = UINT32_MAX;
    gid_t peer_gid = 0;
    if (getpeereid(fd, &peer_uid, &peer_gid) != 0 ||
        (uint32_t)peer_uid != console_session_uid()) {
        send_response(fd, ROVR_SA_ST_UNAUTHORIZED, 0);
        return;
    }

    struct timeval tv = { .tv_sec = 1, .tv_usec = 0 };
    setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));
    setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &tv, sizeof(tv));

    rovr_sa_request req = {0};
    size_t got = 0;
    while (got < sizeof(req)) {
        ssize_t n = recv(fd, ((char *)&req) + got, sizeof(req) - got, 0);
        if (n <= 0) return;
        got += (size_t)n;
    }

    if (req.magic != ROVR_SA_HELPER_MAGIC || req.proto != ROVR_SA_HELPER_PROTO) {
        send_response(fd, ROVR_SA_ST_BAD_REQUEST, 0);
        return;
    }

    // The authenticated kernel identity must also match the claimed uid.
    if ((uint32_t)peer_uid != req.uid) {
        send_response(fd, ROVR_SA_ST_UNAUTHORIZED, 0);
        return;
    }

    switch (req.opcode) {
    case ROVR_SA_OP_STATUS: {
        pid_t dock = resolve_dock_pid();
        if (!validate_artifacts()) {
            send_response(fd, ROVR_SA_ST_ARTIFACTS_INVALID, (int32_t)dock);
            return;
        }
        send_response(fd, ROVR_SA_ST_OK, (int32_t)dock);
        return;
    }
    case ROVR_SA_OP_INJECT: {
        pid_t dock = resolve_dock_pid();
        if (dock <= 0) {
            send_response(fd, ROVR_SA_ST_DOCK_NOT_FOUND, 0);
            return;
        }
        if (!validate_artifacts()) {
            send_response(fd, ROVR_SA_ST_ARTIFACTS_INVALID, (int32_t)dock);
            return;
        }
        uint32_t st = run_injection(dock);
        send_response(fd, st, (int32_t)dock);
        return;
    }
    default:
        send_response(fd, ROVR_SA_ST_BAD_REQUEST, 0);
        return;
    }
}

int main(int argc, char **argv)
{
    (void)argc; (void)argv;

    int *fds = NULL;
    size_t cnt = 0;
    int listener = -1;

    if (launch_activate_socket("Listener", &fds, &cnt) == 0 && cnt >= 1) {
        listener = fds[0];
    } else {
        // Not running under launchd (manual/dev invocation as root): bind the
        // same fixed socket ourselves so `sudo rovr sa install` can exercise
        // the identical code path. Same fixed path, same validation.
        if (geteuid() != 0) {
            fprintf(stderr, "rovr-sa-helper must run as root\n");
            return 1;
        }
        unlink(ROVR_HELPER_SOCKET_PATH);
        listener = socket(AF_UNIX, SOCK_STREAM, 0);
        if (listener < 0) return 1;
        struct sockaddr_un addr = {0};
        addr.sun_family = AF_UNIX;
        strncpy(addr.sun_path, ROVR_HELPER_SOCKET_PATH, sizeof(addr.sun_path) - 1);
        if (bind(listener, (struct sockaddr *)&addr, sizeof(addr)) != 0) return 1;
        chmod(ROVR_HELPER_SOCKET_PATH, 0222); // connect-only; auth is per-peer
        if (listen(listener, 8) != 0) return 1;
        fprintf(stderr, "rovr-sa-helper listening (standalone) on %s\n", ROVR_HELPER_SOCKET_PATH);
    }

    if (listener < 0) return 1;

    // Event-driven accept loop. Authenticate immediately, then isolate each
    // bounded frame read so one silent peer cannot starve other callers.
    for (;;) {
        int conn = accept(listener, NULL, NULL);
        if (conn < 0) {
            if (errno == EINTR) continue;
            return 1;
        }
        uid_t peer_uid = UINT32_MAX;
        gid_t peer_gid = 0;
        if (getpeereid(conn, &peer_uid, &peer_gid) != 0 ||
            (uint32_t)peer_uid != console_session_uid()) {
            send_response(conn, ROVR_SA_ST_UNAUTHORIZED, 0);
            close(conn);
            continue;
        }
        dispatch_async(dispatch_get_global_queue(QOS_CLASS_UTILITY, 0), ^{
            handle_connection(conn);
            close(conn);
        });
    }
}
