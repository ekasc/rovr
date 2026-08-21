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

#define ROVR_SA_VERSION "rovr-sa-1.0"

// Rovr-owned socket namespace: UID-based (never yabai's /tmp/yabai-sa_<user>.socket).
#define SA_SOCKET_PATH_FMT "/tmp/rovr-sa_%d.sock"

#define OSAX_ATTRIB_DOCK_SPACES    (1 << 0)
#define OSAX_ATTRIB_DPPM           (1 << 1)
#define OSAX_ATTRIB_ADD_SPACE      (1 << 2)
#define OSAX_ATTRIB_REM_SPACE      (1 << 3)
#define OSAX_ATTRIB_MOV_SPACE      (1 << 4)

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
