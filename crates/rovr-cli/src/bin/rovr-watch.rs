//! `rovr-watch` — one line per state change on the rovr event stream.
//!
//! `rovr subscribe` prints the raw notification JSON. That is the right thing
//! for a machine consumer and the wrong thing for a human: `StateChanged`
//! carries no payload by design (it is a re-poll hint, not a revision), so the
//! interesting information only appears after a follow-up `query current`, and
//! the useful question — *what actually changed, and how long did it take* —
//! requires a diff against the previous observation.
//!
//! So this tool keeps one subscription open, re-queries the canonical snapshot
//! on every `StateChanged`, and prints only the fields that moved:
//!
//! ```text
//! +00:00.0  subscribed  (/Users/me/Library/Caches/rovr/daemon.sock)  protocol v1
//! +00:00.1  display 1   space 717   win 84679   pid 90846   app "Brave Browser"
//! +00:04.3  space 717→1501   win 84679→77760   app "Brave Browser"→"Ghostty"
//! +00:09.8  scratchpad  terminal open
//! ```
//!
//! The leading stamp is elapsed time since subscribe, not wall clock, because
//! the question this tool exists to answer is propagation delay — how long
//! after a Dock restart or a display reconnect does the engine settle. Gaps
//! between lines are directly readable; no clock formatting and no timezone.
//!
//! Read-only against the daemon: it subscribes and queries, never mutates.
//! Each line is one bounded read, so a wedged daemon cannot grow memory here.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use rovr_protocol::{Command, Notification, QueryCommand, Request, Response, ResponseOutcome};
use serde_json::Value;

/// Upper bound on a single notification line. The daemon caps its own
/// responses well below this; the bound exists so a hostile or wedged peer
/// cannot make us allocate without limit.
const MAX_LINE: u64 = 1024 * 1024;
const DEADLINE: Duration = Duration::from_secs(5);

/// Truncation width for free-form strings (window titles are unbounded).
const TEXT_WIDTH: usize = 48;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Live one-line-per-change view of the rovr event stream.
#[derive(Debug, Parser)]
#[command(name = "rovr-watch", version, about)]
struct Args {
    /// Daemon socket. Defaults to the standard per-user location.
    #[arg(long)]
    socket: Option<PathBuf>,
    /// Also print heartbeat ticks. Silent by default: heartbeats are liveness,
    /// not state, and at one per tick they would bury the changes.
    #[arg(long)]
    heartbeats: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let socket = args
        .socket
        .unwrap_or_else(rovr_platform::daemon_socket_path);
    let start = Instant::now();

    let (mut reader, protocol_version) = subscribe(&socket)?;
    println!(
        "{}  subscribed  ({})  protocol v{protocol_version}",
        stamp(start),
        socket.display()
    );

    let mut previous: Option<Snapshot> = None;
    loop {
        let Some(line) = read_line(&mut reader)? else {
            break;
        };
        if line.trim().is_empty() {
            continue;
        }
        let notification: Notification = match serde_json::from_str(line.trim()) {
            Ok(notification) => notification,
            Err(error) => {
                eprintln!("{}  undecodable notification: {error}", stamp(start));
                continue;
            }
        };

        match notification {
            // The ACK already reported the protocol version, so the Hello frame
            // carries nothing new. Ignored to keep the header one line.
            Notification::Hello { .. } => {}
            Notification::Heartbeat { .. } => {
                if args.heartbeats {
                    println!("{}  heartbeat", stamp(start));
                }
            }
            // The only variant that requires work: the payload is empty, so the
            // actual state must be fetched. A failed fetch is reported and the
            // previous snapshot is kept, so one transient error does not look
            // like a burst of spurious changes on the next successful poll.
            Notification::StateChanged => match query_current(&socket) {
                Ok(next) => {
                    match &previous {
                        None => println!("{}  {}", stamp(start), render(&next)),
                        Some(previous) => {
                            let changes = diff(previous, &next);
                            if !changes.is_empty() {
                                println!("{}  {}", stamp(start), changes.join("   "));
                            }
                        }
                    }
                    previous = Some(next);
                }
                Err(error) => eprintln!("{}  query current failed: {error:#}", stamp(start)),
            },
            Notification::LayoutChanged {
                space,
                horizontal,
                reversed,
            } => println!(
                "{}  layout  space {} {}",
                stamp(start),
                space.0,
                match (horizontal, reversed) {
                    (true, false) => "horizontal",
                    (true, true) => "horizontal (reversed)",
                    (false, false) => "vertical",
                    (false, true) => "vertical (reversed)",
                }
            ),
            Notification::ScratchpadToggled { name, open } => println!(
                "{}  scratchpad  {} {}",
                stamp(start),
                name,
                if open { "open" } else { "closed" }
            ),
            Notification::ConfigReloaded => println!("{}  config reloaded", stamp(start)),
            // Forward compatibility: a newer daemon may send variants this
            // build does not know. Ignore rather than fail the stream.
            Notification::Unknown => {}
        }
    }

    println!("{}  stream closed", stamp(start));
    Ok(())
}

/// The fields of `rovr query current` that are worth diffing. Everything is
/// optional: a Space with no focused window legitimately reports empty, and
/// that absence is a change worth printing, so `None` must stay distinct from
/// an empty string.
#[derive(Clone, Default)]
struct Snapshot {
    display: Option<i64>,
    space: Option<i64>,
    window: Option<i64>,
    pid: Option<i64>,
    app: Option<String>,
    title: Option<String>,
}

/// Field order is fixed so diffs read consistently across lines.
const LABELS: [&str; 6] = ["display", "space", "win", "pid", "app", "title"];

impl Snapshot {
    fn from_value(value: &Value) -> Self {
        let window = value.get("window");
        let text = |key: &str| {
            window
                .and_then(|window| window.get(key))
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
        };
        Self {
            display: value.get("display").and_then(Value::as_i64),
            space: value.get("space").and_then(Value::as_i64),
            window: window.and_then(|w| w.get("id")).and_then(Value::as_i64),
            pid: window.and_then(|w| w.get("pid")).and_then(Value::as_i64),
            app: text("app"),
            title: text("title"),
        }
    }

    fn get(&self, label: &str) -> Option<String> {
        match label {
            "display" => self.display.map(|v| v.to_string()),
            "space" => self.space.map(|v| v.to_string()),
            "win" => self.window.map(|v| v.to_string()),
            "pid" => self.pid.map(|v| v.to_string()),
            "app" => self.app.as_deref().map(quote),
            "title" => self.title.as_deref().map(quote),
            _ => None,
        }
    }
}

/// First observation: every present field, labelled.
fn render(snapshot: &Snapshot) -> String {
    let fields: Vec<String> = LABELS
        .iter()
        .filter_map(|label| snapshot.get(label).map(|value| format!("{label} {value}")))
        .collect();
    if fields.is_empty() {
        "(nothing observed yet)".to_owned()
    } else {
        fields.join("   ")
    }
}

/// Changed fields only, as `label old→new`. A field that disappeared prints as
/// `old→—`, because "focus left the desktop" is exactly the event a watcher
/// must not silently drop.
fn diff(previous: &Snapshot, next: &Snapshot) -> Vec<String> {
    LABELS
        .iter()
        .filter_map(|label| {
            let before = previous.get(label);
            let after = next.get(label);
            if before == after {
                return None;
            }
            Some(match (before, after) {
                (Some(before), Some(after)) => format!("{label} {before}→{after}"),
                (Some(before), None) => format!("{label} {before}→—"),
                (None, Some(after)) => format!("{label} {after}"),
                (None, None) => return None,
            })
        })
        .collect()
}

/// Quote and truncate free-form text so one long window title cannot wrap the
/// line or flood the terminal.
fn quote(text: &str) -> String {
    let mut escaped = text.replace('\n', " ");
    if escaped.chars().count() > TEXT_WIDTH {
        escaped = escaped.chars().take(TEXT_WIDTH).collect::<String>() + "…";
    }
    format!("\"{escaped}\"")
}

/// Elapsed time since subscribe, as `+mm:ss.d`.
fn stamp(start: Instant) -> String {
    let elapsed = start.elapsed();
    format!(
        "+{:02}:{:02}.{:01}",
        elapsed.as_secs() / 60,
        elapsed.as_secs() % 60,
        elapsed.subsec_millis() / 100
    )
}

/// Open a subscription and consume its ACK, returning the stream and the
/// protocol version the ACK reported. The ACK is the first line and carries
/// success or rejection; the stream is not usable until it is read.
fn subscribe(socket: &Path) -> Result<(BufReader<UnixStream>, u16)> {
    let mut stream = UnixStream::connect(socket)
        .with_context(|| format!("connect to rovr daemon at {}", socket.display()))?;
    stream.set_read_timeout(Some(DEADLINE))?;
    stream.set_write_timeout(Some(DEADLINE))?;
    let request = Request::new(NEXT_ID.fetch_add(1, Ordering::Relaxed), Command::Subscribe);
    serde_json::to_writer(&mut stream, &request)?;
    stream.write_all(b"\n")?;
    stream.flush()?;

    let mut reader = BufReader::new(stream);
    let line = read_line(&mut reader)?
        .ok_or_else(|| anyhow!("daemon closed the stream before the subscription ACK"))?;
    let ack: Response = serde_json::from_str(line.trim()).context("decode subscription ACK")?;
    match ack.outcome {
        ResponseOutcome::Ok { .. } => {}
        ResponseOutcome::Error { error } => {
            return Err(anyhow!(
                "subscription rejected ({}): {}",
                error.code,
                error.message
            ));
        }
    }
    // Streaming reads block indefinitely: waiting for the next event is the
    // point, and a heartbeated stream still ends with EOF if the daemon dies.
    reader.get_mut().set_read_timeout(None)?;
    Ok((reader, ack.version))
}

/// One `query current` on its own short-lived connection. Reconnecting per
/// query means a dead or restarted daemon surfaces as a normal connect error
/// instead of a stale socket that looks alive.
fn query_current(socket: &Path) -> Result<Snapshot> {
    let mut stream = UnixStream::connect(socket)
        .with_context(|| format!("connect to rovr daemon at {}", socket.display()))?;
    stream.set_read_timeout(Some(DEADLINE))?;
    stream.set_write_timeout(Some(DEADLINE))?;
    let request = Request::new(
        NEXT_ID.fetch_add(1, Ordering::Relaxed),
        Command::Query(QueryCommand::Current),
    );
    serde_json::to_writer(&mut stream, &request)?;
    stream.write_all(b"\n")?;
    stream.flush()?;

    let mut reader = BufReader::new(stream);
    let line = read_line(&mut reader)?
        .ok_or_else(|| anyhow!("daemon closed before responding to query current"))?;
    let response: Response = serde_json::from_str(line.trim()).context("decode query response")?;
    match response.outcome {
        ResponseOutcome::Ok { result } => Ok(Snapshot::from_value(&result)),
        ResponseOutcome::Error { error } => Err(anyhow!(
            "query current failed ({}): {}",
            error.code,
            error.message
        )),
    }
}

/// Read one newline-terminated line, bounded. `Ok(None)` is a clean EOF.
fn read_line(reader: &mut impl BufRead) -> Result<Option<String>> {
    let mut line = String::new();
    let bytes = reader.by_ref().take(MAX_LINE + 1).read_line(&mut line)?;
    if bytes == 0 {
        return Ok(None);
    }
    if line.len() as u64 > MAX_LINE {
        anyhow::bail!("peer sent a line larger than {MAX_LINE} bytes");
    }
    Ok(Some(line))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(space: i64, app: Option<&str>, title: Option<&str>) -> Snapshot {
        Snapshot {
            display: Some(1),
            space: Some(space),
            window: Some(7),
            pid: Some(70),
            app: app.map(str::to_owned),
            title: title.map(str::to_owned),
        }
    }

    #[test]
    fn parses_the_current_query_shape_and_drops_empty_text() {
        let value = serde_json::json!({
            "display": 1,
            "space": 717,
            "window": { "app": "Brave Browser", "id": 84679, "pid": 90846, "title": "" }
        });
        let snapshot = Snapshot::from_value(&value);
        assert_eq!(snapshot.display, Some(1));
        assert_eq!(snapshot.space, Some(717));
        assert_eq!(snapshot.window, Some(84679));
        assert_eq!(snapshot.pid, Some(90846));
        assert_eq!(snapshot.app.as_deref(), Some("Brave Browser"));
        assert_eq!(snapshot.title, None, "an empty title is absence, not text");
    }

    #[test]
    fn diff_reports_only_what_moved() {
        let before = snapshot(717, Some("Brave Browser"), None);
        let after = snapshot(1501, Some("Brave Browser"), None);
        assert_eq!(diff(&before, &after), vec!["space 717→1501"]);
    }

    #[test]
    fn diff_reports_losing_focus_rather_than_dropping_it() {
        let before = snapshot(717, Some("Brave Browser"), Some("GitHub"));
        let after = Snapshot {
            display: Some(1),
            space: Some(717),
            window: None,
            pid: None,
            app: None,
            title: None,
        };
        assert_eq!(
            diff(&before, &after),
            vec![
                "win 7→—".to_owned(),
                "pid 70→—".to_owned(),
                "app \"Brave Browser\"→—".to_owned(),
                "title \"GitHub\"→—".to_owned(),
            ]
        );
    }

    #[test]
    fn diff_is_empty_when_nothing_changed() {
        let before = snapshot(1, Some("X"), None);
        assert!(diff(&before, &before).is_empty());
    }

    #[test]
    fn render_labels_every_present_field_in_stable_order() {
        let rendered = render(&snapshot(717, Some("Ghostty"), Some("~/src/rovr")));
        assert_eq!(
            rendered,
            "display 1   space 717   win 7   pid 70   app \"Ghostty\"   title \"~/src/rovr\""
        );
    }

    #[test]
    fn long_titles_are_truncated_and_newlines_removed() {
        let long = "x".repeat(TEXT_WIDTH + 20);
        assert_eq!(quote(&long).chars().count(), TEXT_WIDTH + 3); // quotes + ellipsis
        assert_eq!(quote("two\nlines"), "\"two lines\"");
    }

    #[test]
    fn notification_variants_decode_from_their_wire_form() {
        let hello: Notification =
            serde_json::from_str(r#"{"type":"hello","protocol_version":1}"#).unwrap();
        assert_eq!(
            hello,
            Notification::Hello {
                protocol_version: 1
            }
        );
        let changed: Notification = serde_json::from_str(r#"{"type":"state_changed"}"#).unwrap();
        assert_eq!(changed, Notification::StateChanged);
        let layout: Notification = serde_json::from_str(
            r#"{"type":"layout_changed","space":9,"horizontal":true,"reversed":false}"#,
        )
        .unwrap();
        assert_eq!(
            layout,
            Notification::LayoutChanged {
                space: rovr_types::SpaceId(9),
                horizontal: true,
                reversed: false
            }
        );
    }

    #[test]
    fn bounded_read_stops_at_a_clean_eof() {
        let mut reader = BufReader::new(&b""[..]);
        assert!(read_line(&mut reader).unwrap().is_none());
    }

    #[test]
    fn bounded_read_rejects_an_oversized_line() {
        let payload = "y".repeat(MAX_LINE as usize + 8);
        let mut reader = BufReader::new(payload.as_bytes());
        assert!(read_line(&mut reader).is_err());
    }
}
