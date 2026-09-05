use super::*;
use rovr_types::{DisplaySnapshot, ObservedBool, PlatformSnapshot, ProcessId, Rect};

fn window_on(id: u32, space: u64) -> WindowSnapshot {
    WindowSnapshot {
        id: WindowId(id),
        pid: ProcessId(id as i32),
        app: "test".into(),
        bundle_id: None,
        title: String::new(),
        frame: Rect {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
        },
        space_id: Some(SpaceId(space)),
        display_id: Some(DisplayId(1)),
        focused: id == 1,
        minimized: ObservedBool::No,
        fullscreen: ObservedBool::No,
        managed: ObservedBool::Yes,
        generation: 0,
    }
}
fn topology(ids: &[u64], focused: u64, windows: Vec<WindowSnapshot>) -> PlatformSnapshot {
    PlatformSnapshot {
        spaces: ids
            .iter()
            .enumerate()
            .map(|(position, &id)| SpaceSnapshot {
                id: SpaceId(id),
                display_id: DisplayId(1),
                label: None,
                focused: id == focused,
                generation: 0,
                position: position as u32,
                is_fullscreen: id == 99,
                is_system: id == 100,
            })
            .collect(),
        displays: vec![DisplaySnapshot {
            id: DisplayId(1),
            frame: Rect {
                x: 0.0,
                y: 0.0,
                width: 1000.0,
                height: 800.0,
            },
            label: None,
            focused: true,
            is_main: true,
            generation: 0,
        }],
        windows,
        complete: true,
    }
}
fn configured(names: &[&str]) -> Engine {
    let mut engine = Engine::new(Config {
        workspaces: names
            .iter()
            .map(|name| rovr_config::WorkspaceConfig {
                name: (*name).into(),
                layout: rovr_types::LayoutKind::Bsp,
                display: None,
                persistent: true,
                plugin: None,
            })
            .collect(),
        ..Config::default()
    });
    engine.capabilities.create_space = true;
    engine.capabilities.destroy_space = true;
    engine.capabilities.reorder_space = true;
    engine
}
fn sparse() -> Engine {
    let mut engine = configured(&["1", "2", "5"]);
    engine.apply_event(Event::Snapshot(topology(
        &[11, 22, 55],
        22,
        vec![window_on(1, 22)],
    )));
    engine
}
fn creates(actions: &[Action]) -> usize {
    actions
        .iter()
        .filter(|a| matches!(a, Action::CreateSpace { .. }))
        .count()
}
fn assert_mapping(engine: &Engine, pairs: &[(&str, u64)]) {
    for &(name, id) in pairs {
        assert_eq!(
            engine.workspaces.backing_for(name),
            Some(SpaceId(id)),
            "workspace {name}"
        );
    }
}
#[test]
fn sparse_logical_numbers_address_dense_native_slots() {
    let mut engine = sparse();
    assert_mapping(&engine, &[("1", 11), ("2", 22), ("5", 55)]);
    assert_eq!(
        engine.focus_workspace("5").unwrap(),
        vec![Action::FocusSpace { space: SpaceId(55) }]
    );
}
#[test]
fn delayed_focus_creation_is_single_flight_and_inserts_before_five() {
    let mut engine = sparse();
    assert_eq!(creates(&engine.focus_workspace("4").unwrap()), 1);
    for _ in 0..4 {
        assert_eq!(creates(&engine.focus_workspace("4").unwrap()), 0);
        assert_eq!(
            creates(&engine.apply_event(Event::Snapshot(topology(
                &[11, 22, 55],
                22,
                vec![window_on(1, 22)]
            )))),
            0
        );
    }
    let actions = engine.apply_event(Event::Snapshot(topology(
        &[11, 22, 55, 44],
        22,
        vec![window_on(1, 22)],
    )));
    assert!(actions.contains(&Action::MoveSpace {
        space: SpaceId(44),
        after: SpaceId(22)
    }));
    assert!(!actions.contains(&Action::FocusSpace { space: SpaceId(44) }));
    let actions = engine.apply_event(Event::Snapshot(topology(
        &[11, 22, 44, 55],
        22,
        vec![window_on(1, 22)],
    )));
    assert!(actions.contains(&Action::FocusSpace { space: SpaceId(44) }));
    assert_eq!(engine.workspaces.ordered_names(), vec!["1", "2", "4", "5"]);
    assert_mapping(&engine, &[("1", 11), ("2", 22), ("4", 44), ("5", 55)]);
}
#[test]
fn delayed_move_creation_retains_window_and_does_not_follow() {
    let mut engine = sparse();
    assert_eq!(
        creates(&engine.move_window_to_workspace(None, "4").unwrap()),
        1
    );
    for _ in 0..4 {
        assert_eq!(
            creates(&engine.apply_event(Event::Snapshot(topology(
                &[11, 22, 55],
                22,
                vec![window_on(1, 22)]
            )))),
            0
        );
    }
    let actions = engine.apply_event(Event::Snapshot(topology(
        &[11, 22, 55, 44],
        22,
        vec![window_on(1, 22)],
    )));
    assert!(actions.contains(&Action::MoveSpace {
        space: SpaceId(44),
        after: SpaceId(22)
    }));
    let actions = engine.apply_event(Event::Snapshot(topology(
        &[11, 22, 44, 55],
        22,
        vec![window_on(1, 22)],
    )));
    assert!(actions.contains(&Action::MoveWindowToSpace {
        window: WindowId(1),
        space: SpaceId(44)
    }));
    assert!(!actions
        .iter()
        .any(|a| matches!(a, Action::FocusSpace { .. })));
    assert!(engine.pending_workspace_move.is_empty());
}
#[test]
fn mission_control_reorder_changes_logical_contents() {
    let mut engine = configured(&["1", "2", "3", "4"]);
    engine.apply_event(Event::Snapshot(topology(&[11, 22, 33, 44], 11, vec![])));
    let actions = engine.apply_event(Event::Snapshot(topology(&[11, 22, 44, 33], 11, vec![])));
    assert_mapping(&engine, &[("1", 11), ("2", 22), ("3", 44), ("4", 33)]);
    assert!(!actions
        .iter()
        .any(|a| matches!(a, Action::MoveSpace { .. })));
}
#[test]
fn persistent_recreation_waits_for_observation_without_duplicate_create() {
    let mut engine = configured(&["1", "2"]);
    assert_eq!(
        creates(&engine.apply_event(Event::Snapshot(topology(&[11], 11, vec![])))),
        1
    );
    for _ in 0..4 {
        assert_eq!(
            creates(&engine.apply_event(Event::Snapshot(topology(&[11], 11, vec![])))),
            0
        );
    }
    assert_eq!(
        creates(&engine.apply_event(Event::Snapshot(topology(&[11, 22], 11, vec![])))),
        0
    );
    assert_mapping(&engine, &[("1", 11), ("2", 22)]);
    assert!(engine.awaited_creations.is_empty());
}
#[test]
fn manual_empty_space_has_bounded_grace() {
    let mut engine = sparse();
    let snap = topology(&[11, 22, 55, 66], 22, vec![window_on(1, 22)]);
    let actions = engine.apply_event(Event::Snapshot(snap.clone()));
    assert!(!actions.contains(&Action::DestroySpace { space: SpaceId(66) }));
    assert!(engine.external_spaces.contains_key(&SpaceId(66)));
    engine.external_spaces.insert(
        SpaceId(66),
        std::time::Instant::now() - std::time::Duration::from_secs(1),
    );
    let actions = engine.apply_event(Event::Snapshot(snap));
    assert!(actions.contains(&Action::DestroySpace { space: SpaceId(66) }));
}
fn fullscreen_snapshot(focused: u64) -> PlatformSnapshot {
    let mut window = window_on(1, 99);
    window.fullscreen = ObservedBool::Yes;
    topology(&[11, 22, 55, 99], focused, vec![window])
}
#[test]
fn fullscreen_substitutes_origin_without_shifting_sparse_hotkeys_or_collecting_restore() {
    let mut engine = sparse();
    engine.apply_event(Event::Snapshot(topology(
        &[11, 22, 55],
        11,
        vec![window_on(1, 11)],
    )));
    engine.workspaces.0.get_mut("1").unwrap().persistent = false;
    let actions = engine.apply_event(Event::Snapshot(fullscreen_snapshot(99)));
    assert_eq!(
        engine.workspaces.0["1"].backing,
        Some(WorkspaceBacking::FullscreenReplacement {
            fullscreen_space: SpaceId(99),
            restore_space: SpaceId(11),
            window: WindowId(1)
        })
    );
    assert_mapping(&engine, &[("1", 99), ("2", 22), ("5", 55)]);
    for (name, id) in [("1", 99), ("2", 22), ("5", 55)] {
        assert_eq!(
            engine.focus_workspace(name).unwrap(),
            vec![Action::FocusSpace { space: SpaceId(id) }]
        );
    }
    assert_eq!(engine.workspaces.0.len(), 3);
    assert!(!actions.contains(&Action::DestroySpace { space: SpaceId(11) }));
    let actions = engine.apply_event(Event::Snapshot(fullscreen_snapshot(22)));
    assert!(!actions.contains(&Action::DestroySpace { space: SpaceId(11) }));
    assert!(matches!(
        engine.move_window_to_workspace(Some(WindowId(1)), "4"),
        Err(EngineError::NativeFullscreenMove)
    ));
    assert!(!engine.workspaces.0.contains_key("4"));
    engine.apply_event(Event::Snapshot(topology(
        &[11, 22, 55],
        11,
        vec![window_on(1, 11)],
    )));
    assert_eq!(
        engine.workspaces.0["1"].backing,
        Some(WorkspaceBacking::Normal { space: SpaceId(11) })
    );
}
#[test]
fn closing_fullscreen_window_releases_restore_for_normal_gc() {
    let mut engine = sparse();
    engine.apply_event(Event::Snapshot(topology(
        &[11, 22, 55],
        11,
        vec![window_on(1, 11)],
    )));
    engine.workspaces.0.get_mut("1").unwrap().persistent = false;
    engine.apply_event(Event::Snapshot(fullscreen_snapshot(99)));
    engine.apply_event(Event::Snapshot(topology(&[11, 22, 55, 99], 22, vec![])));
    assert!(!matches!(
        engine.workspaces.0["1"].backing,
        Some(WorkspaceBacking::FullscreenReplacement { .. })
    ));
    engine.dynamic_grace.clear();
    let actions = engine.apply_event(Event::Snapshot(topology(&[11, 22, 55], 22, vec![])));
    assert!(
        actions.contains(&Action::DestroySpace { space: SpaceId(11) })
            || engine.pending_destroy.contains_key(&SpaceId(11))
    );
    engine.apply_event(Event::Snapshot(topology(&[22, 55], 22, vec![])));
    assert!(!engine.workspaces.0.contains_key("1"));
}
#[test]
fn empty_dynamic_workspace_survives_focus_then_is_collected_after_leaving() {
    let mut engine = sparse();
    engine.focus_workspace("4").unwrap();
    engine.apply_event(Event::Snapshot(topology(
        &[11, 22, 55, 44],
        22,
        vec![window_on(1, 22)],
    )));
    engine.apply_event(Event::Snapshot(topology(
        &[11, 22, 44, 55],
        44,
        vec![window_on(1, 22)],
    )));
    engine.dynamic_grace.clear();
    let actions = engine.apply_event(Event::Snapshot(topology(
        &[11, 22, 44, 55],
        44,
        vec![window_on(1, 22)],
    )));
    assert!(!actions.contains(&Action::DestroySpace { space: SpaceId(44) }));
    let actions = engine.apply_event(Event::Snapshot(topology(
        &[11, 22, 44, 55],
        22,
        vec![window_on(1, 22)],
    )));
    assert!(actions.contains(&Action::DestroySpace { space: SpaceId(44) }));
    engine.apply_event(Event::Snapshot(topology(
        &[11, 22, 55],
        22,
        vec![window_on(1, 22)],
    )));
    assert!(!engine.workspaces.0.contains_key("4"));
}

#[test]
fn fullscreen_physical_position_and_system_spaces_do_not_consume_logical_slots() {
    let mut engine = sparse();
    engine.apply_event(Event::Snapshot(topology(
        &[11, 22, 55],
        11,
        vec![window_on(1, 11)],
    )));
    engine.apply_event(Event::Snapshot(fullscreen_snapshot(99)));
    for order in [&[99, 22, 55, 11, 100][..], &[100, 11, 22, 99, 55][..]] {
        let mut window = window_on(1, 99);
        window.fullscreen = ObservedBool::Yes;
        let actions = engine.apply_event(Event::Snapshot(topology(order, 99, vec![window])));
        assert_mapping(&engine, &[("1", 99), ("2", 22), ("5", 55)]);
        assert_eq!(engine.workspaces.0.len(), 3);
        assert!(!actions.iter().any(|a| matches!(a, Action::DestroySpace { space } if *space == SpaceId(99) || *space == SpaceId(100) || *space == SpaceId(11))));
    }
}

#[test]
fn first_slot_insertion_reorders_prefix_before_fulfilling_focus() {
    let mut engine = configured(&["2", "5"]);
    engine.apply_event(Event::Snapshot(topology(&[22, 55], 22, vec![])));
    assert_eq!(creates(&engine.focus_workspace("1").unwrap()), 1);
    let actions = engine.apply_event(Event::Snapshot(topology(&[22, 55, 11], 22, vec![])));
    assert!(actions.contains(&Action::MoveSpace {
        space: SpaceId(22),
        after: SpaceId(11)
    }));
    assert!(!actions
        .iter()
        .any(|a| matches!(a, Action::FocusSpace { .. })));
    engine.placement.as_mut().unwrap().retry_at = std::time::Instant::now();
    let actions = engine.apply_event(Event::Snapshot(topology(&[55, 11, 22], 22, vec![])));
    assert!(actions.contains(&Action::MoveSpace {
        space: SpaceId(55),
        after: SpaceId(11)
    }));
    engine.placement.as_mut().unwrap().retry_at = std::time::Instant::now();
    let actions = engine.apply_event(Event::Snapshot(topology(&[11, 55, 22], 22, vec![])));
    assert!(actions.contains(&Action::MoveSpace {
        space: SpaceId(22),
        after: SpaceId(11)
    }));
    let actions = engine.apply_event(Event::Snapshot(topology(&[11, 22, 55], 22, vec![])));
    assert!(actions.contains(&Action::FocusSpace { space: SpaceId(11) }));
    assert_mapping(&engine, &[("1", 11), ("2", 22), ("5", 55)]);
}

#[test]
fn incomplete_snapshot_cannot_complete_creation_or_destroy_workspace() {
    let mut engine = sparse();
    engine.focus_workspace("4").unwrap();
    let mut incomplete = topology(&[44], 44, vec![]);
    incomplete.complete = false;
    assert!(engine.apply_event(Event::Snapshot(incomplete)).is_empty());
    assert_eq!(engine.awaited_creations.len(), 1);
    assert_eq!(engine.workspaces.backing_for("4"), None);
    assert_mapping(&engine, &[("1", 11), ("2", 22), ("5", 55)]);
    assert_eq!(
        creates(&engine.apply_event(Event::Snapshot(topology(
            &[11, 22, 55],
            22,
            vec![window_on(1, 22)]
        )))),
        0
    );
}

#[test]
fn repeated_focus_and_move_share_one_creation_and_keep_both_intents() {
    let mut engine = sparse();
    assert_eq!(creates(&engine.focus_workspace("4").unwrap()), 1);
    for _ in 0..4 {
        assert_eq!(
            creates(
                &engine
                    .move_window_to_workspace(Some(WindowId(1)), "4")
                    .unwrap()
            ),
            0
        );
        assert_eq!(creates(&engine.focus_workspace("4").unwrap()), 0);
    }
    assert_eq!(engine.pending_workspace_focus.len(), 1);
    assert_eq!(engine.pending_workspace_move.len(), 1);
    engine.apply_event(Event::Snapshot(topology(
        &[11, 22, 55, 44],
        22,
        vec![window_on(1, 22)],
    )));
    let actions = engine.apply_event(Event::Snapshot(topology(
        &[11, 22, 44, 55],
        22,
        vec![window_on(1, 22)],
    )));
    assert!(actions.contains(&Action::FocusSpace { space: SpaceId(44) }));
    assert!(actions.contains(&Action::MoveWindowToSpace {
        window: WindowId(1),
        space: SpaceId(44)
    }));
    assert!(engine.pending_workspace_focus.is_empty());
    assert!(engine.pending_workspace_move.is_empty());
}

#[test]
fn fullscreen_exit_restores_parked_native_space_to_original_slot() {
    let mut engine = sparse();
    engine.apply_event(Event::Snapshot(topology(
        &[11, 22, 55],
        11,
        vec![window_on(1, 11)],
    )));
    engine.apply_event(Event::Snapshot(fullscreen_snapshot(99)));
    let mut fullscreen_window = window_on(1, 99);
    fullscreen_window.fullscreen = ObservedBool::Yes;
    engine.apply_event(Event::Snapshot(topology(
        &[99, 22, 55, 11],
        99,
        vec![fullscreen_window],
    )));
    let actions = engine.apply_event(Event::Snapshot(topology(
        &[22, 55, 11],
        11,
        vec![window_on(1, 11)],
    )));
    assert!(actions.contains(&Action::MoveSpace {
        space: SpaceId(22),
        after: SpaceId(11)
    }));
    assert_mapping(&engine, &[("1", 11), ("2", 22), ("5", 55)]);
    assert_eq!(
        engine.workspaces.0["1"].backing,
        Some(WorkspaceBacking::Normal { space: SpaceId(11) })
    );
    assert!(engine.placement.is_some());
    engine.apply_event(Event::Snapshot(topology(
        &[11, 22, 55],
        11,
        vec![window_on(1, 11)],
    )));
    assert!(engine.placement.is_none());
    assert_mapping(&engine, &[("1", 11), ("2", 22), ("5", 55)]);
}

#[test]
fn last_unfocused_desktop_on_second_display_does_not_pin_logical_workspace_one() {
    let mut engine = configured(&["1", "2"]);
    engine.workspaces.0.get_mut("1").unwrap().desired_display = Some("2".into());
    engine.workspaces.0.get_mut("2").unwrap().desired_display = Some("1".into());
    let mut snapshot = topology(&[22, 11], 22, vec![window_on(1, 22)]);
    snapshot.spaces[1].display_id = DisplayId(2);
    let mut second_display = snapshot.displays[0].clone();
    second_display.id = DisplayId(2);
    second_display.is_main = false;
    second_display.focused = false;
    snapshot.displays.push(second_display);
    engine.apply_event(Event::Snapshot(snapshot.clone()));
    assert_mapping(&engine, &[("1", 11), ("2", 22)]);
    engine.workspaces.0.get_mut("1").unwrap().persistent = false;
    engine.workspaces.0.get_mut("1").unwrap().dynamic = true;
    let actions = engine.apply_event(Event::Snapshot(snapshot));
    assert!(!actions.contains(&Action::DestroySpace { space: SpaceId(11) }));
    assert!(!engine.workspaces.0.contains_key("1"));
    assert!(engine.observed.spaces.contains_key(&SpaceId(11)));
    assert_mapping(&engine, &[("2", 22)]);
}

#[test]
fn uncertain_creation_deadline_keeps_reservation_until_delayed_space_binds() {
    let mut engine = sparse();
    assert_eq!(creates(&engine.focus_workspace("4").unwrap()), 1);
    assert_eq!(engine.awaited_creations.len(), 1);
    // Force the 10s observation deadline to elapse without any platform
    // evidence: the reservation must go Uncertain, never re-issue.
    for awaited in engine.awaited_creations.iter_mut() {
        awaited.observation = super::CreationObservation::Awaiting {
            deadline: std::time::Instant::now() - std::time::Duration::from_secs(1),
        };
    }
    let actions = engine.apply_event(Event::Snapshot(topology(
        &[11, 22, 55],
        22,
        vec![window_on(1, 22)],
    )));
    assert_eq!(
        creates(&actions),
        0,
        "uncertain creation must never re-issue against an unchanged snapshot"
    );
    assert_eq!(
        engine.awaited_creations.len(),
        1,
        "single-flight reservation retained while success is still possible"
    );
    // Delayed success finally materializes: binds with no duplicate.
    let actions = engine.apply_event(Event::Snapshot(topology(
        &[11, 22, 55, 44],
        22,
        vec![window_on(1, 22)],
    )));
    assert_eq!(creates(&actions), 0);
    assert!(engine.awaited_creations.is_empty());
    assert_mapping(&engine, &[("4", 44)]);
}

#[test]
fn genuine_creation_failure_releases_single_flight_for_retry() {
    let mut engine = sparse();
    assert_eq!(creates(&engine.focus_workspace("4").unwrap()), 1);
    // The daemon reports explicit evidence nothing landed (the SA call
    // failed, or an earlier action in the batch failed first).
    engine.note_batch_failed(&[Action::CreateSpace {
        anchor: SpaceId(22),
    }]);
    assert!(
        engine.awaited_creations.is_empty(),
        "failed reservation must be released"
    );
    assert!(
        engine.workspaces.0.contains_key("4"),
        "workspace intent survives the failure"
    );
    // Deterministic recovery: the next Alt+N retries exactly once...
    assert_eq!(creates(&engine.focus_workspace("4").unwrap()), 1);
    // ...and unchanged snapshots still never duplicate.
    assert_eq!(
        creates(&engine.apply_event(Event::Snapshot(topology(
            &[11, 22, 55],
            22,
            vec![window_on(1, 22)]
        )))),
        0
    );
    // The retry's Space binds normally.
    let actions = engine.apply_event(Event::Snapshot(topology(
        &[11, 22, 55, 44],
        22,
        vec![window_on(1, 22)],
    )));
    assert_eq!(creates(&actions), 0);
    assert_mapping(&engine, &[("4", 44)]);
}

#[test]
fn batch_failure_releases_only_unexecuted_reservations() {
    let mut engine = sparse();
    assert_eq!(creates(&engine.focus_workspace("4").unwrap()), 1);
    // A batch with no CreateSpace must not touch the creation reservation.
    engine.note_batch_failed(&[Action::FocusSpace { space: SpaceId(11) }]);
    assert_eq!(engine.awaited_creations.len(), 1);
    // Precision for destroys: the executed prefix keeps its reservation
    // (observation will confirm it), the failed suffix is released.
    engine.pending_destroy.insert(SpaceId(44), "4".into());
    engine.pending_destroy.insert(SpaceId(55), "5".into());
    engine.topology_destroy = Some(SpaceId(55));
    engine.note_batch_failed(&[Action::DestroySpace { space: SpaceId(55) }]);
    assert!(
        engine.pending_destroy.contains_key(&SpaceId(44)),
        "executed destroy keeps its reservation"
    );
    assert!(
        !engine.pending_destroy.contains_key(&SpaceId(55)),
        "unexecuted destroy is released for recompute"
    );
    assert_eq!(engine.topology_destroy, None);
}

#[test]
fn topology_reset_requeues_inflight_creation_without_shuffling() {
    let mut engine = sparse();
    assert_eq!(creates(&engine.focus_workspace("4").unwrap()), 1);
    // Dock rebuild: every known SpaceId vanishes, all-new IDs appear. The
    // voided reservation is re-queued (held out of the remap, so stable
    // bindings never shuffle) and exactly one fresh creation issues against
    // the new world — evidence-gated, not a blind retry.
    let actions = engine.apply_event(Event::Snapshot(topology(
        &[101, 102, 103],
        102,
        vec![window_on(1, 102)],
    )));
    assert_mapping(&engine, &[("1", 101), ("2", 102), ("5", 103)]);
    assert_eq!(engine.workspaces.backing_for("4"), None);
    assert_eq!(
        creates(&actions),
        1,
        "reset re-issues exactly once against the new topology: {actions:?}"
    );
    assert_eq!(engine.awaited_creations.len(), 1);
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, Action::FocusSpace { .. })),
        "no hijacked focus from a mis-bound slot: {actions:?}"
    );
    // The fresh creation binds with no duplicate.
    let actions = engine.apply_event(Event::Snapshot(topology(
        &[101, 102, 103, 104],
        102,
        vec![window_on(1, 102)],
    )));
    assert_eq!(creates(&actions), 0);
    assert!(engine.awaited_creations.is_empty());
    assert_mapping(&engine, &[("1", 101), ("2", 102), ("4", 104), ("5", 103)]);
}

#[test]
fn manual_space_adoption_extends_numbering_positionally() {
    let mut engine = sparse();
    // Mission Control + appends an empty Space at the end; the user focuses it.
    let actions = engine.apply_event(Event::Snapshot(topology(
        &[11, 22, 55, 66],
        66,
        vec![window_on(1, 22)],
    )));
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, Action::DestroySpace { .. })),
        "a grace-period manual Space must not be collected: {actions:?}"
    );
    assert_eq!(
        engine.workspaces.backing_for("6"),
        Some(SpaceId(66)),
        "an appended Space extends numbering (1,2,5 + D => 6), not first-gap 3"
    );
    assert!(engine.workspaces.backing_for("3").is_none());
    assert_mapping(&engine, &[("1", 11), ("2", 22), ("5", 55), ("6", 66)]);
    // The positional mapping is stable across snapshots.
    let actions = engine.apply_event(Event::Snapshot(topology(
        &[11, 22, 55, 66],
        66,
        vec![window_on(1, 22)],
    )));
    assert_mapping(&engine, &[("1", 11), ("2", 22), ("5", 55), ("6", 66)]);
    assert!(!actions.contains(&Action::DestroySpace { space: SpaceId(66) }));
}

#[test]
fn fullscreen_entry_survives_empty_restore_gap() {
    let mut engine = sparse();
    engine.apply_event(Event::Snapshot(topology(
        &[11, 22, 55],
        11,
        vec![window_on(1, 11)],
    )));
    engine.workspaces.0.get_mut("1").unwrap().persistent = false;
    // The window leaves A one tick before the fullscreen Space/window
    // correlation is observable: A is empty and the window is missing.
    let actions = engine.apply_event(Event::Snapshot(topology(&[11, 22, 55], 22, vec![])));
    assert!(
        !actions.contains(&Action::DestroySpace { space: SpaceId(11) }),
        "restore Space must survive the entry gap: {actions:?}"
    );
    // The late correlation still enters substitution without shifting hotkeys.
    let actions = engine.apply_event(Event::Snapshot(fullscreen_snapshot(99)));
    assert_eq!(
        engine.workspaces.0["1"].backing,
        Some(WorkspaceBacking::FullscreenReplacement {
            fullscreen_space: SpaceId(99),
            restore_space: SpaceId(11),
            window: WindowId(1)
        })
    );
    assert_mapping(&engine, &[("1", 99), ("2", 22), ("5", 55)]);
    assert!(!actions.contains(&Action::DestroySpace { space: SpaceId(11) }));
}

#[test]
fn fullscreen_entry_bridges_spaceless_window_observation() {
    let mut engine = sparse();
    engine.apply_event(Event::Snapshot(topology(
        &[11, 22, 55],
        11,
        vec![window_on(1, 11)],
    )));
    engine.workspaces.0.get_mut("1").unwrap().persistent = false;
    // Transient variant: the window is still enumerated but reports no Space.
    let mut homeless = window_on(1, 11);
    homeless.space_id = None;
    let actions = engine.apply_event(Event::Snapshot(topology(&[11, 22, 55], 22, vec![homeless])));
    assert!(
        !actions.contains(&Action::DestroySpace { space: SpaceId(11) }),
        "restore Space must survive a spaceless observation: {actions:?}"
    );
    assert!(matches!(
        engine.workspaces.0["1"].backing,
        Some(WorkspaceBacking::Normal { .. })
    ));
    let actions = engine.apply_event(Event::Snapshot(fullscreen_snapshot(99)));
    assert!(
        matches!(
            engine.workspaces.0["1"].backing,
            Some(WorkspaceBacking::FullscreenReplacement { .. })
        ),
        "entry must fire once the correlation appears"
    );
    assert!(!actions.contains(&Action::DestroySpace { space: SpaceId(11) }));
}

#[test]
fn fullscreen_exit_before_window_returns_keeps_restore() {
    let mut engine = sparse();
    engine.apply_event(Event::Snapshot(topology(
        &[11, 22, 55],
        11,
        vec![window_on(1, 11)],
    )));
    engine.workspaces.0.get_mut("1").unwrap().persistent = false;
    engine.apply_event(Event::Snapshot(fullscreen_snapshot(99)));
    assert!(matches!(
        engine.workspaces.0["1"].backing,
        Some(WorkspaceBacking::FullscreenReplacement { .. })
    ));
    // F vanishes a tick before the window is observed back on A.
    let actions = engine.apply_event(Event::Snapshot(topology(&[11, 22, 55], 22, vec![])));
    assert_eq!(
        engine.workspaces.0["1"].backing,
        Some(WorkspaceBacking::Normal { space: SpaceId(11) })
    );
    assert!(
        !actions.contains(&Action::DestroySpace { space: SpaceId(11) }),
        "just-restored Space has grace until its window lands: {actions:?}"
    );
    // The window lands back; the restore was never collected in between.
    let actions = engine.apply_event(Event::Snapshot(topology(
        &[11, 22, 55],
        11,
        vec![window_on(1, 11)],
    )));
    assert!(!actions.contains(&Action::DestroySpace { space: SpaceId(11) }));
    assert_mapping(&engine, &[("1", 11), ("2", 22), ("5", 55)]);
}

#[test]
fn incomplete_snapshot_freezes_fullscreen_lifecycle() {
    let mut engine = sparse();
    engine.apply_event(Event::Snapshot(topology(
        &[11, 22, 55],
        11,
        vec![window_on(1, 11)],
    )));
    engine.workspaces.0.get_mut("1").unwrap().persistent = false;
    engine.apply_event(Event::Snapshot(fullscreen_snapshot(99)));
    // A partial observation during the transition must change nothing.
    let mut partial = topology(&[11, 22, 55], 22, vec![]);
    partial.complete = false;
    assert!(engine.apply_event(Event::Snapshot(partial)).is_empty());
    assert!(
        matches!(
            engine.workspaces.0["1"].backing,
            Some(WorkspaceBacking::FullscreenReplacement { .. })
        ),
        "partial snapshots must not enter, exit, or collect"
    );
}

#[test]
fn orphan_fullscreen_and_system_spaces_are_never_adopted() {
    let mut engine = sparse();
    // Brand-new fullscreen + system Spaces nobody owns (their window is new,
    // so no restore Space claims it).
    let mut snap = topology(&[11, 22, 55, 99, 100], 22, vec![window_on(1, 22)]);
    let mut fresh = window_on(7, 99);
    fresh.fullscreen = ObservedBool::Yes;
    snap.windows.push(fresh);
    let actions = engine.apply_event(Event::Snapshot(snap));
    assert_eq!(
        engine.workspaces.0.len(),
        3,
        "system/fullscreen Spaces must never consume logical slots"
    );
    assert_mapping(&engine, &[("1", 11), ("2", 22), ("5", 55)]);
    assert!(
        !actions.iter().any(
            |a| matches!(a, Action::DestroySpace { space } if *space == SpaceId(99) || *space == SpaceId(100))
        ),
        "system/fullscreen Spaces are never destroy targets: {actions:?}"
    );
}

#[test]
fn lost_backing_parks_layout_under_logical_name_until_slot_returns() {
    let mut engine = configured(&["1", "2", "3"]);
    engine.capabilities.create_space = true;
    engine.apply_event(Event::Snapshot(topology(&[11, 22, 33], 11, vec![])));
    assert_mapping(&engine, &[("1", 11), ("2", 22), ("3", 33)]);
    for id in [1u32, 2, 3] {
        engine
            .layouts
            .entry(SpaceId(33))
            .or_default()
            .bsp
            .insert(WindowId(id));
    }
    let area = Rect {
        x: 0.0,
        y: 0.0,
        width: 1000.0,
        height: 800.0,
    };
    let before = engine.layouts[&SpaceId(33)].bsp.placements(area, 8.0);
    assert_eq!(before.len(), 3);
    // External deletion removes the third slot; "3" has no replacement, so
    // its tree parks under the logical name and recreation is requested.
    let actions = engine.apply_event(Event::Snapshot(topology(&[11, 22], 11, vec![])));
    assert_eq!(engine.workspaces.backing_for("3"), None);
    assert_eq!(creates(&actions), 1);
    assert!(
        !engine.layouts.contains_key(&SpaceId(33)),
        "a vanished Space must not keep a SpaceId-keyed tree"
    );
    // The created slot binds and the parked tree reattaches to it intact.
    let actions = engine.apply_event(Event::Snapshot(topology(&[11, 22, 44], 11, vec![])));
    assert_eq!(creates(&actions), 0);
    assert_mapping(&engine, &[("1", 11), ("2", 22), ("3", 44)]);
    let after = engine.layouts[&SpaceId(44)].bsp.placements(area, 8.0);
    assert_eq!(
        before, after,
        "workspace-owned layout must survive backing loss and rebind"
    );
}

#[test]
fn config_reload_preserves_inflight_creation_single_flight() {
    let mut engine = sparse();
    assert_eq!(creates(&engine.focus_workspace("4").unwrap()), 1);
    engine.reload_config(engine.config.clone());
    assert_eq!(
        engine.awaited_creations.len(),
        1,
        "reload must preserve the creation reservation"
    );
    assert_eq!(engine.pending_workspace_focus.len(), 1);
    // Unchanged snapshots still never duplicate after the reload.
    assert_eq!(
        creates(&engine.apply_event(Event::Snapshot(topology(
            &[11, 22, 55],
            22,
            vec![window_on(1, 22)]
        )))),
        0
    );
    // And the in-flight creation still binds.
    let actions = engine.apply_event(Event::Snapshot(topology(
        &[11, 22, 55, 44],
        22,
        vec![window_on(1, 22)],
    )));
    assert_eq!(creates(&actions), 0);
    assert_mapping(&engine, &[("4", 44)]);
}

#[test]
fn externally_moved_managed_window_is_corrected_back_to_bsp_frame() {
    let mut engine = configured(&["1"]);
    engine.capabilities.destroy_space = false;
    let first = engine.apply_event(Event::Snapshot(topology(&[11], 11, vec![window_on(1, 11)])));
    let tiled = first
        .iter()
        .find_map(|a| match a {
            Action::SetWindowFrame { window, frame } if *window == WindowId(1) => Some(*frame),
            _ => None,
        })
        .expect("managed window must be tiled");
    // External drag: the same managed, non-minimized window is observed at a
    // different native frame on the same Space. The BSP tree is
    // membership-only (never learns dragged geometry), so reconciliation
    // must emit the frame correction back to the tiled allocation.
    let mut moved = window_on(1, 11);
    moved.frame = Rect {
        x: 200.0,
        y: 200.0,
        width: 300.0,
        height: 300.0,
    };
    assert_ne!(moved.frame, tiled);
    let actions = engine.apply_event(Event::Snapshot(topology(&[11], 11, vec![moved])));
    assert!(
        actions.contains(&Action::SetWindowFrame {
            window: WindowId(1),
            frame: tiled
        }),
        "dragged tiled window must snap back to its BSP frame: {actions:?}"
    );
}

#[test]
fn stale_placement_must_not_permanently_block_window_movement() {
    let mut engine = configured(&["2", "5"]);
    engine.capabilities.create_space = true;
    engine.capabilities.destroy_space = true;
    engine.capabilities.reorder_space = true;
    engine.apply_event(Event::Snapshot(topology(
        &[22, 55],
        22,
        vec![window_on(1, 22)],
    )));
    assert_eq!(creates(&engine.focus_workspace("1").unwrap()), 1);
    engine.apply_event(Event::Snapshot(topology(
        &[22, 55, 11],
        22,
        vec![window_on(1, 22)],
    )));
    assert!(
        engine.placement.is_some(),
        "out-of-order insertion reserves placement"
    );
    // A move requested while placement is active is queued, not lost.
    let queued = engine
        .move_window_to_workspace(Some(WindowId(1)), "5")
        .unwrap();
    assert!(queued.is_empty());
    assert_eq!(engine.pending_workspace_move.len(), 1);
    // The placement goes stale (its MoveSpace never verified): the deadline
    // path must release it so the queued move still fires — at workspace 5's
    // current backing after observed order is accepted.
    engine.placement.as_mut().unwrap().deadline =
        std::time::Instant::now() - std::time::Duration::from_secs(1);
    engine.placement.as_mut().unwrap().retry_at =
        std::time::Instant::now() - std::time::Duration::from_secs(1);
    let actions = engine.apply_event(Event::Snapshot(topology(
        &[22, 55, 11],
        22,
        vec![window_on(1, 22)],
    )));
    assert!(engine.placement.is_none(), "stale placement must clear");
    let target = engine.workspaces.backing_for("5").expect("5 backed");
    assert!(
        actions.contains(&Action::MoveWindowToSpace {
            window: WindowId(1),
            space: target
        }),
        "queued move fires after placement release: {actions:?}"
    );
}

#[test]
fn reload_heals_external_frame_drift_immediately() {
    let mut engine = configured(&["1"]);
    engine.capabilities.destroy_space = false;
    engine.apply_event(Event::Snapshot(topology(&[11], 11, vec![window_on(1, 11)])));
    // External drag moves the tiled window off its BSP frame.
    let drifted = Rect {
        x: 200.0,
        y: 200.0,
        width: 300.0,
        height: 300.0,
    };
    let mut moved = window_on(1, 11);
    moved.frame = drifted;
    engine.apply_event(Event::Snapshot(topology(&[11], 11, vec![moved])));
    // Alt+Shift+R: same config, then the normal observation pipeline must
    // emit the correction with no Space switch or focus event involved.
    engine.reload_config(engine.config.clone());
    let tiled = engine.layouts[&SpaceId(11)]
        .bsp
        .placements(
            Rect {
                x: 0.0,
                y: 0.0,
                width: 1000.0,
                height: 800.0,
            },
            8.0,
        )
        .into_iter()
        .find(|(w, _)| *w == WindowId(1))
        .map(|(_, r)| r)
        .expect("tiled allocation");
    // Sanity: the drift is real (observed != desired).
    assert_ne!(drifted, tiled, "test setup must actually drift the window");
    let mut moved_again = window_on(1, 11);
    moved_again.frame = drifted;
    let actions = engine.apply_event(Event::Snapshot(topology(&[11], 11, vec![moved_again])));
    assert!(
        actions.contains(&Action::SetWindowFrame {
            window: WindowId(1),
            frame: tiled
        }),
        "reload must heal frame drift on the next observation: {actions:?}"
    );
}

#[test]
fn reload_preserves_fullscreen_restore_protection() {
    let mut engine = sparse();
    engine.apply_event(Event::Snapshot(topology(
        &[11, 22, 55],
        11,
        vec![window_on(1, 11)],
    )));
    engine.workspaces.0.get_mut("1").unwrap().persistent = false;
    engine.apply_event(Event::Snapshot(fullscreen_snapshot(99)));
    assert!(matches!(
        engine.workspaces.0["1"].backing,
        Some(WorkspaceBacking::FullscreenReplacement { .. })
    ));
    // Alt+Shift+R with equivalent config must not disturb the substitution.
    engine.reload_config(engine.config.clone());
    assert!(matches!(
        engine.workspaces.0["1"].backing,
        Some(WorkspaceBacking::FullscreenReplacement {
            fullscreen_space: SpaceId(99),
            ..
        })
    ));
    assert_eq!(engine.workspaces.0.len(), 3, "no workspace added for F");
    assert_eq!(
        engine.focus_workspace("1").unwrap(),
        vec![Action::FocusSpace { space: SpaceId(99) }],
        "Alt+1 still resolves to the fullscreen backing"
    );
    // Fullscreen exit afterwards still restores the protected Space.
    let actions = engine.apply_event(Event::Snapshot(topology(&[11, 22, 55], 22, vec![])));
    assert_eq!(
        engine.workspaces.0["1"].backing,
        Some(WorkspaceBacking::Normal { space: SpaceId(11) })
    );
    assert!(!actions.contains(&Action::DestroySpace { space: SpaceId(11) }));
}

#[test]
fn reload_config_cleans_only_removed_workspace_state() {
    let mut engine = configured(&["1", "2", "tmp"]);
    engine.apply_event(Event::Snapshot(topology(&[11, 22, 33], 11, vec![])));
    assert_mapping(&engine, &[("1", 11), ("2", 22), ("tmp", 33)]);
    for id in [1u32, 2] {
        engine
            .layouts
            .entry(SpaceId(11))
            .or_default()
            .bsp
            .insert(WindowId(id));
    }
    engine
        .layouts
        .entry(SpaceId(33))
        .or_default()
        .bsp
        .insert(WindowId(9));
    // Same-config reload: healthy BSP state and runtime registry survive.
    engine.reload_config(engine.config.clone());
    assert!(engine.workspaces.0.contains_key("tmp"));
    assert!(engine.layouts.contains_key(&SpaceId(11)));
    assert!(engine.layouts.contains_key(&SpaceId(33)));
    // Reload that actually removes "tmp": its SpaceId tree is cleaned, live
    // workspaces keep theirs.
    let ws = |name: &str| rovr_config::WorkspaceConfig {
        name: name.into(),
        layout: rovr_types::LayoutKind::Bsp,
        display: None,
        persistent: true,
        plugin: None,
    };
    engine.reload_config(Config {
        workspaces: vec![ws("1"), ws("2")],
        ..Default::default()
    });
    assert!(!engine.workspaces.0.contains_key("tmp"));
    assert!(
        !engine.layouts.contains_key(&SpaceId(33)),
        "removed workspace must not leak its SpaceId tree"
    );
    assert!(
        !engine.restored_workspace_layouts.contains_key("tmp"),
        "removed workspace must not leak a parked tree"
    );
    assert!(
        engine.layouts.contains_key(&SpaceId(11)),
        "surviving workspace keeps its tree"
    );
}
