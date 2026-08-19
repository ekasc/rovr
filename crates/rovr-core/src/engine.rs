use rovr_types::{Direction, PlatformSnapshot, Rect, SpaceId, WindowId};
use thiserror::Error;

use crate::{reconcile::reconcile, Action, DesiredState, Event, FlightRecorder, ObservedState};

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("window {0:?} does not exist")]
    WindowNotFound(WindowId),
    #[error("no focusable window in direction {direction:?} from {from:?}")]
    NoWindowInDirection {
        from: WindowId,
        direction: Direction,
    },
}

#[derive(Debug, Default)]
pub struct Engine {
    pub observed: ObservedState,
    pub desired: DesiredState,
    pub flight_recorder: FlightRecorder,
}

impl Engine {
    pub fn apply_event(&mut self, event: Event) -> Vec<Action> {
        self.flight_recorder.record("event", format!("{event:?}"));

        match event {
            Event::Snapshot(snapshot) => self.apply_snapshot(snapshot),
            Event::WindowDestroyed { window } => {
                self.observed.windows.remove(&window);
                self.desired.windows.remove(&window);
            }
            Event::SpaceDestroyed { space } => {
                self.observed.spaces.remove(&space);
                for target in self.desired.windows.values_mut() {
                    if target.space == Some(space) {
                        target.space = None;
                    }
                }
            }
            Event::DisplayRemoved { display } => {
                self.observed.displays.remove(&display);
                self.observed.bump_generation();
            }
            Event::SystemWillSleep => {}
            Event::SystemWoke | Event::DockRestarted | Event::DisplayTopologyChanged => {
                self.observed.bump_generation();
            }
        }

        let actions = reconcile(&self.observed, &self.desired);
        for action in &actions {
            self.flight_recorder
                .record("reconcile.action", format!("{action:?}"));
        }
        actions
    }

    /// Request a one-shot frame mutation. Persistent layout policy belongs in
    /// `DesiredState`; CLI commands must not accidentally pin a floating window
    /// forever.
    pub fn set_window_frame(
        &self,
        window: WindowId,
        frame: Rect,
    ) -> Result<Vec<Action>, EngineError> {
        self.require_window(window)?;
        Ok(vec![Action::SetWindowFrame { window, frame }])
    }

    /// Request a one-shot Space mutation. Named/persistent workspace assignment
    /// will be represented separately in desired policy.
    pub fn move_window_to_space(
        &self,
        window: WindowId,
        space: SpaceId,
    ) -> Result<Vec<Action>, EngineError> {
        self.require_window(window)?;
        Ok(vec![Action::MoveWindowToSpace { window, space }])
    }

    /// Focus is inherently transient and must never be stored as desired state,
    /// otherwise periodic reconciliation would continuously steal focus back.
    pub fn focus_window(&self, window: WindowId) -> Result<Vec<Action>, EngineError> {
        self.require_window(window)?;
        Ok(vec![Action::FocusWindow { window }])
    }

    pub fn focus_direction(
        &mut self,
        from: WindowId,
        direction: Direction,
    ) -> Result<Vec<Action>, EngineError> {
        let target = self.closest_window_in_direction(from, direction)?;
        self.focus_window(target)
    }

    fn require_window(&self, window: WindowId) -> Result<(), EngineError> {
        if self.observed.windows.contains_key(&window) {
            Ok(())
        } else {
            Err(EngineError::WindowNotFound(window))
        }
    }

    fn apply_snapshot(&mut self, mut snapshot: PlatformSnapshot) {
        let generation = self.observed.generation;
        for window in &mut snapshot.windows {
            window.generation = generation;
        }
        for space in &mut snapshot.spaces {
            space.generation = generation;
        }
        for display in &mut snapshot.displays {
            display.generation = generation;
        }

        self.observed.windows = snapshot
            .windows
            .into_iter()
            .map(|window| (window.id, window))
            .collect();
        self.observed.spaces = snapshot
            .spaces
            .into_iter()
            .map(|space| (space.id, space))
            .collect();
        self.observed.displays = snapshot
            .displays
            .into_iter()
            .map(|display| (display.id, display))
            .collect();
        if snapshot.complete {
            self.observed.refresh_required = false;
        }
    }

    fn closest_window_in_direction(
        &self,
        from: WindowId,
        direction: Direction,
    ) -> Result<WindowId, EngineError> {
        let source = self
            .observed
            .windows
            .get(&from)
            .ok_or(EngineError::WindowNotFound(from))?;
        let source_center = source.frame.center();

        self.observed
            .windows
            .values()
            .filter(|candidate| {
                candidate.id != from
                    && !candidate.minimized
                    && candidate.generation == self.observed.generation
            })
            .filter_map(|candidate| {
                let center = candidate.frame.center();
                let dx = center.x - source_center.x;
                let dy = center.y - source_center.y;
                let in_direction = match direction {
                    Direction::North => dy < 0.0,
                    Direction::South => dy > 0.0,
                    Direction::East => dx > 0.0,
                    Direction::West => dx < 0.0,
                };
                in_direction.then_some((candidate.id, dx * dx + dy * dy))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(id, _)| id)
            .ok_or(EngineError::NoWindowInDirection { from, direction })
    }
}

#[cfg(test)]
mod tests {
    use rovr_types::{DisplayId, ProcessId, Rect, WindowSnapshot};

    use super::*;

    fn snapshot(windows: Vec<WindowSnapshot>) -> PlatformSnapshot {
        PlatformSnapshot {
            windows,
            spaces: vec![],
            displays: vec![],
            complete: true,
        }
    }

    fn window(id: u32, x: f64, y: f64) -> WindowSnapshot {
        WindowSnapshot {
            id: WindowId(id),
            pid: ProcessId(id as i32),
            app: format!("app-{id}"),
            bundle_id: None,
            title: String::new(),
            frame: Rect {
                x,
                y,
                width: 100.0,
                height: 100.0,
            },
            space_id: None,
            display_id: Some(DisplayId(1)),
            focused: id == 1,
            minimized: false,
            fullscreen: false,
            managed: true,
            generation: 0,
        }
    }

    #[test]
    fn wake_invalidates_cached_windows() {
        let mut engine = Engine::default();
        engine.apply_event(Event::Snapshot(snapshot(vec![window(1, 0.0, 0.0)])));
        assert!(!engine.observed.refresh_required);

        let actions = engine.apply_event(Event::SystemWoke);
        assert!(engine.observed.refresh_required);
        assert_eq!(actions, vec![Action::RefreshAll]);
    }

    #[test]
    fn direction_focus_uses_nearest_candidate() {
        let mut engine = Engine::default();
        engine.apply_event(Event::Snapshot(snapshot(vec![
            window(1, 0.0, 0.0),
            window(2, 150.0, 0.0),
            window(3, 500.0, 0.0),
        ])));

        let actions = engine
            .focus_direction(WindowId(1), Direction::East)
            .unwrap();
        assert_eq!(
            actions,
            vec![Action::FocusWindow {
                window: WindowId(2)
            }]
        );
    }
}
