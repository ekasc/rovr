//! Native SketchyBar trigger payload.
//!
//! Pure state-to-payload conversion for the low-latency status path:
//! `PublicState` -> `--trigger` argv -> NUL-joined Mach message bytes.
//! No subprocesses, no shell, no Mach code here — the platform layer owns
//! the transport (`Platform::sketchybar_trigger`), this module owns the
//! bytes. Heavily tested; the actual Mach send is deliberately thin.

use rovr_types::{PublicState, PublicWindow};

/// Custom event name SketchyBar items subscribe to (`rovr:subscribe(...)`
/// in SbarLua, `--subscribe <item> rovr_state` in shell configs).
pub const SKETCHYBAR_EVENT: &str = "rovr_state";

/// Public native event contract: variable names carried on `rovr_state`.
/// Stable — SketchyBar items read these as `env.SPACE`, etc.
pub const VAR_SPACE: &str = "SPACE";
pub const VAR_DISPLAY: &str = "DISPLAY";
pub const VAR_WINDOW_ID: &str = "WINDOW_ID";
pub const VAR_PID: &str = "PID";
pub const VAR_APP: &str = "APP";
pub const VAR_TITLE: &str = "TITLE";

/// Replacement for NUL bytes in app/title strings. NUL cannot cross the
/// Mach boundary (SketchyBar splits the message into C strings), so it is
/// swapped for U+FFFD here. Everything else — spaces, quotes, `$`, `;`,
/// backticks, unicode, emoji — passes through literally: this is not a
/// shell command and must never be shell-quoted.
pub const NUL_REPLACEMENT: char = '\u{FFFD}';

fn clean(value: &str) -> String {
    if value.contains('\0') {
        value.replace('\0', &NUL_REPLACEMENT.to_string())
    } else {
        value.to_string()
    }
}

/// Trigger argv for a snapshot: `--trigger rovr_state SPACE=.. ...`.
/// Values are the exact `PublicState` fields (raw ROVR ids); absent fields
/// encode as empty strings. Presentation (e.g. a "Desktop" label) belongs
/// to SketchyBar, never here.
pub fn trigger_args(state: &PublicState) -> Vec<String> {
    let (space, display) = (
        state.space.map(|id| id.0.to_string()).unwrap_or_default(),
        state.display.map(|id| id.0.to_string()).unwrap_or_default(),
    );
    let (window_id, pid, app, title) = match &state.window {
        Some(PublicWindow {
            id,
            pid,
            app,
            title,
        }) => (
            id.0.to_string(),
            pid.0.to_string(),
            clean(app),
            clean(title),
        ),
        None => (String::new(), String::new(), String::new(), String::new()),
    };
    vec![
        "--trigger".to_string(),
        SKETCHYBAR_EVENT.to_string(),
        format!("{VAR_SPACE}={space}"),
        format!("{VAR_DISPLAY}={display}"),
        format!("{VAR_WINDOW_ID}={window_id}"),
        format!("{VAR_PID}={pid}"),
        format!("{VAR_APP}={app}"),
        format!("{VAR_TITLE}={title}"),
    ]
}

/// Encode argv exactly the way SketchyBar's own CLI does (`src/sketchybar.c`):
/// each arg NUL-terminated plus one trailing NUL. The bridge sends these
/// bytes as the out-of-line Mach message; SketchyBar splits them back into
/// tokens server-side.
pub fn encode_mach_message(args: &[String]) -> Vec<u8> {
    let mut message = Vec::new();
    for arg in args {
        message.extend_from_slice(arg.as_bytes());
        message.push(0);
    }
    message.push(0);
    message
}

#[cfg(test)]
mod tests {
    use super::*;
    use rovr_types::{DisplayId, ProcessId, SpaceId, WindowId};

    fn state(
        space: Option<u64>,
        display: Option<u32>,
        window: Option<(u32, i32, &str, &str)>,
    ) -> PublicState {
        PublicState {
            space: space.map(SpaceId),
            display: display.map(DisplayId),
            window: window.map(|(id, pid, app, title)| PublicWindow {
                id: WindowId(id),
                pid: ProcessId(pid),
                app: app.to_string(),
                title: title.to_string(),
            }),
        }
    }

    fn decode(message: &[u8]) -> Vec<String> {
        // Mirror of SketchyBar's server-side split: NUL-separated tokens.
        // The encoding ends each arg with NUL plus one extra terminator,
        // so the split yields exactly two trailing empty segments.
        let mut tokens: Vec<String> = message
            .split(|b| *b == 0)
            .map(|part| String::from_utf8(part.to_vec()).unwrap())
            .collect();
        assert_eq!(tokens.pop().as_deref(), Some(""));
        assert_eq!(tokens.pop().as_deref(), Some(""));
        tokens
    }

    #[test]
    fn full_state_encodes_stable_contract() {
        assert_eq!(SKETCHYBAR_EVENT, "rovr_state");
        let args = trigger_args(&state(
            Some(1098),
            Some(2),
            Some((482, 1234, "Ghostty", "~/src/rovr")),
        ));
        assert_eq!(
            args,
            vec![
                "--trigger",
                "rovr_state",
                "SPACE=1098",
                "DISPLAY=2",
                "WINDOW_ID=482",
                "PID=1234",
                "APP=Ghostty",
                "TITLE=~/src/rovr",
            ]
        );
        // Round-trips through the Mach encoding untouched.
        assert_eq!(decode(&encode_mach_message(&args)), args);
    }

    #[test]
    fn focus_loss_encodes_empty_window_fields() {
        let args = trigger_args(&state(Some(1098), Some(2), None));
        assert_eq!(
            &args[2..],
            &[
                "SPACE=1098",
                "DISPLAY=2",
                "WINDOW_ID=",
                "PID=",
                "APP=",
                "TITLE=",
            ]
        );
        assert!(!args.iter().any(|arg| arg.contains("Desktop")));
    }

    #[test]
    fn absent_space_and_display_encode_empty() {
        let args = trigger_args(&state(None, None, None));
        assert_eq!(&args[2..4], &["SPACE=", "DISPLAY="]);
    }

    #[test]
    fn unicode_and_emoji_survive_literally() {
        let args = trigger_args(&state(
            Some(3),
            Some(1),
            Some((7, 8, "Fünf 🖥️ app", "café — 日本語 🚀")),
        ));
        assert!(args.contains(&"APP=Fünf 🖥️ app".to_string()));
        assert!(args.contains(&"TITLE=café — 日本語 🚀".to_string()));
        assert_eq!(decode(&encode_mach_message(&args)), args);
    }

    #[test]
    fn shell_looking_characters_are_preserved_literally() {
        let title = "$(rm -rf ~); echo `id` | tee & || && 'quoted' \"dq\" $HOME \\ back";
        let args = trigger_args(&state(Some(3), Some(1), Some((7, 8, "sh", title))));
        let expected = format!("TITLE={title}");
        assert!(args.contains(&expected));
        // Byte-identical after Mach encoding: never shell-quoted.
        assert_eq!(decode(&encode_mach_message(&args)), args);
    }

    #[test]
    fn values_containing_equals_survive() {
        // SketchyBar splits each token on the FIRST '=' server-side.
        let args = trigger_args(&state(Some(3), Some(1), Some((7, 8, "app", "a=b=c"))));
        assert!(args.contains(&"TITLE=a=b=c".to_string()));
    }

    #[test]
    fn nul_bytes_are_replaced_never_truncated() {
        let args = trigger_args(&state(Some(3), Some(1), Some((7, 8, "a\0b", "x\0y"))));
        let message = encode_mach_message(&args);
        // Interior NULs would corrupt the token stream; only separators
        // and the terminator may remain.
        assert_eq!(message.iter().filter(|b| **b == 0).count(), args.len() + 1);
        let decoded = decode(&message);
        assert!(decoded.contains(&"APP=a\u{FFFD}b".to_string()));
        assert!(decoded.contains(&"TITLE=x\u{FFFD}y".to_string()));
    }

    #[test]
    fn changed_space_focus_title_each_change_encoding() {
        let base = state(Some(3), Some(1), Some((1, 10, "A", "one")));
        let space_moved = state(Some(4), Some(1), Some((1, 10, "A", "one")));
        let focus_moved = state(Some(3), Some(1), Some((2, 20, "B", "two")));
        let title_changed = state(Some(3), Some(1), Some((1, 10, "A", "uno")));
        let focus_lost = state(Some(3), Some(1), None);
        for changed in [&space_moved, &focus_moved, &title_changed, &focus_lost] {
            assert_ne!(
                trigger_args(&base),
                trigger_args(changed),
                "each visible transition must produce distinct bytes"
            );
        }
        // ...while the source dedup (PublicState equality) suppresses the rest.
        assert_eq!(trigger_args(&base), trigger_args(&base.clone()));
    }

    #[test]
    fn encoding_has_no_envelope_or_shell_wrapping() {
        let args = trigger_args(&state(Some(3), Some(1), Some((1, 2, "A", "t"))));
        let message = encode_mach_message(&args);
        assert!(message.ends_with(&[0, 0]));
        assert!(!message.windows(2).any(|w| w == [b'$', b'(']));
        let text = String::from_utf8(message).unwrap();
        assert!(!text.contains("sketchybar"));
        assert!(!text.contains("jq"));
    }
}
