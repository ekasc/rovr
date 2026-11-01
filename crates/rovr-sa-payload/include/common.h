//
// Rovr scripting-addition protocol constants.
//
// Wire framing is identical to yabai's osax/common.h (MIT © 2019 Åsmund
// Vikane) so the transport code stays reviewable against upstream, but the
// socket namespace and version prefix are Rovr-owned. Rovr implements ONLY
// the opcodes below — no window-move/focus/order/swap-proxy surface.
//
// This file is a Rovr-owned adaptation; see docs/YABAI_PORT.md for license
// discipline.

#ifndef ROVR_SA_COMMON_H
#define ROVR_SA_COMMON_H

#define ROVR_SA_VERSION "rovr-sa-2.0"

// Rovr-owned 0700 runtime directory (relative to the console user's home) and
// socket namespace (never yabai's). Deliberately NOT under /tmp: macOS runs
// com.apple.tmp_cleaner daily, deleting /tmp entries whose atime, mtime AND
// ctime are all older than 3 days. A bound Unix socket never touches its path
// again, so its timestamps freeze at bind time and the socket file is deleted
// ~3 days after injection while the payload keeps running — the SA silently
// becomes unreachable until Dock restarts. This is the same reason the daemon
// socket lives in ~/Library/Caches/rovr.
#define SA_RUNTIME_SUBDIR "Library/Caches/rovr"
#define SA_SOCKET_NAME "sa.sock"

#define OSAX_ATTRIB_DOCK_SPACES    (1 << 0)
#define OSAX_ATTRIB_DPPM           (1 << 1)
#define OSAX_ATTRIB_ADD_SPACE      (1 << 2)
#define OSAX_ATTRIB_REM_SPACE      (1 << 3)
#define OSAX_ATTRIB_MOV_SPACE      (1 << 4)
#define OSAX_ATTRIB_FOCUS_SPACE    (1 << 5)
#define OSAX_ATTRIB_WINDOW_OPACITY (1 << 6)
#define OSAX_ATTRIB_WINDOW_LAYER   (1 << 7)
#define OSAX_ATTRIB_WINDOW_STICKY  (1 << 8)
#define OSAX_ATTRIB_WINDOW_SHADOW  (1 << 9)
#define OSAX_ATTRIB_WINDOW_SCALE   (1 << 10)

#define SA_STATUS_OK          0
#define SA_STATUS_BAD_FRAME   1
#define SA_STATUS_UNSUPPORTED 2
#define SA_STATUS_INVALID     3

enum sa_opcode
{
    SA_OPCODE_HANDSHAKE             = 0x01,
    SA_OPCODE_SPACE_FOCUS           = 0x02,
    SA_OPCODE_SPACE_CREATE          = 0x03,
    SA_OPCODE_SPACE_DESTROY         = 0x04,
    SA_OPCODE_SPACE_MOVE            = 0x05,
    SA_OPCODE_WINDOW_OPACITY        = 0x07,
    SA_OPCODE_WINDOW_OPACITY_FADE   = 0x08,
    SA_OPCODE_WINDOW_LAYER          = 0x09,
    SA_OPCODE_WINDOW_STICKY         = 0x0A,
    SA_OPCODE_WINDOW_SHADOW         = 0x0B,
    SA_OPCODE_WINDOW_SCALE          = 0x0D,
};

#endif
