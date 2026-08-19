use std::collections::HashMap;

use rovr_config::Config;
use rovr_layout::{compute, LayoutRequest};
use rovr_types::{DisplayId, LayoutKind, WindowId, WindowSnapshot};

use crate::{DesiredState, ObservedState};

fn is_managed(w: &WindowSnapshot) -> bool {
    !w.fullscreen
}

/// Recompute tiling targets for every observed managed window and write them
/// into `desired.windows[].frame`. Idempotent: rebuilt from `observed` each call.
///
/// `area` is the raw display frame; `rovr_layout::compute` insets it by `padding`
/// internally (lib.rs:34), so we must NOT pre-inset here (that would double-inset).
pub fn apply_layout(config: &Config, observed: &ObservedState, desired: &mut DesiredState) {
    let kind: LayoutKind = config.general.layout;
    let gap = config.general.gap as f64;
    let padding = config.general.padding as f64;

    desired
        .windows
        .retain(|id, _| observed.windows.contains_key(id));
    for id in observed.windows.keys() {
        desired.windows.entry(*id).or_default();
    }

    let mut by_display: HashMap<DisplayId, Vec<WindowId>> = HashMap::new();
    for w in observed.windows.values() {
        if !is_managed(w) {
            if let Some(t) = desired.windows.get_mut(&w.id) {
                t.frame = None;
            }
            continue;
        }
        match (w.space_id, w.space_id.and_then(|s| observed.spaces.get(&s))) {
            (Some(_), Some(space)) => {
                by_display.entry(space.display_id).or_default().push(w.id);
            }
            _ => {
                if let Some(t) = desired.windows.get_mut(&w.id) {
                    t.frame = None;
                }
            }
        }
    }

    for (display_id, window_ids) in by_display {
        let Some(display) = observed.displays.get(&display_id) else {
            continue;
        };
        let area = display.frame; // compute() insets this by padding ONCE
        let request = LayoutRequest {
            area,
            windows: &window_ids,
            gap,
            padding,
            split_ratio: 0.5,
        };
        if let Ok(placements) = compute(kind, request) {
            for p in placements {
                if let Some(t) = desired.windows.get_mut(&p.window) {
                    t.frame = Some(p.frame);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rovr_config::Config;
    use rovr_types::{
        DisplayId, DisplaySnapshot, LayoutKind, ProcessId, Rect, SpaceId, SpaceSnapshot, WindowId,
        WindowSnapshot,
    };

    use crate::{DesiredState, ObservedState};

    /// M3a-2: apply_layout tiles managed windows and writes frames into desired.
    /// The exact-bbox check FAILS if a double-inset bug regresses (would give
    /// min 24 instead of 12).
    #[test]
    fn m3a2_apply_layout_single_inset() {
        let mut config = Config::default();
        config.general.layout = LayoutKind::Bsp;
        config.general.padding = 12; // NONZERO, exposes double-inset
        config.general.gap = 8;

        let mut observed = ObservedState::default();
        observed.displays.insert(
            DisplayId(1),
            DisplaySnapshot {
                id: DisplayId(1),
                frame: Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 1440.0,
                    height: 900.0,
                },
                label: None,
                focused: false,
                generation: 0,
            },
        );
        observed.spaces.insert(
            SpaceId(11),
            SpaceSnapshot {
                id: SpaceId(11),
                display_id: DisplayId(1),
                label: None,
                focused: false,
                generation: 0,
                position: 0,
            },
        );

        let mk = |id: u32, fullscreen: bool| WindowSnapshot {
            id: WindowId(id),
            pid: ProcessId(1),
            app: String::new(),
            bundle_id: None,
            title: String::new(),
            frame: Rect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 100.0,
            },
            space_id: Some(SpaceId(11)),
            display_id: Some(DisplayId(1)),
            focused: false,
            minimized: false,
            fullscreen,
            managed: true,
            generation: 0,
        };
        observed.windows.insert(WindowId(1), mk(1, false));
        observed.windows.insert(WindowId(2), mk(2, false));
        observed.windows.insert(WindowId(3), mk(3, false));
        observed.windows.insert(WindowId(9), mk(9, true)); // fullscreen

        let mut desired = DesiredState::default();
        apply_layout(&config, &observed, &mut desired);

        // Managed windows are tiled.
        for id in [WindowId(1), WindowId(2), WindowId(3)] {
            assert!(
                desired.windows.get(&id).and_then(|t| t.frame).is_some(),
                "managed window {id:?} must be tiled"
            );
        }
        // Fullscreen window is left alone.
        assert_eq!(
            desired.windows.get(&WindowId(9)).and_then(|t| t.frame),
            None,
            "fullscreen window must not be tiled"
        );

        // Bounding box of the 3 managed frames must equal inset(display.frame, 12)
        // == (min x 12, max x 1428, min y 12, max y 888).
        let frames: Vec<Rect> = [WindowId(1), WindowId(2), WindowId(3)]
            .iter()
            .map(|id| desired.windows.get(id).unwrap().frame.unwrap())
            .collect();
        let min_x = frames.iter().map(|f| f.x).fold(f64::INFINITY, f64::min);
        let max_x = frames
            .iter()
            .map(|f| f.x + f.width)
            .fold(f64::NEG_INFINITY, f64::max);
        let min_y = frames.iter().map(|f| f.y).fold(f64::INFINITY, f64::min);
        let max_y = frames
            .iter()
            .map(|f| f.y + f.height)
            .fold(f64::NEG_INFINITY, f64::max);

        let eps = 1.0;
        assert!((min_x - 12.0).abs() <= eps, "min_x={min_x} expected 12");
        assert!((max_x - 1428.0).abs() <= eps, "max_x={max_x} expected 1428");
        assert!((min_y - 12.0).abs() <= eps, "min_y={min_y} expected 12");
        assert!((max_y - 888.0).abs() <= eps, "max_y={max_y} expected 888");
    }
}
