//! What this machine's agents are doing, moment to moment.
//!
//! Hooks append one line per event to a spool under `$ROBOFINGER_HOME/activity`,
//! and `serve` reads it back as the status line each agent shows. Nothing here
//! touches the network: a tool call is too frequent an event to spend the
//! relay's write budget on, and a hook must never be what an agent waits on.
//!
//! One file per top-level session slot (`claude-1.jsonl`), holding that agent
//! and every subagent under it. Slots are reused, so a reader takes an agent's
//! events from its last `appear` on.

use std::io::Write;
use std::path::{Path, PathBuf};

/// Lines kept per file once it grows past `MAX_BYTES`. Enough for a page's
/// worth of history per agent; the timeline is the durable record.
const KEEP_LINES: usize = 400;
const MAX_BYTES: u64 = 128 * 1024;
/// A spool file nobody has written to for this long belongs to a session that
/// is over, however it ended.
const STALE: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

/// One thing an agent did.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub t: i64,
    /// The instance: `claude-1`, or `claude-1/explore-2` for a subagent.
    pub agent: String,
    /// appear, prompt, tool, idle, waiting, stop, done or gone.
    pub kind: String,
    pub tool: String,
    pub target: String,
}

pub fn dir(home: &Path) -> PathBuf {
    home.join("activity")
}

/// Append one event. Best effort: a read-only home costs the live view, never
/// the session.
pub fn append(dir: &Path, e: &Event) {
    let top = e
        .agent
        .split('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("_");
    let path = dir.join(format!("{top}.jsonl"));
    let line = serde_json::json!({
        "t": e.t, "agent": e.agent, "kind": e.kind, "tool": e.tool, "target": e.target,
    });
    let _ = std::fs::create_dir_all(dir);
    // One short write with O_APPEND: concurrent hooks interleave whole lines.
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "{line}");
    }
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_BYTES) {
        trim(&path);
    }
}

fn trim(path: &Path) {
    let Ok(body) = std::fs::read_to_string(path) else {
        return;
    };
    let lines: Vec<&str> = body.lines().collect();
    let keep = &lines[lines.len().saturating_sub(KEEP_LINES)..];
    let _ = std::fs::write(path, keep.join("\n") + "\n");
}

/// Delete spool files nobody has written to in a day.
pub fn prune(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| m.elapsed().ok())
            .is_some_and(|age| age > STALE);
        if old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Every event in the spool, oldest first.
pub fn read(dir: &Path) -> Vec<Event> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in entries.flatten() {
        let Ok(body) = std::fs::read_to_string(e.path()) else {
            continue;
        };
        for line in body.lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
            let Some(t) = v.get("t").and_then(|x| x.as_i64()) else {
                continue;
            };
            out.push(Event {
                t,
                agent: s("agent"),
                kind: s("kind"),
                tool: s("tool"),
                target: s("target"),
            });
        }
    }
    out.sort_by_key(|e| e.t);
    out
}

/// The tool and what it was pointed at, from PreToolUse input. Short, and with
/// paths made relative to the repo, because it is shown in a status line.
pub fn describe(input: &serde_json::Value) -> (String, String) {
    let tool = input
        .get("tool_name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let arg = |k: &str| {
        input
            .pointer(&format!("/tool_input/{k}"))
            .and_then(|v| v.as_str())
    };
    let cwd = input.get("cwd").and_then(|v| v.as_str()).unwrap_or("");
    let rel = |p: &str| {
        p.strip_prefix(cwd)
            .map(|r| r.trim_start_matches('/'))
            .filter(|r| !cwd.is_empty() && !r.is_empty())
            .unwrap_or(p)
            .to_string()
    };
    let target = match tool.as_str() {
        "Bash" => arg("command").map(|c| c.lines().next().unwrap_or("").to_string()),
        "Edit" | "MultiEdit" | "Write" | "Read" => arg("file_path").map(rel),
        "NotebookEdit" | "NotebookRead" => arg("notebook_path").map(rel),
        "Grep" | "Glob" => arg("pattern").map(String::from),
        "Task" | "Agent" => arg("description")
            .or_else(|| arg("subagent_type"))
            .map(String::from),
        "WebFetch" => arg("url").map(|u| u.split('/').nth(2).unwrap_or(u).to_string()),
        "WebSearch" => arg("query").map(String::from),
        _ => None,
    };
    (tool, short(&target.unwrap_or_default(), 72))
}

fn short(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        None => s.to_string(),
        Some((i, _)) => format!("{}…", &s[..i]),
    }
}

/// What an agent is doing, as the line under Claude Code's input box would
/// put it.
#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Working,
    Thinking,
    /// Blocked on its person: a permission prompt or a question.
    NeedsYou,
    /// Finished its turn; the next move is its person's.
    YourTurn,
    Done,
    Gone,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    pub state: State,
    pub text: String,
    pub since: i64,
}

/// An agent's current status from its own events, oldest first. None when it
/// has none since it last appeared.
pub fn status(events: &[&Event]) -> Option<Status> {
    let from = events.iter().rposition(|e| e.kind == "appear").unwrap_or(0);
    let last = events.get(from..)?.last()?;
    let (state, text) = match last.kind.as_str() {
        "tool" => (State::Working, doing(&last.tool, &last.target)),
        "waiting" if !last.target.is_empty() => {
            (State::NeedsYou, format!("Needs you: {}", last.target))
        }
        "waiting" => (State::NeedsYou, "Needs you".into()),
        "stop" => (State::YourTurn, "Your turn".into()),
        "done" => (State::Done, "Done".into()),
        "gone" => (State::Gone, "Ended".into()),
        "appear" => (State::Thinking, "Starting".into()),
        _ => (State::Thinking, "Thinking".into()),
    };
    Some(Status {
        state,
        text,
        since: last.t,
    })
}

/// "Running cargo test", "Editing src/main.rs".
pub fn doing(tool: &str, target: &str) -> String {
    let verb = match tool {
        "Bash" => "Running",
        "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => "Editing",
        "Read" | "NotebookRead" => "Reading",
        "Grep" | "Glob" | "LS" => "Searching",
        "Task" | "Agent" => "Delegating",
        "WebFetch" | "WebSearch" => "Browsing",
        "TodoWrite" => "Planning",
        "" => "Working",
        other => other,
    };
    match target {
        "" => verb.to_string(),
        t => format!("{verb} {t}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(t: i64, agent: &str, kind: &str, tool: &str, target: &str) -> Event {
        Event {
            t,
            agent: agent.into(),
            kind: kind.into(),
            tool: tool.into(),
            target: target.into(),
        }
    }

    #[test]
    fn a_session_and_its_subagent_share_a_file_and_read_back_in_order() {
        let home = std::env::temp_dir().join(format!("rf-activity-{}", std::process::id()));
        let d = dir(&home);
        let _ = std::fs::remove_dir_all(&d);
        append(
            &d,
            &ev(2, "claude-1/explore-1", "tool", "Read", "src/main.rs"),
        );
        append(&d, &ev(1, "claude-1", "appear", "", ""));
        append(&d, &ev(3, "claude-2", "stop", "", ""));
        let files: Vec<_> = std::fs::read_dir(&d)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(files.len(), 2, "one file per top-level slot: {files:?}");
        let all = read(&d);
        assert_eq!(all.iter().map(|e| e.t).collect::<Vec<_>>(), vec![1, 2, 3]);
        assert_eq!(all[1].agent, "claude-1/explore-1");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_full_file_keeps_its_newest_lines() {
        let home = std::env::temp_dir().join(format!("rf-activity-trim-{}", std::process::id()));
        let d = dir(&home);
        let _ = std::fs::remove_dir_all(&d);
        let long = "x".repeat(400);
        for t in 0..500 {
            append(&d, &ev(t, "claude-1", "tool", "Bash", &long));
        }
        let all = read(&d);
        assert!(all.len() <= KEEP_LINES + 400, "{}", all.len());
        assert_eq!(all.last().unwrap().t, 499);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn status_reads_like_the_line_under_the_input_box() {
        let es = [
            ev(1, "a", "appear", "", ""),
            ev(2, "a", "tool", "Bash", "cargo test"),
        ];
        let refs: Vec<&Event> = es.iter().collect();
        let s = status(&refs).unwrap();
        assert_eq!(
            (s.state, s.text.as_str(), s.since),
            (State::Working, "Running cargo test", 2)
        );

        let es = [ev(1, "a", "tool", "Edit", "x"), ev(5, "a", "idle", "", "")];
        let refs: Vec<&Event> = es.iter().collect();
        assert_eq!(status(&refs).unwrap().state, State::Thinking);

        let es = [ev(
            1,
            "a",
            "waiting",
            "",
            "Claude needs your permission to use Bash",
        )];
        let refs: Vec<&Event> = es.iter().collect();
        assert_eq!(
            status(&refs).unwrap().text,
            "Needs you: Claude needs your permission to use Bash"
        );
    }

    #[test]
    fn a_reused_slot_starts_from_its_last_appearance() {
        let es = [
            ev(1, "a", "tool", "Bash", "old"),
            ev(2, "a", "gone", "", ""),
            ev(9, "a", "appear", "", ""),
        ];
        let refs: Vec<&Event> = es.iter().collect();
        let s = status(&refs).unwrap();
        assert_eq!((s.state, s.since), (State::Thinking, 9));
    }

    #[test]
    fn describe_keeps_targets_short_and_relative() {
        let input = serde_json::json!({
            "tool_name": "Edit", "cwd": "/src/repo",
            "tool_input": { "file_path": "/src/repo/client/src/main.rs" }
        });
        assert_eq!(
            describe(&input),
            ("Edit".into(), "client/src/main.rs".into())
        );
        let input = serde_json::json!({
            "tool_name": "Bash",
            "tool_input": { "command": format!("cargo test {}\necho done", "x".repeat(100)) }
        });
        let (_, target) = describe(&input);
        assert!(
            target.starts_with("cargo test") && target.ends_with('…') && !target.contains("echo")
        );
    }
}
