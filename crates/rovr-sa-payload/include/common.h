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

// Rovr-owned 0700 runtime directory and socket namespace (never yabai's).
#define SA_RUNTIME_DIR_FMT "/tmp/rovr-%d"
#define SA_SOCKET_PATH_FMT "/tmp/rovr-%d/sa.sock"

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
