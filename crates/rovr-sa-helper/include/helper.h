// Rovr SA helper — shared constants with the Rust client
// (crates/rovr-platform/src/macos/reinject.rs). Keep in sync.
//
// Wire protocol (fixed 16-byte frames, little-endian):
//   request:  u32 magic | u32 proto | u32 opcode | u32 uid
//   response: u32 magic | u32 proto | u32 status  | i32 dock_pid
//
// There is deliberately NO field for a pid, path, command or environment:
// the helper resolves Dock itself and only ever executes the fixed
// root-owned loader against the fixed root-owned payload.

#define ROVR_SA_HELPER_MAGIC   0x31485652u /* "RVH1" little-endian */
#define ROVR_SA_HELPER_PROTO   1u

#define ROVR_SA_OP_INJECT      1u
#define ROVR_SA_OP_STATUS      2u

/* Response status codes */
#define ROVR_SA_ST_OK               0u
#define ROVR_SA_ST_UNAUTHORIZED     1u
#define ROVR_SA_ST_BAD_REQUEST      2u
#define ROVR_SA_ST_DOCK_NOT_FOUND   3u
#define ROVR_SA_ST_ARTIFACTS_INVALID 4u
#define ROVR_SA_ST_INJECTION_FAILED 5u
#define ROVR_SA_ST_INTERNAL         6u

#pragma pack(push, 1)
typedef struct {
    uint32_t magic;
    uint32_t proto;
    uint32_t opcode;
    uint32_t uid;
} rovr_sa_request;

typedef struct {
    uint32_t magic;
    uint32_t proto;
    uint32_t status;
    int32_t dock_pid;
} rovr_sa_response;
#pragma pack(pop)
