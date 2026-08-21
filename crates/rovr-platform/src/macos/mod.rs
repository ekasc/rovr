pub mod sa;

use std::ffi::{c_char, c_void, CStr};
use std::sync::mpsc;
use std::time::Duration;

use rovr_core::Action;
use rovr_types::{
    Capabilities, DisplayId, DisplaySnapshot, PlatformSnapshot, ProcessId, Rect, SpaceId,
    SpaceSnapshot, WindowId, WindowSnapshot,
};

use crate::{Platform, PlatformError};

use sa::{SaClient, SaInfo, OSAX_ATTRIB_ADD_SPACE, OSAX_ATTRIB_MOV_SPACE, OSAX_ATTRIB_REM_SPACE};

const ROVR_APP_MAX: usize = 256;
const ROVR_TITLE_MAX: usize = 512;
const ROVR_BUNDLE_MAX: usize = 256;
const ROVR_CAP_OBSERVE_WINDOWS: u64 = 1 << 0;
const ROVR_CAP_SET_WINDOW_FRAME: u64 = 1 << 1;
const ROVR_CAP_FOCUS_WINDOW: u64 = 1 << 2;
const ROVR_CAP_OBSERVE_SPACES: u64 = 1 << 3;
const ROVR_CAP_MOVE_WINDOW_TO_SPACE: u64 = 1 << 4;
const ROVR_CAP_FOCUS_SPACE: u64 = 1 << 5;

#[repr(C)]
#[derive(Clone, Copy)]
struct BridgeWindow {
    id: u32,
    pid: i32,
    display_id: u32,
    space_id: u64,
    focused: u8,
    minimized: u8,
    fullscreen: u8,
    managed: u8,
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
type SpaceCallback = unsafe extern "C" fn(*const BridgeSpace, *mut c_void);

#[repr(C)]
#[derive(Clone, Copy)]
struct BridgeSpace {
    id: u64,
    display_id: u32,
    type_: i32,
    focused: u8,
    position: u32,
}

extern "C" {
    fn rovr_bridge_init() -> i32;
    fn rovr_bridge_capabilities() -> u64;
    fn rovr_bridge_enumerate_windows(callback: WindowCallback, context: *mut c_void) -> i32;
    fn rovr_bridge_enumerate_displays(callback: DisplayCallback, context: *mut c_void) -> i32;
    fn rovr_bridge_set_window_frame(window_id: u32, x: f64, y: f64, width: f64, height: f64)
        -> i32;
    fn rovr_bridge_focus_window(window_id: u32) -> i32;
    fn rovr_bridge_set_window_minimized(window_id: u32, minimized: i32) -> i32;
    fn rovr_bridge_needs_refresh() -> i32;
    fn rovr_bridge_enumerate_spaces(callback: SpaceCallback, context: *mut c_void) -> i32;
    fn rovr_bridge_move_window_to_space(window_id: u32, space_id: u64) -> i32;
    fn rovr_bridge_focus_space(space_id: u64) -> i32;
    fn rovr_bridge_dock_pid() -> i32;
}

pub struct MacPlatform {
    bridge_capabilities: u64,
    sa: SaClient,
    sa_info: std::cell::RefCell<Option<SaInfo>>,
    last_dock_pid: std::cell::Cell<Option<i32>>,
    last_sa_present: std::cell::Cell<bool>,
}

#[derive(Debug, Clone)]
pub struct SaStatus {
    pub socket_path: std::path::PathBuf,
    pub present: bool,
    pub version: Option<String>,
    pub attribs: Option<u32>,
    pub compatible: bool,
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
        let sa = SaClient::new();
        let sa_info = sa.probe();
        let last_sa_present = std::cell::Cell::new(sa_info.is_some());
        let last_dock_pid = std::cell::Cell::new({
            let pid = unsafe { rovr_bridge_dock_pid() };
            if pid > 0 {
                Some(pid)
            } else {
                None
            }
        });
        Ok(Self {
            bridge_capabilities,
            sa,
            sa_info: std::cell::RefCell::new(sa_info),
            last_dock_pid,
            last_sa_present,
        })
    }

    pub fn sa_status(&self) -> SaStatus {
        let info = self.sa_info.borrow();
        SaStatus {
            socket_path: self.sa.socket_path().clone(),
            present: info.is_some(),
            version: info.as_ref().map(|i| i.version.clone()),
            attribs: info.as_ref().map(|i| i.attribs),
            compatible: info.as_ref().is_some_and(|i| i.is_compatible()),
        }
    }

    fn sa_attribs(&self) -> u32 {
        self.sa_info
            .borrow()
            .as_ref()
            .map(|info| info.attribs)
            .unwrap_or(0)
    }

    fn execute_sa(
        &self,
        op: impl Fn(&SaClient) -> Result<(), sa::SaError>,
    ) -> Result<(), PlatformError> {
        if self.sa_info.borrow().is_none() {
            return Err(PlatformError::Unsupported("scripting_addition"));
        }
        op(&self.sa).map_err(|err| PlatformError::Operation(err.to_string()))
    }

    fn execute_focus_space(&self, space: &SpaceId) -> Result<(), PlatformError> {
        // Prefer the SA's clean focus (no gesture, no animation) when the
        // payload is live; fall back to gesture synthesis otherwise.
        if self.sa_info.borrow().is_some() && self.sa.focus_space(space.0).is_ok() {
            return Ok(());
        }
        let status = unsafe { rovr_bridge_focus_space(space.0) };
        if status == 0 {
            Ok(())
        } else {
            Err(PlatformError::Operation(format!(
                "focus space failed with status {status}"
            )))
        }
    }

    fn snapshot_inner(bridge_capabilities: u64) -> Result<PlatformSnapshot, PlatformError> {
        unsafe extern "C" fn collect_window(window: *const BridgeWindow, context: *mut c_void) {
            if window.is_null() || context.is_null() {
                return;
            }
            let window = *window;
            let windows = &mut *(context as *mut Vec<WindowSnapshot>);
            let bundle_id = c_string(&window.bundle_id);
            let minimized = matches!(window.minimized, 1);
            let fullscreen = matches!(window.fullscreen, 1);
            let managed = match window.managed {
                0 => false,
                1 => true,
                _ => false,
            };
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
                space_id: (window.space_id != 0).then_some(SpaceId(window.space_id)),
                display_id: (window.display_id != 0).then_some(DisplayId(window.display_id)),
                focused: window.focused != 0,
                minimized,
                fullscreen,
                managed,
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
        unsafe extern "C" fn collect_space(space: *const BridgeSpace, context: *mut c_void) {
            if space.is_null() || context.is_null() {
                return;
            }
            let space = *space;
            let spaces = &mut *(context as *mut Vec<SpaceSnapshot>);
            spaces.push(SpaceSnapshot {
                id: SpaceId(space.id),
                display_id: DisplayId(space.display_id),
                label: None,
                focused: space.focused != 0,
                generation: 0,
                position: space.position,
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
        let mut spaces = Vec::new();
        if bridge_capabilities & ROVR_CAP_OBSERVE_SPACES != 0 {
            let space_status = unsafe {
                rovr_bridge_enumerate_spaces(collect_space, &mut spaces as *mut _ as *mut c_void)
            };
            if space_status != 0 {
                return Err(PlatformError::Operation(format!(
                    "space enumeration failed with status {space_status}"
                )));
            }
        }
        Ok(PlatformSnapshot {
            windows,
            spaces,
            displays,
            complete: true,
        })
    }
}

impl Platform for MacPlatform {
    fn capabilities(&self) -> Capabilities {
        let sa_attribs = self.sa_attribs();
        Capabilities {
            observe_windows: self.bridge_capabilities & ROVR_CAP_OBSERVE_WINDOWS != 0,
            set_window_frame: self.bridge_capabilities & ROVR_CAP_SET_WINDOW_FRAME != 0,
            focus_window: self.bridge_capabilities & ROVR_CAP_FOCUS_WINDOW != 0,
            move_window_to_space: self.bridge_capabilities & ROVR_CAP_MOVE_WINDOW_TO_SPACE != 0,
            create_space: sa_attribs & OSAX_ATTRIB_ADD_SPACE != 0,
            destroy_space: sa_attribs & OSAX_ATTRIB_REM_SPACE != 0,
            focus_space: self.bridge_capabilities & ROVR_CAP_FOCUS_SPACE != 0,
            reorder_space: sa_attribs & OSAX_ATTRIB_MOV_SPACE != 0,
            set_window_layer: self.sa_info.borrow().is_some(),
            set_window_sticky: self.sa_info.borrow().is_some(),
            set_window_shadow: self.sa_info.borrow().is_some(),
            set_window_opacity: self.sa_info.borrow().is_some(),
            set_window_scale: self.sa_info.borrow().is_some(),
            scripting_addition: self.sa_info.borrow().is_some(),
        }
    }

    fn snapshot(&mut self) -> Result<PlatformSnapshot, PlatformError> {
        // Hardened snapshot with bounded wait: AX and SkyLight can hang (e.g., app dying mid-call,
        // Dock restart). We run the actual enumeration in a thread and bound it by 2s.
        // This keeps the daemon responsive even when platform hangs.
        let bridge_capabilities = self.bridge_capabilities;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let res = Self::snapshot_inner(bridge_capabilities);
            let _ = tx.send(res);
        });
        match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(res) => res,
            Err(_) => Err(PlatformError::Operation(
                "snapshot timeout (AX/SkyLight hung)".into(),
            )),
        }
    }

    fn execute(&mut self, action: &Action) -> Result<(), PlatformError> {
        let status = match action {
            Action::RefreshAll | Action::RefreshWindow { .. } => return Ok(()),
            Action::SetWindowFrame { window, frame } => unsafe {
                rovr_bridge_set_window_frame(window.0, frame.x, frame.y, frame.width, frame.height)
            },
            Action::FocusWindow { window } => unsafe { rovr_bridge_focus_window(window.0) },
            Action::SetWindowMinimized { window, minimized } => unsafe {
                rovr_bridge_set_window_minimized(window.0, if *minimized { 1 } else { 0 })
            },
            Action::MoveWindowToSpace { window, space } => unsafe {
                rovr_bridge_move_window_to_space(window.0, space.0)
            },
            Action::FocusDirection { .. } => {
                return Err(PlatformError::Unsupported("focus_direction"))
            }
            Action::FocusSpace { space } => {
                return self.execute_focus_space(space);
            }
            Action::CreateSpace { anchor } => {
                return self.execute_sa(|sa| sa.create_space(anchor.0));
            }
            Action::DestroySpace { space } => {
                return self.execute_sa(|sa| sa.destroy_space(space.0));
            }
            Action::MoveSpace { space, after } => {
                return self.execute_sa(|sa| sa.move_space(space.0, after.0));
            }
            Action::SetWindowLayer { window, layer } => {
                return self.execute_sa(|sa| sa.set_layer(window.0, *layer));
            }
            Action::SetWindowSticky { window, sticky } => {
                return self.execute_sa(|sa| sa.set_sticky(window.0, *sticky));
            }
            Action::SetWindowShadow { window, shadow } => {
                return self.execute_sa(|sa| sa.set_shadow(window.0, *shadow));
            }
            Action::SetWindowOpacity {
                window,
                opacity,
                duration_ms,
            } => {
                return self.execute_sa(|sa| {
                    sa.set_opacity(window.0, *opacity as f32, *duration_ms as f32 / 1000.0)
                });
            }
            Action::SetWindowScale { window, x, y, w, h } => {
                return self.execute_sa(|sa| sa.scale_window(window.0, *x, *y, *w, *h));
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

    fn needs_refresh(&self) -> bool {
        let mut needs = unsafe { rovr_bridge_needs_refresh() != 0 };
        // Dock restart detection: pid change means spaces/windows were rebuilt
        let current_dock = {
            let pid = unsafe { rovr_bridge_dock_pid() };
            if pid > 0 {
                Some(pid)
            } else {
                None
            }
        };
        if current_dock != self.last_dock_pid.get() {
            self.last_dock_pid.set(current_dock);
            needs = true;
        }
        // SA payload disconnect/reconnect: re-probe and detect change, update cache
        let current_probe = self.sa.probe();
        let current_sa = current_probe.is_some();
        if current_sa != self.last_sa_present.get() {
            self.last_sa_present.set(current_sa);
            *self.sa_info.borrow_mut() = current_probe;
            needs = true;
        }
        needs
    }
}

fn c_string<const N: usize>(buffer: &[c_char; N]) -> Option<String> {
    if buffer[0] == 0 {
        return None;
    }
    let value = unsafe { CStr::from_ptr(buffer.as_ptr()) };
    Some(value.to_string_lossy().into_owned())
}
