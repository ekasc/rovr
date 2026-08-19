use std::ffi::{c_char, c_void, CStr};

use rovr_core::Action;
use rovr_types::{
    Capabilities, DisplayId, DisplaySnapshot, PlatformSnapshot, ProcessId, Rect, WindowId,
    WindowSnapshot,
};

use crate::{Platform, PlatformError};

const ROVR_APP_MAX: usize = 256;
const ROVR_TITLE_MAX: usize = 512;
const ROVR_BUNDLE_MAX: usize = 256;
const ROVR_CAP_OBSERVE_WINDOWS: u64 = 1 << 0;
const ROVR_CAP_SET_WINDOW_FRAME: u64 = 1 << 1;
const ROVR_CAP_FOCUS_WINDOW: u64 = 1 << 2;

#[repr(C)]
#[derive(Clone, Copy)]
struct BridgeWindow {
    id: u32,
    pid: i32,
    display_id: u32,
    focused: u8,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    app: [c_char; ROVR_APP_MAX],
    title: [c_char; ROVR_TITLE_MAX],
    bundle_id: [c_char; ROVR_BUNDLE_MAX],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct BridgeDisplay {
    id: u32,
    focused: u8,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

type WindowCallback = unsafe extern "C" fn(*const BridgeWindow, *mut c_void);
type DisplayCallback = unsafe extern "C" fn(*const BridgeDisplay, *mut c_void);

extern "C" {
    fn rovr_bridge_init() -> i32;
    fn rovr_bridge_capabilities() -> u64;
    fn rovr_bridge_enumerate_windows(callback: WindowCallback, context: *mut c_void) -> i32;
    fn rovr_bridge_enumerate_displays(callback: DisplayCallback, context: *mut c_void) -> i32;
    fn rovr_bridge_set_window_frame(window_id: u32, x: f64, y: f64, width: f64, height: f64)
        -> i32;
    fn rovr_bridge_focus_window(window_id: u32) -> i32;
}

pub struct MacPlatform {
    bridge_capabilities: u64,
}

impl MacPlatform {
    pub fn new() -> Result<Self, PlatformError> {
        let status = unsafe { rovr_bridge_init() };
        if status != 0 {
            return Err(PlatformError::Operation(format!(
                "macOS bridge initialization failed with status {status}"
            )));
        }
        let bridge_capabilities = unsafe { rovr_bridge_capabilities() };
        Ok(Self {
            bridge_capabilities,
        })
    }
}

impl Platform for MacPlatform {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            observe_windows: self.bridge_capabilities & ROVR_CAP_OBSERVE_WINDOWS != 0,
            set_window_frame: self.bridge_capabilities & ROVR_CAP_SET_WINDOW_FRAME != 0,
            focus_window: self.bridge_capabilities & ROVR_CAP_FOCUS_WINDOW != 0,
            move_window_to_space: false,
            create_space: false,
            destroy_space: false,
            focus_space: false,
            set_window_layer: false,
            set_window_opacity: false,
            scripting_addition: false,
        }
    }

    fn snapshot(&mut self) -> Result<PlatformSnapshot, PlatformError> {
        unsafe extern "C" fn collect_window(window: *const BridgeWindow, context: *mut c_void) {
            if window.is_null() || context.is_null() {
                return;
            }
            let window = *window;
            let windows = &mut *(context as *mut Vec<WindowSnapshot>);
            let bundle_id = c_string(&window.bundle_id);
            windows.push(WindowSnapshot {
                id: WindowId(window.id),
                pid: ProcessId(window.pid),
                app: c_string(&window.app).unwrap_or_default(),
                bundle_id,
                title: c_string(&window.title).unwrap_or_default(),
                frame: Rect {
                    x: window.x,
                    y: window.y,
                    width: window.width,
                    height: window.height,
                },
                space_id: None,
                display_id: (window.display_id != 0).then_some(DisplayId(window.display_id)),
                focused: window.focused != 0,
                minimized: false,
                fullscreen: false,
                managed: true,
                generation: 0,
            });
        }

        unsafe extern "C" fn collect_display(display: *const BridgeDisplay, context: *mut c_void) {
            if display.is_null() || context.is_null() {
                return;
            }
            let display = *display;
            let displays = &mut *(context as *mut Vec<DisplaySnapshot>);
            displays.push(DisplaySnapshot {
                id: DisplayId(display.id),
                frame: Rect {
                    x: display.x,
                    y: display.y,
                    width: display.width,
                    height: display.height,
                },
                label: None,
                focused: display.focused != 0,
                generation: 0,
            });
        }

        let mut windows = Vec::new();
        let window_status = unsafe {
            rovr_bridge_enumerate_windows(collect_window, &mut windows as *mut _ as *mut c_void)
        };
        if window_status != 0 {
            return Err(PlatformError::Operation(format!(
                "window enumeration failed with status {window_status}"
            )));
        }

        let mut displays = Vec::new();
        let display_status = unsafe {
            rovr_bridge_enumerate_displays(collect_display, &mut displays as *mut _ as *mut c_void)
        };
        if display_status != 0 {
            return Err(PlatformError::Operation(format!(
                "display enumeration failed with status {display_status}"
            )));
        }

        Ok(PlatformSnapshot {
            windows,
            spaces: vec![],
            displays,
            complete: true,
        })
    }

    fn execute(&mut self, action: &Action) -> Result<(), PlatformError> {
        let status = match action {
            Action::RefreshAll | Action::RefreshWindow { .. } => return Ok(()),
            Action::SetWindowFrame { window, frame } => unsafe {
                rovr_bridge_set_window_frame(window.0, frame.x, frame.y, frame.width, frame.height)
            },
            Action::FocusWindow { window } => unsafe { rovr_bridge_focus_window(window.0) },
            Action::MoveWindowToSpace { .. } => {
                return Err(PlatformError::Unsupported("move_window_to_space"))
            }
            Action::FocusDirection { .. } => {
                return Err(PlatformError::Unsupported("focus_direction"))
            }
        };

        if status == 0 {
            Ok(())
        } else {
            Err(PlatformError::Operation(format!(
                "bridge operation failed with status {status}"
            )))
        }
    }
}

fn c_string<const N: usize>(buffer: &[c_char; N]) -> Option<String> {
    if buffer[0] == 0 {
        return None;
    }
    let value = unsafe { CStr::from_ptr(buffer.as_ptr()) };
    Some(value.to_string_lossy().into_owned())
}
