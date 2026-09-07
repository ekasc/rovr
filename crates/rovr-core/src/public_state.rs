use rovr_types::{DisplayId, PublicState, PublicWindow, SpaceId, WindowId};

use crate::{Engine, ObservedState};

/// Pure builder for the canonical public snapshot.
///
/// `focused_window` must be the already-resolved focus (callers use
/// [`Engine::focused_window`], the same deterministic resolution as
/// `query focused`, so the two never disagree). A missing or stale id maps
/// to `window: None` — never a placeholder.
pub fn build_public_state(
    observed: &ObservedState,
    space: Option<SpaceId>,
    display: Option<DisplayId>,
    focused_window: Option<WindowId>,
) -> PublicState {
    let window = focused_window
        .and_then(|id| observed.windows.get(&id))
        .map(|w| PublicWindow {
            id: w.id,
            pid: w.pid,
            app: w.app.clone(),
            title: w.title.clone(),
        });
    PublicState {
        space,
        display,
        window,
    }
}

impl Engine {
    /// Current public snapshot for the given resolved focus.
    ///
    /// The daemon supplies `space`/`display` from its per-display
    /// current-space/active-display tracking; focus resolution reuses
    /// [`Engine::focused_window`]. CLI (`query current`) and the
    /// `com.rovr.state.changed` publisher both go through here — there is
    /// exactly one builder.
    pub fn public_state(&self, space: Option<SpaceId>, display: Option<DisplayId>) -> PublicState {
        let focused = self.focused_window();
        build_public_state(&self.observed, space, display, focused)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rovr_types::{
        DisplaySnapshot, ObservedBool, PlatformSnapshot, ProcessId, Rect, SpaceSnapshot,
        WindowSnapshot,
    };

    fn rect() -> Rect {
        Rect {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
        }
    }

    fn window(id: u32, app: &str, title: &str, focused: bool) -> WindowSnapshot {
        WindowSnapshot {
            id: WindowId(id),
            pid: ProcessId(100 + id as i32),
            app: app.into(),
            bundle_id: None,
            title: title.into(),
            frame: rect(),
            space_id: Some(SpaceId(3)),
            display_id: Some(DisplayId(1)),
            focused,
            minimized: ObservedBool::No,
            fullscreen: ObservedBool::No,
            managed: ObservedBool::Yes,
            generation: 1,
        }
    }

    fn observed_with(windows: Vec<WindowSnapshot>) -> ObservedState {
        let snapshot = PlatformSnapshot {
            windows,
            spaces: vec![SpaceSnapshot {
                id: SpaceId(3),
                display_id: DisplayId(1),
                label: None,
                focused: true,
                generation: 1,
                position: 2,
                is_fullscreen: false,
                is_system: false,
            }],
            displays: vec![DisplaySnapshot {
                id: DisplayId(1),
                frame: rect(),
                label: None,
                focused: true,
                is_main: true,
                generation: 1,
            }],
            complete: true,
        };
        let mut engine = Engine::default();
        engine.apply_event(crate::Event::Snapshot(snapshot));
        engine.observed
    }

    #[test]
    fn serializes_to_stable_fields() {
        let observed = observed_with(vec![window(482, "Ghostty", "~/src/rovr", true)]);
        let state = build_public_state(
            &observed,
            Some(SpaceId(3)),
            Some(DisplayId(1)),
            Some(WindowId(482)),
        );
        let value = serde_json::to_value(&state).unwrap();
        assert_eq!(value["space"], 3);
        assert_eq!(value["display"], 1);
        assert_eq!(value["window"]["id"], 482);
        assert_eq!(value["window"]["pid"], 582);
        assert_eq!(value["window"]["app"], "Ghostty");
        assert_eq!(value["window"]["title"], "~/src/rovr");
        // Round-trip: the wire format is the struct, nothing else.
        let back: PublicState = serde_json::from_value(value).unwrap();
        assert_eq!(state, back);
    }

    #[test]
    fn absent_focused_window_is_null_not_placeholder() {
        let observed = observed_with(vec![]);
        let state = build_public_state(&observed, Some(SpaceId(3)), Some(DisplayId(1)), None);
        assert_eq!(state.window, None);
        let value = serde_json::to_value(&state).unwrap();
        assert!(value["window"].is_null());
        assert_eq!(value["space"], 3);
        assert_eq!(value["display"], 1);
    }

    #[test]
    fn stale_focused_id_maps_to_no_window() {
        let observed = observed_with(vec![window(1, "App", "t", false)]);
        let state = build_public_state(
            &observed,
            Some(SpaceId(3)),
            Some(DisplayId(1)),
            Some(WindowId(999)),
        );
        assert_eq!(state.window, None);
    }

    #[test]
    fn equality_distinguishes_every_visible_field() {
        let observed = observed_with(vec![window(482, "Ghostty", "~/src/rovr", true)]);
        let base = build_public_state(
            &observed,
            Some(SpaceId(3)),
            Some(DisplayId(1)),
            Some(WindowId(482)),
        );
        // Space change.
        assert_ne!(
            base,
            build_public_state(
                &observed,
                Some(SpaceId(4)),
                Some(DisplayId(1)),
                Some(WindowId(482))
            )
        );
        // Display change.
        assert_ne!(
            base,
            build_public_state(
                &observed,
                Some(SpaceId(3)),
                Some(DisplayId(2)),
                Some(WindowId(482))
            )
        );
        // Focus change (different window).
        let observed2 = observed_with(vec![
            window(482, "Ghostty", "~/src/rovr", false),
            WindowSnapshot {
                focused: true,
                ..window(483, "Safari", "page", true)
            },
        ]);
        assert_ne!(
            base,
            build_public_state(
                &observed2,
                Some(SpaceId(3)),
                Some(DisplayId(1)),
                Some(WindowId(483))
            )
        );
        // Title change.
        let observed3 = observed_with(vec![window(482, "Ghostty", "~/other", true)]);
        assert_ne!(
            base,
            build_public_state(
                &observed3,
                Some(SpaceId(3)),
                Some(DisplayId(1)),
                Some(WindowId(482))
            )
        );
        // Focus lost.
        assert_ne!(
            base,
            build_public_state(&observed, Some(SpaceId(3)), Some(DisplayId(1)), None)
        );
        // Identical snapshots compare equal (duplicate suppression key).
        assert_eq!(
            base,
            build_public_state(
                &observed,
                Some(SpaceId(3)),
                Some(DisplayId(1)),
                Some(WindowId(482))
            )
        );
    }

    #[test]
    fn engine_public_state_agrees_with_focused_window() {
        let mut engine = Engine::default();
        engine.apply_event(crate::Event::Snapshot(PlatformSnapshot {
            windows: vec![
                window(1, "A", "one", true),
                WindowSnapshot {
                    focused: true,
                    space_id: Some(SpaceId(9)),
                    ..window(2, "B", "two", true)
                },
            ],
            spaces: vec![
                SpaceSnapshot {
                    id: SpaceId(3),
                    display_id: DisplayId(1),
                    label: None,
                    focused: true,
                    generation: 1,
                    position: 0,
                    is_fullscreen: false,
                    is_system: false,
                },
                SpaceSnapshot {
                    id: SpaceId(9),
                    display_id: DisplayId(1),
                    label: None,
                    focused: false,
                    generation: 1,
                    position: 1,
                    is_fullscreen: false,
                    is_system: false,
                },
            ],
            displays: vec![DisplaySnapshot {
                id: DisplayId(1),
                frame: rect(),
                label: None,
                focused: true,
                is_main: true,
                generation: 1,
            }],
            complete: true,
        }));
        // Focused window on the focused space (id 1) wins over id 2.
        let state = engine.public_state(Some(SpaceId(3)), Some(DisplayId(1)));
        assert_eq!(state.window.as_ref().unwrap().id, WindowId(1));
        assert_eq!(engine.focused_window(), Some(WindowId(1)));
    }
}
