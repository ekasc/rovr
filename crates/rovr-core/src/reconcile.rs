use rovr_types::WindowId;

use crate::{Action, DesiredState, ObservedState};

const FRAME_EPSILON: f64 = 0.75;

pub fn reconcile(observed: &ObservedState, desired: &DesiredState) -> Vec<Action> {
    if observed.refresh_required {
        return vec![Action::RefreshAll];
    }

    let mut actions = Vec::new();
    let mut ids: Vec<WindowId> = desired.windows.keys().copied().collect();
    ids.sort_unstable();

    for id in ids {
        let Some(target) = desired.windows.get(&id) else {
            continue;
        };

        let Some(window) = observed.windows.get(&id) else {
            actions.push(Action::RefreshWindow { window: id });
            continue;
        };

        if window.generation != observed.generation {
            actions.push(Action::RefreshWindow { window: id });
            continue;
        }

        if let Some(space) = target.space {
            if window.space_id != Some(space) {
                actions.push(Action::MoveWindowToSpace { window: id, space });
            }
        }

        if let Some(frame) = target.frame {
            if !window.frame.approx_eq(frame, FRAME_EPSILON) {
                actions.push(Action::SetWindowFrame { window: id, frame });
            }
        }

        if target.focused == Some(true) && !window.focused {
            actions.push(Action::FocusWindow { window: id });
        }
    }

    actions
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use rovr_types::{ProcessId, Rect, WindowId, WindowSnapshot};

    use super::*;
    use crate::WindowTarget;

    fn window(id: u32, generation: u64, frame: Rect) -> WindowSnapshot {
        WindowSnapshot {
            id: WindowId(id),
            pid: ProcessId(1),
            app: "Test".into(),
            bundle_id: None,
            title: "Window".into(),
            frame,
            space_id: None,
            display_id: None,
            focused: false,
            minimized: false,
            fullscreen: false,
            managed: true,
            generation,
        }
    }

    #[test]
    fn refresh_wins_over_speculative_mutation() {
        let observed = ObservedState::default();
        let mut desired = DesiredState::default();
        desired.windows.insert(
            WindowId(7),
            WindowTarget {
                frame: Some(Rect {
                    x: 1.0,
                    y: 2.0,
                    width: 3.0,
                    height: 4.0,
                }),
                ..WindowTarget::default()
            },
        );

        assert_eq!(reconcile(&observed, &desired), vec![Action::RefreshAll]);
    }

    #[test]
    fn stale_window_is_refreshed_before_mutation() {
        let mut observed = ObservedState {
            generation: 2,
            refresh_required: false,
            windows: HashMap::new(),
            spaces: HashMap::new(),
            displays: HashMap::new(),
        };
        observed.windows.insert(
            WindowId(7),
            window(
                WindowId(7).0,
                1,
                Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 100.0,
                    height: 100.0,
                },
            ),
        );

        let mut desired = DesiredState::default();
        desired.windows.insert(
            WindowId(7),
            WindowTarget {
                frame: Some(Rect {
                    x: 20.0,
                    y: 20.0,
                    width: 100.0,
                    height: 100.0,
                }),
                ..WindowTarget::default()
            },
        );

        assert_eq!(
            reconcile(&observed, &desired),
            vec![Action::RefreshWindow {
                window: WindowId(7)
            }]
        );
    }
}
