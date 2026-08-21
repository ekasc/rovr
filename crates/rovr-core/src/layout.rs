use std::collections::HashMap;

use rovr_config::{CompiledRule, Config, ScratchpadConfig};
use rovr_layout::{compute, LayoutRequest};
use rovr_types::{DisplayId, LayoutKind, Rect, SpaceId, WindowId, WindowSnapshot};

use crate::layout_state::{Layouts, ScratchpadState};
use crate::workspace::WorkspaceRegistry;
use crate::{DesiredState, ObservedState};
use rovr_layout_plugin::{PluginRequest, Registry as PluginRegistry};

/// A window is tileable when the WM manages it and it is not fullscreen
/// and not minimized. `managed` is false for floating/system windows.
/// `minimized`/`fullscreen` are observed via AX; unknown is treated as not
/// tileable by the reconciliation layer (see is_tileable guards).
fn is_tileable(w: &WindowSnapshot) -> bool {
    w.managed == rovr_types::ObservedBool::Yes
        && w.fullscreen == rovr_types::ObservedBool::No
        && w.minimized == rovr_types::ObservedBool::No
}
/// Blocker 10: rule matching uses the COMPILED regexes from config load —
/// never equality/substring checks that would diverge from validation.
fn window_matches_rule(
    w: &WindowSnapshot,
    rule: &CompiledRule,
    observed: &ObservedState,
    workspaces: &WorkspaceRegistry,
) -> bool {
    let app_ok = match &rule.app {
        Some(re) => {
            let bundle_hit = w.bundle_id.as_deref().is_some_and(|b| re.is_match(b));
            let app_hit = re.is_match(&w.app);
            bundle_hit || app_hit
        }
        None => true,
    };
    let title_ok = match &rule.title {
        Some(re) => re.is_match(&w.title),
        None => true,
    };
    let workspace_ok = match &rule.workspace {
        Some(ws) => {
            // Match against logical workspace name backing the window's space
            let ws_name = w
                .space_id
                .and_then(|sid| workspaces.name_for_space(sid))
                .or_else(|| {
                    w.space_id
                        .and_then(|sid| observed.spaces.get(&sid))
                        .and_then(|s| s.label.as_deref())
                });
            ws_name == Some(ws.as_str())
        }
        None => true,
    };
    app_ok && title_ok && workspace_ok
}

/// A window floats when some `floating == Some(true)` rule matches it.
fn matches_float_rule(
    w: &WindowSnapshot,
    rules: &[CompiledRule],
    observed: &ObservedState,
    workspaces: &WorkspaceRegistry,
) -> bool {
    for rule in rules {
        let Some(true) = rule.floating else { continue };
        if window_matches_rule(w, rule, observed, workspaces) {
            return true;
        }
    }
    false
}

fn target_workspace_for_window(
    w: &WindowSnapshot,
    rules: &[CompiledRule],
    observed: &ObservedState,
    workspaces: &WorkspaceRegistry,
) -> Option<SpaceId> {
    for rule in rules {
        if let Some(target) = &rule.target_workspace {
            if window_matches_rule(w, rule, observed, workspaces) {
                if let Some(sid) = workspaces.backing_for(target) {
                    return Some(sid);
                }
            }
        }
    }
    None
}
/// Resolve the layout kind for a space. Logical workspaces own the name:
/// if `space_id` is backing a logical workspace, that workspace's layout
/// overrides the global. Falls back to legacy `space.label == workspace.name`
/// for back-compat, then global.
fn resolve_layout(
    config: &Config,
    space_id: SpaceId,
    observed: &ObservedState,
    workspaces: &WorkspaceRegistry,
) -> LayoutKind {
    if let Some(name) = workspaces.name_for_space(space_id) {
        if let Some(ws) = config.workspaces.iter().find(|w| w.name == name) {
            return ws.layout;
        }
    }
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
/// True when the window matches at least one scratchpad that is currently
/// open. A pad matches on `app` (exact bundle id) and/or `title` (substring);
/// `None` fields wildcard. Closed pads are ignored, so a window matching both
/// an open and a closed pad still floats (no config-order dependence).
fn matches_open_scratchpad(
    w: &WindowSnapshot,
    pads: &[ScratchpadConfig],
    state: &ScratchpadState,
) -> bool {
    pads.iter().any(|pad| {
        let app_ok = match &pad.app {
            Some(app) => w.bundle_id.as_deref() == Some(app.as_str()),
            None => true,
        };
        let title_ok = match &pad.title {
            Some(title) => w.title.contains(title),
            None => true,
        };
        app_ok && title_ok && state.is_open(&pad.name)
    })
}

fn inset_area(area: Rect, padding: f64) -> Option<Rect> {
    let w = area.width - padding * 2.0;
    let h = area.height - padding * 2.0;
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    Some(Rect {
        x: area.x + padding,
        y: area.y + padding,
        width: w,
        height: h,
    })
}

/// Recompute tiling targets for every observed tileable window and write them
/// into `desired.windows[].frame`. For BSP the persistent per-space `BspTree`
/// is synced with observed tileable windows (insert/remove) so topology
/// is preserved across reconcile cycles and not derived from enumeration order.
/// Non-BSP layouts remain stateless but use deterministic sorted order.
///
/// `rules` are the config's COMPILED rules (see `Config::compile_rules`) —
/// built once at load/reload, never per cycle.
#[allow(clippy::too_many_arguments)] // pure function over engine state slices; a struct would obscure the borrow split
pub fn apply_layout(
    config: &Config,
    observed: &ObservedState,
    desired: &mut DesiredState,
    layouts: &mut Layouts,
    workspaces: &WorkspaceRegistry,
    plugins: &PluginRegistry,
    scratchpads: &ScratchpadState,
    rules: &[CompiledRule],
) {
    let gap = config.general.gap as f64;
    let padding = config.general.padding as f64;

    desired
        .windows
        .retain(|id, _| observed.windows.contains_key(id));
    for id in observed.windows.keys() {
        desired.windows.entry(*id).or_default();
    }

    // Blocker 9: rule-derived desired state is rebuilt from scratch every
    // cycle. `desired.space` is exclusively rule-owned (manual moves are
    // one-shot actions and never stored here), so clearing it first means a
    // rule that stops matching stops pulling the window immediately.
    for target in desired.windows.values_mut() {
        target.space = None;
    }

    let mut by_space: HashMap<SpaceId, (DisplayId, Rect, Vec<WindowId>)> = HashMap::new();
    for w in observed.windows.values() {
        // Rule-driven workspace move: evaluated every snapshot, deterministic.
        // This writes desired.space so reconcile will move the window.
        if let Some(target) = target_workspace_for_window(w, rules, observed, workspaces) {
            if observed.spaces.contains_key(&target) {
                if let Some(t) = desired.windows.get_mut(&w.id) {
                    t.space = Some(target);
                }
            }
        }
        let is_floating = !is_tileable(w)
            || matches_float_rule(w, rules, observed, workspaces)
            || matches_open_scratchpad(w, &config.scratchpads, scratchpads);
        if is_floating {
            if let Some(t) = desired.windows.get_mut(&w.id) {
                t.frame = None;
            }
            // Still honor target workspace move even for floating windows
            // (desired.space already set above); continue without tiling.
            continue;
        }
        // Determine effective space for tiling: use rule target if present, else current
        let effective_space_id = desired
            .windows
            .get(&w.id)
            .and_then(|t| t.space)
            .or(w.space_id)
            .and_then(|sid| observed.spaces.get(&sid).map(|s| s.id))
            .or(w.space_id);
        let Some(space) = effective_space_id.and_then(|sid| observed.spaces.get(&sid)) else {
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
        // Plugin layout: check per-workspace override then general
        let plugin_name: Option<String> = workspaces
            .name_for_space(space_id)
            .and_then(|n| config.workspaces.iter().find(|w| w.name == n))
            .and_then(|w| w.plugin.clone())
            .or_else(|| config.general.plugin.clone());
        if let Some(name) = plugin_name {
            if let Some(plugin) = plugins.get(&name) {
                if let Some(inset) = inset_area(area, padding) {
                    let req = PluginRequest {
                        area: inset,
                        windows: window_ids.clone(),
                        gap,
                        padding: 0.0,
                    };
                    match plugin.compute(&req) {
                        Ok(placements) => {
                            // Blocker 12: validate the ENTIRE result before
                            // applying any of it. Invalid output is discarded
                            // wholesale and the built-in layout takes over.
                            if let Err(reason) =
                                rovr_layout_plugin::validate_placements(&req, &placements)
                            {
                                tracing::warn!(plugin = %name, %reason, "invalid plugin output discarded, falling back to built-in layout");
                            } else {
                                for p in placements {
                                    if let Some(t) = desired.windows.get_mut(&p.window) {
                                        t.frame = Some(p.frame);
                                    }
                                }
                                continue;
                            }
                        }
                        Err(e) => {
                            tracing::warn!(plugin=%name, error=%e, "plugin compute failed, falling back to built-in");
                        }
                    }
                }
            } else {
                tracing::warn!(plugin=%name, "plugin not found, falling back");
            }
        }
        let kind = resolve_layout(config, space_id, observed, workspaces);
        if kind == LayoutKind::Bsp {
            // Persistent BSP: sync tree with observed tileable set for this space.
            let state = layouts.entry(space_id).or_default();
            let set: std::collections::HashSet<WindowId> = window_ids.iter().copied().collect();
            state.bsp.sync_with_windows(&set);
            // Inset area before BSP placement (compute() did this internally;
            // tree placements expect already-inset area).
            if let Some(inset) = inset_area(area, padding) {
                let placements = state.bsp.placements(inset, gap);
                for (win, frame) in placements {
                    if let Some(t) = desired.windows.get_mut(&win) {
                        t.frame = Some(frame);
                    }
                }
            }
            // BSP tree is authoritative; orientation is retained only for
            // diagnostics/back-compat and is not driven from the tree here.
        } else {
            let mut wids = window_ids;
            wids.sort_unstable();
            let request = LayoutRequest {
                area,
                windows: &wids,
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
    // Clean up BSP trees for spaces with no tileable windows (optional: keep
    // empty tree). We retain the tree even when empty so ratio/topology
    // survives temporary hibernation; sync already removed missing windows.
    // No extra cleanup needed.
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout_state::Layouts;
    use rovr_config::{Config, RuleConfig, WorkspaceConfig};
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
            // convert bool to ObservedBool
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
            minimized: rovr_types::ObservedBool::No,
            fullscreen: if fullscreen {
                rovr_types::ObservedBool::Yes
            } else {
                rovr_types::ObservedBool::No
            },
            managed: rovr_types::ObservedBool::Yes,
            generation: 0,
        };
        observed.windows.insert(WindowId(1), mk(1, false));
        observed.windows.insert(WindowId(2), mk(2, false));
        observed.windows.insert(WindowId(3), mk(3, false));
        observed.windows.insert(WindowId(9), mk(9, true)); // fullscreen

        let mut desired = DesiredState::default();
        apply_layout(
            &config,
            &observed,
            &mut desired,
            &mut Layouts::new(),
            &crate::workspace::WorkspaceRegistry::default(),
            &rovr_layout_plugin::Registry::new(),
            &ScratchpadState::new(),
            &[],
        );

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
            minimized: rovr_types::ObservedBool::No,
            fullscreen: rovr_types::ObservedBool::No,
            managed: if use_managed {
                rovr_types::ObservedBool::Yes
            } else {
                rovr_types::ObservedBool::No
            },
            generation: 0,
        };
        observed.windows.insert(WindowId(1), mk(true)); // managed, non-fullscreen
        observed.windows.insert(WindowId(7), mk(false)); // floating

        let mut desired = DesiredState::default();
        apply_layout(
            &config,
            &observed,
            &mut desired,
            &mut Layouts::new(),
            &crate::workspace::WorkspaceRegistry::default(),
            &rovr_layout_plugin::Registry::new(),
            &ScratchpadState::new(),
            &[],
        );

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

    /// Blocker 13: an eligibility-critical property reported as Unknown must
    /// NOT be tiled — the policy is conservative and never invents certainty.
    #[test]
    fn blocker13_unknown_state_is_not_tiled() {
        let config = Config::default();
        let mut observed = ObservedState::default();
        observed.displays.insert(
            DisplayId(1),
            DisplaySnapshot {
                id: DisplayId(1),
                frame: Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 1000.0,
                    height: 800.0,
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
        let mk = |id: u32,
                  managed: rovr_types::ObservedBool,
                  fullscreen: rovr_types::ObservedBool,
                  minimized: rovr_types::ObservedBool| WindowSnapshot {
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
            minimized,
            fullscreen,
            managed,
            generation: 0,
        };
        observed.windows.insert(
            WindowId(1),
            mk(
                1,
                rovr_types::ObservedBool::Unknown,
                rovr_types::ObservedBool::No,
                rovr_types::ObservedBool::No,
            ),
        );
        observed.windows.insert(
            WindowId(2),
            mk(
                2,
                rovr_types::ObservedBool::Yes,
                rovr_types::ObservedBool::Unknown,
                rovr_types::ObservedBool::No,
            ),
        );
        observed.windows.insert(
            WindowId(3),
            mk(
                3,
                rovr_types::ObservedBool::Yes,
                rovr_types::ObservedBool::No,
                rovr_types::ObservedBool::Unknown,
            ),
        );

        let mut desired = DesiredState::default();
        apply_layout(
            &config,
            &observed,
            &mut desired,
            &mut Layouts::new(),
            &crate::workspace::WorkspaceRegistry::default(),
            &rovr_layout_plugin::Registry::new(),
            &ScratchpadState::new(),
            &[],
        );

        for id in [WindowId(1), WindowId(2), WindowId(3)] {
            assert_eq!(
                desired.windows.get(&id).and_then(|t| t.frame),
                None,
                "window {id:?} with an unknown eligibility property must not be tiled"
            );
        }
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
            minimized: rovr_types::ObservedBool::No,
            fullscreen: rovr_types::ObservedBool::No,
            managed: rovr_types::ObservedBool::Yes,
            generation: 0,
        };
        observed
            .windows
            .insert(WindowId(1), mk(1, "com.apple.Safari")); // matches rule
        observed
            .windows
            .insert(WindowId(2), mk(2, "com.apple.Mail")); // no match

        let mut desired = DesiredState::default();
        apply_layout(
            &config,
            &observed,
            &mut desired,
            &mut Layouts::new(),
            &crate::workspace::WorkspaceRegistry::default(),
            &rovr_layout_plugin::Registry::new(),
            &ScratchpadState::new(),
            &config.compile_rules().unwrap(),
        );

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
            minimized: rovr_types::ObservedBool::No,
            fullscreen: rovr_types::ObservedBool::No,
            managed: rovr_types::ObservedBool::Yes,
            generation: 0,
        };
        observed.windows.insert(WindowId(1), mk(1, "Login Modal")); // matches
        observed.windows.insert(WindowId(2), mk(2, "Editor")); // no match

        let mut desired = DesiredState::default();
        apply_layout(
            &config,
            &observed,
            &mut desired,
            &mut Layouts::new(),
            &crate::workspace::WorkspaceRegistry::default(),
            &rovr_layout_plugin::Registry::new(),
            &ScratchpadState::new(),
            &config.compile_rules().unwrap(),
        );

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
            minimized: rovr_types::ObservedBool::No,
            fullscreen: rovr_types::ObservedBool::No,
            managed: rovr_types::ObservedBool::Yes,
            generation: 0,
        };
        observed.windows.insert(WindowId(1), mk(1));
        observed.windows.insert(WindowId(2), mk(2));

        let mut desired = DesiredState::default();
        apply_layout(
            &config,
            &observed,
            &mut desired,
            &mut Layouts::new(),
            &crate::workspace::WorkspaceRegistry::default(),
            &rovr_layout_plugin::Registry::new(),
            &ScratchpadState::new(),
            &[],
        );

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

    // ---- Blockers 9 + 10: rule-derived desired state and regex matching ----

    /// Builds an observed state with one display, two spaces (11 backing the
    /// logical workspace "chat"), and one managed window on space 12.
    fn observed_for_rule_tests() -> ObservedState {
        let mut observed = ObservedState::default();
        observed.displays.insert(
            DisplayId(1),
            DisplaySnapshot {
                id: DisplayId(1),
                frame: Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 1000.0,
                    height: 800.0,
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
        observed.spaces.insert(
            SpaceId(12),
            SpaceSnapshot {
                id: SpaceId(12),
                display_id: DisplayId(1),
                label: None,
                focused: false,
                generation: 0,
                position: 1,
            },
        );
        observed.windows.insert(
            WindowId(1),
            WindowSnapshot {
                id: WindowId(1),
                pid: ProcessId(1),
                app: "Slack".into(),
                bundle_id: Some("com.tinyspeck.slackmacgap".into()),
                title: "deploy bot".into(),
                frame: Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 100.0,
                    height: 100.0,
                },
                space_id: Some(SpaceId(12)),
                display_id: Some(DisplayId(1)),
                focused: false,
                minimized: rovr_types::ObservedBool::No,
                fullscreen: rovr_types::ObservedBool::No,
                managed: rovr_types::ObservedBool::Yes,
                generation: 0,
            },
        );
        observed
    }

    /// Blocker 9: while a rule matches, desired.space points at the rule's
    /// target workspace; once the rule stops matching (title changed), the
    /// rule-derived target must DISAPPEAR in the next cycle — never stick.
    #[test]
    fn blocker9_rule_derived_space_clears_when_rule_stops_matching() {
        let config = Config {
            rules: vec![RuleConfig {
                app: Some("^com\\.tinyspeck\\.slackmacgap$".into()),
                title: Some("deploy bot".into()),
                target_workspace: Some("chat".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let rules = config.compile_rules().unwrap();
        let mut workspaces = crate::workspace::WorkspaceRegistry::default();
        workspaces.0.insert(
            "chat".into(),
            crate::workspace::WorkspaceState {
                name: "chat".into(),
                persistent: true,
                backing_space: Some(SpaceId(11)),
                desired_display: None,
                ordinal: 0,
                last_position: None,
            },
        );

        // Cycle 1: rule matches -> desired.space = chat's backing space.
        let mut desired = DesiredState::default();
        apply_layout(
            &config,
            &observed_for_rule_tests(),
            &mut desired,
            &mut Layouts::new(),
            &workspaces,
            &rovr_layout_plugin::Registry::new(),
            &ScratchpadState::new(),
            &rules,
        );
        assert_eq!(
            desired.windows[&WindowId(1)].space,
            Some(SpaceId(11)),
            "matching rule must set rule-derived workspace target"
        );

        // Cycle 2: title changed so the rule no longer matches.
        let mut observed = observed_for_rule_tests();
        observed.windows.get_mut(&WindowId(1)).unwrap().title = "standup notes".into();
        let mut desired2 = DesiredState::default();
        apply_layout(
            &config,
            &observed,
            &mut desired2,
            &mut Layouts::new(),
            &workspaces,
            &rovr_layout_plugin::Registry::new(),
            &ScratchpadState::new(),
            &rules,
        );
        assert_eq!(
            desired2.windows[&WindowId(1)].space,
            None,
            "rule-derived workspace target must disappear when the rule stops matching"
        );
    }

    /// Blocker 10: exact regex anchoring is honored at runtime — a pattern
    /// `^Finder$` must NOT match "Finder Helper".
    #[test]
    fn blocker10_exact_regex_is_enforced_at_runtime() {
        let config = Config {
            rules: vec![RuleConfig {
                app: Some(r"^com\.tinyspeck\.slackmacgap$".into()),
                floating: Some(true),
                ..Default::default()
            }],
            ..Default::default()
        };
        let rules = config.compile_rules().unwrap();
        let mut desired = DesiredState::default();
        apply_layout(
            &config,
            &observed_for_rule_tests(),
            &mut desired,
            &mut Layouts::new(),
            &crate::workspace::WorkspaceRegistry::default(),
            &rovr_layout_plugin::Registry::new(),
            &ScratchpadState::new(),
            &rules,
        );
        assert_eq!(
            desired.windows.get(&WindowId(1)).and_then(|t| t.frame),
            None,
            "exact regex hit must float the window"
        );
    }

    /// Blocker 10: alternation patterns match any alternative at runtime.
    #[test]
    fn blocker10_alternation_regex_matches() {
        let config = Config {
            rules: vec![RuleConfig {
                title: Some("Preferences|Settings".into()),
                floating: Some(true),
                ..Default::default()
            }],
            ..Default::default()
        };
        let rules = config.compile_rules().unwrap();

        for title in ["Preferences", "Settings"] {
            let mut observed = observed_for_rule_tests();
            observed.windows.get_mut(&WindowId(1)).unwrap().title = title.into();
            let mut desired = DesiredState::default();
            apply_layout(
                &config,
                &observed,
                &mut desired,
                &mut Layouts::new(),
                &crate::workspace::WorkspaceRegistry::default(),
                &rovr_layout_plugin::Registry::new(),
                &ScratchpadState::new(),
                &rules,
            );
            assert_eq!(
                desired.windows.get(&WindowId(1)).and_then(|t| t.frame),
                None,
                "alternation must match {title}"
            );
        }

        // Non-match: unrelated title stays tiled.
        let mut observed = observed_for_rule_tests();
        observed.windows.get_mut(&WindowId(1)).unwrap().title = "Editor".into();
        let mut desired = DesiredState::default();
        apply_layout(
            &config,
            &observed,
            &mut desired,
            &mut Layouts::new(),
            &crate::workspace::WorkspaceRegistry::default(),
            &rovr_layout_plugin::Registry::new(),
            &ScratchpadState::new(),
            &rules,
        );
        assert!(
            desired
                .windows
                .get(&WindowId(1))
                .and_then(|t| t.frame)
                .is_some(),
            "non-matching title must stay tiled"
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
            plugin: None,
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
            resolve_layout(
                &config,
                SpaceId(11),
                &observed,
                &crate::workspace::WorkspaceRegistry::default()
            ),
            LayoutKind::Stack,
            "labeled 'dev' space must use the named workspace layout"
        );
        assert_eq!(
            resolve_layout(
                &config,
                SpaceId(12),
                &observed,
                &crate::workspace::WorkspaceRegistry::default()
            ),
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
            plugin: None,
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
            resolve_layout(
                &config,
                SpaceId(11),
                &observed,
                &crate::workspace::WorkspaceRegistry::default()
            ),
            LayoutKind::Stack,
            "unlabeled space uses global layout"
        );
    }
    /// M3e: a window matching an OPEN scratchpad is floated (excluded from tiling).
    #[test]
    fn m3e_open_scratchpad_floats_member() {
        let mut config = Config::default();
        config.general.layout = LayoutKind::Bsp;
        config.scratchpads = vec![ScratchpadConfig {
            name: "term".into(),
            app: Some("com.apple.Terminal".into()),
            title: None,
        }];

        let mut pads = ScratchpadState::new();
        pads.toggle("term"); // open
        assert!(pads.is_open("term"));

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
        observed.windows.insert(
            WindowId(1),
            WindowSnapshot {
                id: WindowId(1),
                pid: ProcessId(1),
                app: String::new(),
                bundle_id: Some("com.apple.Terminal".into()),
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
                minimized: rovr_types::ObservedBool::No,
                fullscreen: rovr_types::ObservedBool::No,
                managed: rovr_types::ObservedBool::Yes,
                generation: 0,
            },
        );

        let mut desired = DesiredState::default();
        apply_layout(
            &config,
            &observed,
            &mut desired,
            &mut Layouts::new(),
            &crate::workspace::WorkspaceRegistry::default(),
            &rovr_layout_plugin::Registry::new(),
            &pads,
            &[],
        );

        assert_eq!(
            desired.windows.get(&WindowId(1)).and_then(|t| t.frame),
            None,
            "open scratchpad member must be floated (not tiled)"
        );
    }

    /// M3e: a window matching a CLOSED scratchpad tiles normally (rejoins layout).
    #[test]
    fn m3e_closed_scratchpad_tiles_member() {
        let mut config = Config::default();
        config.general.layout = LayoutKind::Bsp;
        config.scratchpads = vec![ScratchpadConfig {
            name: "term".into(),
            app: Some("com.apple.Terminal".into()),
            title: None,
        }];

        let pads = ScratchpadState::new(); // closed by default
        assert!(!pads.is_open("term"));

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
        observed.windows.insert(
            WindowId(1),
            WindowSnapshot {
                id: WindowId(1),
                pid: ProcessId(1),
                app: String::new(),
                bundle_id: Some("com.apple.Terminal".into()),
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
                minimized: rovr_types::ObservedBool::No,
                fullscreen: rovr_types::ObservedBool::No,
                managed: rovr_types::ObservedBool::Yes,
                generation: 0,
            },
        );

        let mut desired = DesiredState::default();
        apply_layout(
            &config,
            &observed,
            &mut desired,
            &mut Layouts::new(),
            &crate::workspace::WorkspaceRegistry::default(),
            &rovr_layout_plugin::Registry::new(),
            &pads,
            &[],
        );

        assert!(
            desired
                .windows
                .get(&WindowId(1))
                .and_then(|t| t.frame)
                .is_some(),
            "closed scratchpad member must tile normally"
        );
    }

    /// M3e ordering guard: when a window matches a CLOSED pad (first in config)
    /// and an OPEN pad (later in config), it must still float. Catches the
    /// first-match-then-check-open defect.
    #[test]
    fn m3e_open_pad_beats_closed() {
        let mut config = Config::default();
        config.general.layout = LayoutKind::Bsp;
        config.scratchpads = vec![
            ScratchpadConfig {
                name: "term".into(),
                app: Some("com.apple.Terminal".into()),
                title: None,
            },
            ScratchpadConfig {
                name: "term2".into(),
                app: Some("com.apple.Terminal".into()),
                title: None,
            },
        ];

        let mut pads = ScratchpadState::new();
        // open only the SECOND pad; first pad stays closed
        pads.toggle("term2");
        assert!(pads.is_open("term2"));
        assert!(!pads.is_open("term"));

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
        observed.windows.insert(
            WindowId(1),
            WindowSnapshot {
                id: WindowId(1),
                pid: ProcessId(1),
                app: String::new(),
                bundle_id: Some("com.apple.Terminal".into()),
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
                minimized: rovr_types::ObservedBool::No,
                fullscreen: rovr_types::ObservedBool::No,
                managed: rovr_types::ObservedBool::Yes,
                generation: 0,
            },
        );

        let mut desired = DesiredState::default();
        apply_layout(
            &config,
            &observed,
            &mut desired,
            &mut Layouts::new(),
            &crate::workspace::WorkspaceRegistry::default(),
            &rovr_layout_plugin::Registry::new(),
            &pads,
            &[],
        );

        assert_eq!(
            desired.windows.get(&WindowId(1)).and_then(|t| t.frame),
            None,
            "window matching a closed pad first + open pad later must float"
        );
    }
}
