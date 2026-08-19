use std::collections::HashMap;

use rovr_config::{Config, RuleConfig};
use rovr_layout::{compute, LayoutRequest};
use rovr_types::{DisplayId, LayoutKind, Rect, SpaceId, WindowId, WindowSnapshot};

use crate::layout_state::{Axis, Layouts, Orientation};
use crate::{DesiredState, ObservedState};

/// A window is tileable when the WM manages it and it is not fullscreen.
/// `managed` is false for floating windows (yabai semantics). The snapshot
/// bridge currently hardcodes `managed: true` and `fullscreen: false`, so this
/// only differentiates once the bridge reports them truthfully.
fn is_tileable(w: &WindowSnapshot) -> bool {
    w.managed && !w.fullscreen
}
/// A window floats when some `floating == Some(true)` rule matches it on every
/// field it specifies (app / title / workspace). Used to exclude windows from
/// tiling without relying on the bridge's hardcoded `managed` flag.
fn matches_float_rule(w: &WindowSnapshot, rules: &[RuleConfig], observed: &ObservedState) -> bool {
    for rule in rules {
        let Some(true) = rule.floating else { continue };
        let app_ok = match &rule.app {
            Some(app) => w.bundle_id.as_deref() == Some(app.as_str()),
            None => true,
        };
        let title_ok = match &rule.title {
            Some(title) => w.title.contains(title),
            None => true,
        };
        let workspace_ok = match &rule.workspace {
            Some(ws) => {
                w.space_id
                    .and_then(|sid| observed.spaces.get(&sid))
                    .and_then(|s| s.label.as_deref())
                    == Some(ws.as_str())
            }
            None => true,
        };
        if app_ok && title_ok && workspace_ok {
            return true;
        }
    }
    false
}
/// Resolve the layout kind for a space. A named workspace (`WorkspaceConfig`)
/// whose `name` matches the space's `label` overrides the global default
/// layout. Falls back to `config.general.layout` when there is no label or no
/// matching workspace — never panics.
fn resolve_layout(config: &Config, space_id: SpaceId, observed: &ObservedState) -> LayoutKind {
    if let Some(label) = observed
        .spaces
        .get(&space_id)
        .and_then(|s| s.label.as_deref())
    {
        if let Some(ws) = config.workspaces.iter().find(|w| w.name == label) {
            return ws.layout;
        }
    }
    config.general.layout
}

/// Recompute tiling targets for every observed managed window and write them
/// into `desired.windows[].frame`. Idempotent: rebuilt from `observed` each call.
///
/// `area` is the raw display frame; `rovr_layout::compute` insets it by `padding`
/// internally (lib.rs:34), so we must NOT pre-inset here (that would double-inset).
///
/// Layout is computed per Space (each macOS Space tiles independently within its
/// display's area). For BSP, `layouts` supplies a per-Space orientation: `reversed`
/// reorders windows and `axis == Horizontal` is applied as an area transpose so
/// `compute` (kept pure) still yields the right frames.
pub fn apply_layout(
    config: &Config,
    observed: &ObservedState,
    desired: &mut DesiredState,
    layouts: &Layouts,
) {
    let gap = config.general.gap as f64;
    let padding = config.general.padding as f64;

    desired
        .windows
        .retain(|id, _| observed.windows.contains_key(id));
    for id in observed.windows.keys() {
        desired.windows.entry(*id).or_default();
    }

    let mut by_space: HashMap<SpaceId, (DisplayId, Rect, Vec<WindowId>)> = HashMap::new();
    for w in observed.windows.values() {
        if !is_tileable(w) || matches_float_rule(w, &config.rules, observed) {
            if let Some(t) = desired.windows.get_mut(&w.id) {
                t.frame = None;
            }
            continue;
        }
        let Some(space) = w.space_id.and_then(|s| observed.spaces.get(&s)) else {
            if let Some(t) = desired.windows.get_mut(&w.id) {
                t.frame = None;
            }
            continue;
        };
        let Some(display) = observed.displays.get(&space.display_id) else {
            if let Some(t) = desired.windows.get_mut(&w.id) {
                t.frame = None;
            }
            continue;
        };
        by_space
            .entry(space.id)
            .or_insert((space.display_id, display.frame, Vec::new()))
            .2
            .push(w.id);
    }

    for (space_id, (_display_id, area, window_ids)) in by_space {
        let kind = resolve_layout(config, space_id, observed);
        // Orientation only affects BSP; other layouts ignore it.
        let orientation = if kind == LayoutKind::Bsp {
            layouts
                .get(&space_id)
                .map(|l| l.orientation)
                .unwrap_or_default()
        } else {
            Orientation::default()
        };
        let mut wids = window_ids;
        if kind == LayoutKind::Bsp && orientation.reversed {
            wids.reverse();
        }
        // Horizontal axis: transpose the area so the pure vertical split in
        // compute becomes a top/bottom split, then transpose frames back.
        let (area2, swap) = if kind == LayoutKind::Bsp && orientation.axis == Axis::Horizontal {
            (
                Rect {
                    x: area.y,
                    y: area.x,
                    width: area.height,
                    height: area.width,
                },
                true,
            )
        } else {
            (area, false)
        };
        let request = LayoutRequest {
            area: area2,
            windows: &wids,
            gap,
            padding,
            split_ratio: 0.5,
        };
        if let Ok(placements) = compute(kind, request) {
            for p in placements {
                let frame = if swap {
                    Rect {
                        x: p.frame.y,
                        y: p.frame.x,
                        width: p.frame.height,
                        height: p.frame.width,
                    }
                } else {
                    p.frame
                };
                if let Some(t) = desired.windows.get_mut(&p.window) {
                    t.frame = Some(frame);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout_state::Layouts;
    use rovr_config::{Config, WorkspaceConfig};
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
        apply_layout(&config, &observed, &mut desired, &Layouts::new());

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

    /// M3a-2b: a floating window (managed = false, not fullscreen) is skipped.
    /// Guards `is_tileable` from tiling floating windows once the snapshot
    /// bridge reports `managed` truthfully (currently hardcoded true).
    #[test]
    fn m3a2b_floating_window_skipped() {
        let mut config = Config::default();
        config.general.layout = LayoutKind::Bsp;
        config.general.padding = 10;
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

        let mk = |use_managed: bool| WindowSnapshot {
            id: WindowId(if use_managed { 1 } else { 7 }),
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
            fullscreen: false,
            managed: use_managed,
            generation: 0,
        };
        observed.windows.insert(WindowId(1), mk(true)); // managed, non-fullscreen
        observed.windows.insert(WindowId(7), mk(false)); // floating

        let mut desired = DesiredState::default();
        apply_layout(&config, &observed, &mut desired, &Layouts::new());

        assert!(
            desired
                .windows
                .get(&WindowId(1))
                .and_then(|t| t.frame)
                .is_some(),
            "managed window must be tiled"
        );
        assert_eq!(
            desired.windows.get(&WindowId(7)).and_then(|t| t.frame),
            None,
            "floating (managed = false) window must not be tiled"
        );
    }
    /// M3c: a window whose bundle id matches a `float = true` rule is skipped
    /// even though the bridge reports it as managed.
    #[test]
    fn m3c_app_rule_floats() {
        let config = Config {
            rules: vec![RuleConfig {
                app: Some("com.apple.Safari".into()),
                floating: Some(true),
                ..Default::default()
            }],
            ..Default::default()
        };

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

        let mk = |id: u32, bundle: &str| WindowSnapshot {
            id: WindowId(id),
            pid: ProcessId(1),
            app: String::new(),
            bundle_id: Some(bundle.to_string()),
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
            fullscreen: false,
            managed: true,
            generation: 0,
        };
        observed
            .windows
            .insert(WindowId(1), mk(1, "com.apple.Safari")); // matches rule
        observed
            .windows
            .insert(WindowId(2), mk(2, "com.apple.Mail")); // no match

        let mut desired = DesiredState::default();
        apply_layout(&config, &observed, &mut desired, &Layouts::new());

        assert_eq!(
            desired.windows.get(&WindowId(1)).and_then(|t| t.frame),
            None,
            "Safari matches float rule -> must not be tiled"
        );
        assert!(
            desired
                .windows
                .get(&WindowId(2))
                .and_then(|t| t.frame)
                .is_some(),
            "Mail does not match -> must be tiled"
        );
    }

    /// M3c: a title-substring match floats the window.
    #[test]
    fn m3c_title_rule_floats() {
        let config = Config {
            rules: vec![RuleConfig {
                title: Some("Modal".into()),
                floating: Some(true),
                ..Default::default()
            }],
            ..Default::default()
        };

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

        let mk = |id: u32, title: &str| WindowSnapshot {
            id: WindowId(id),
            pid: ProcessId(1),
            app: String::new(),
            bundle_id: None,
            title: title.to_string(),
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
            fullscreen: false,
            managed: true,
            generation: 0,
        };
        observed.windows.insert(WindowId(1), mk(1, "Login Modal")); // matches
        observed.windows.insert(WindowId(2), mk(2, "Editor")); // no match

        let mut desired = DesiredState::default();
        apply_layout(&config, &observed, &mut desired, &Layouts::new());

        assert_eq!(
            desired.windows.get(&WindowId(1)).and_then(|t| t.frame),
            None,
            "title contains 'Modal' -> must not be tiled"
        );
        assert!(
            desired
                .windows
                .get(&WindowId(2))
                .and_then(|t| t.frame)
                .is_some(),
            "title without 'Modal' -> must be tiled"
        );
    }

    /// M3c: empty rules leave managed non-fullscreen windows tiling as before.
    #[test]
    fn m3c_no_rule_tiles() {
        let config = Config {
            rules: vec![],
            ..Default::default()
        };

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

        let mk = |id: u32| WindowSnapshot {
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
            fullscreen: false,
            managed: true,
            generation: 0,
        };
        observed.windows.insert(WindowId(1), mk(1));
        observed.windows.insert(WindowId(2), mk(2));

        let mut desired = DesiredState::default();
        apply_layout(&config, &observed, &mut desired, &Layouts::new());

        assert!(
            desired
                .windows
                .get(&WindowId(1))
                .and_then(|t| t.frame)
                .is_some(),
            "no rules -> managed window must be tiled"
        );
        assert!(
            desired
                .windows
                .get(&WindowId(2))
                .and_then(|t| t.frame)
                .is_some(),
            "no rules -> managed window must be tiled"
        );
    }
    /// M3d: a Space whose label matches a named workspace uses that
    /// workspace's layout; a non-matching label falls back to global.
    #[test]
    fn m3d_named_workspace_overrides_layout() {
        let mut config = Config::default();
        config.general.layout = LayoutKind::Bsp; // global
        config.workspaces = vec![WorkspaceConfig {
            name: "dev".into(),
            layout: LayoutKind::Stack,
            display: None,
            persistent: false,
        }];

        let mut observed = ObservedState::default();
        observed.spaces.insert(
            SpaceId(11),
            SpaceSnapshot {
                id: SpaceId(11),
                display_id: DisplayId(1),
                label: Some("dev".into()),
                focused: false,
                generation: 0,
                position: 0,
            },
        );
        observed.spaces.insert(
            SpaceId(12),
            SpaceSnapshot {
                id: SpaceId(12),
                display_id: DisplayId(1),
                label: Some("other".into()),
                focused: false,
                generation: 0,
                position: 0,
            },
        );

        assert_eq!(
            resolve_layout(&config, SpaceId(11), &observed),
            LayoutKind::Stack,
            "labeled 'dev' space must use the named workspace layout"
        );
        assert_eq!(
            resolve_layout(&config, SpaceId(12), &observed),
            LayoutKind::Bsp,
            "non-matching label falls back to global"
        );
    }

    /// M3d: an unlabeled space always uses the global layout even when a
    /// named workspace exists.
    #[test]
    fn m3d_unlabeled_space_uses_global() {
        let mut config = Config::default();
        config.general.layout = LayoutKind::Stack; // global
        config.workspaces = vec![WorkspaceConfig {
            name: "dev".into(),
            layout: LayoutKind::Bsp,
            display: None,
            persistent: false,
        }];

        let mut observed = ObservedState::default();
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

        assert_eq!(
            resolve_layout(&config, SpaceId(11), &observed),
            LayoutKind::Stack,
            "unlabeled space uses global layout"
        );
    }
}
