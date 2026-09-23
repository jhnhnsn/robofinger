//! Claude Code hook installation for `robofinger init`.
//!
//! Merges three hooks into `~/.claude/settings.json`:
//!   SessionStart  — surface live peer claims into the agent's context
//!   SessionEnd    — mark this agent's work done
//!   PreToolUse    — warn before editing a path a peer has claimed
//!
//! Unlike envstow's skill install (which writes a fresh file), this edits a
//! config the user already owns, so it backs up first, never clobbers unrelated
//! keys, and is idempotent.

use std::path::PathBuf;

/// The workflow file written into a repo by `init` and `hooks install`.
///
/// A file rather than a string the user must paste somewhere: the paste step
/// was unenforced and invisible when skipped, and an agent that never reads it
/// never claims — so `touching` stays empty, every peer's check passes
/// trivially, and the tool silently does nothing while looking like it works.
///
/// Named ROBOFINGER.md rather than appended to CLAUDE.md so it is
/// agent-agnostic, survives the user rewriting their own memory file, and can
/// be committed for the team. The MARKER section is regenerated on upgrade;
/// everything below "Team conventions" is the user's and is never touched.
pub const ROBOFINGER_MD: &str = r#"# robofinger — how this repo coordinates agents

<!-- managed by robofinger; edits above "Team conventions" are overwritten on upgrade -->

Several coding agents work in this repo, on different machines. None of them
can see what the others are doing. robofinger is how they tell each other —
what they are touching now, what they have just finished, and anything the
others need to know. Git says all of it eventually; this says it in time to
matter.

**Everything here is advisory. Nothing blocks, nothing locks, nothing waits on
a server.** A warning you ignore costs a merge conflict, not a deadlock.

## What to do, in order

**1. Before editing files, claim them.**

```sh
robofinger claim "<short task description>" '<glob>' '<glob>'
```

Globs are relative to the repo root. A claim *replaces* your previous one
rather than adding to it, so pass every path you are still working on. This is
the step everything else depends on: without it you publish nothing, and every
teammate's conflict check passes trivially.

**2. If the claim looks contentious, say so before you start.**

```sh
robofinger ask --to <peer> "I'm about to take src/auth for ~an hour. Does that
  cut across what you're doing, or shall I start with the API layer?"
```

Cheapest coordination there is: a question before the work costs one line, the
same question after costs somebody their afternoon. Worth doing when a peer
released the path recently, when the claim is broad, or when you are about to
change something everything else imports.

**3. When you are done, release with what actually happened.**

```sh
robofinger release --note "dual-write landed, rollback is a flag"
```

The claim said what you intended. The note says what you did — it is what a
teammate reads to decide whether they can proceed. Claims expire on their own
after roughly an hour, so a forgotten release is not fatal, just unhelpful.

## The timeline

Claims are ephemeral — they say who holds what *right now*, and release drops
them. Every claim and release is also written to a timeline that persists, and
that record is half of what this tool is for.

**Read it.** The SessionStart hook runs `since` for you at the top of a
session; run it again after a long stretch of work.

```sh
robofinger since          # what changed since you last looked; advances a cursor
robofinger list           # who is holding what right now
robofinger log --since 2h # a window, without moving the cursor
```

`since` *consumes*: it advances a cursor, so what it shows you it will not show
again. Use `log --since` when you want to look without spending it.

**Write to it.** A note costs seconds and saves a teammate the round trip of
asking.

```sh
robofinger post "auth migration is blocked on the schema change landing first"
robofinger post "the retry backoff was wrong for 5xx, not just timeouts — fixed"
```

Worth a post: something you learned that changes what a teammate would do, a
handoff, a decision that is not obvious from the diff. Not worth a post: what
the commit already says.

**Be brief.** Entries are capped at 140 characters — a tweet, not a summary.
Say the one thing a teammate could not work out from the diff, and let the
commit carry the detail. An entry that gets truncated was the wrong entry.

## Talking to the other agents

```sh
robofinger ask --to <peer> "both of us want src/auth. I can take the API layer
  instead, or wait for your release. Which?"
robofinger answer --to <peer> --re <id> "take the API layer; auth frees ~20m"
```

Without `--to` the whole team sees it; with it, that agent gets it marked
FOR YOU at its next session start. An `ask` is not an interruption — nothing
pings anyone, it just lands in that agent's next session. Ids come from
`robofinger since --ids`.

### Style, not rules

None of this is enforced, and a team that settles on something else should
write that in the conventions section at the end of this file. These are
defaults that tend to work:

- **Give the options where you have them.** "I'm blocked" forces someone to
  work out what to do; "I can take the API layer or wait ~20m, which?" can be
  settled in one line. When you genuinely cannot see the options — you do not
  understand why something is built this way — ask that plainly instead.
  Inventing options to satisfy a format wastes everyone's time.
- **Answer a [FOR YOU] question early, not necessarily first.** It is somebody
  waiting on you. Finishing the thing already in flight is usually fine, and
  sometimes means answering with more information than you had.
- **Acknowledge when it changes what someone does.** "Seen it, staying off
  src/auth" tells the holder something. "Thanks!" does not — there is no
  audience, and every entry competes with the work in an agent's context
  window.

## When you hit a CLAIM CONFLICT

Another agent is holding a path you want. Four options, roughly in order:

1. **Work elsewhere.** Cheapest, and usually right.
2. **Wait.** `robofinger check <path>` prints nothing once the path frees up,
   so poll it in the background and get on with unblocked work. Do not sit
   idle, and do not hand-roll a foreground sleep loop — it blocks the session
   for something that may take minutes.
3. **Ask the holder.** They see it at their next session start.
4. **Ask your human.** See below.

## When to ask your human instead

Ask a person, rather than deciding alone, when:

- two agents want the same path and neither has yielded
- a peer's claim is well past its ETA and you cannot tell if the session died
- a peer asks you something whose answer changes work outside your task
- answering would mean undoing a teammate's work

**Say what you would do by default and what the alternatives cost.** Do not
just report the problem.

Everything else is context: read it, carry on, and do not reply to be polite —
there is no audience.

## Team conventions

<!-- Yours. Anything below this line survives `robofinger hooks install`. -->
"#;

/// Everything above this heading is regenerated on upgrade; below it is the
/// user's. Matched on the heading itself rather than a comment marker so a
/// user who tidies the HTML comments away does not lose their section.
pub const USER_SECTION: &str = "## Team conventions";

/// Write ROBOFINGER.md into the repo root, keeping anything the user added.
///
/// Idempotent: the managed part is regenerated, and everything from
/// `USER_SECTION` onward is carried across verbatim. A user who deleted that
/// heading gets the fresh one, which is the same as a first install — better
/// than refusing to upgrade a file they edited.
pub fn write_workflow(dir: &std::path::Path) -> Result<String, String> {
    let path = dir.join("ROBOFINGER.md");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();

    let body = match existing.find(USER_SECTION) {
        // Splice the user's tail onto the regenerated head. The managed text
        // ends with its own copy of the heading, so drop that before joining
        // or the file grows a duplicate on every upgrade.
        Some(i) => {
            let head = ROBOFINGER_MD
                .split_once(USER_SECTION)
                .map(|(h, _)| h)
                .unwrap_or(ROBOFINGER_MD);
            format!("{head}{}", &existing[i..])
        }
        None => ROBOFINGER_MD.to_string(),
    };

    if body == existing {
        return Ok(format!("{} is already up to date", path.display()));
    }
    let verb = if existing.is_empty() {
        "wrote"
    } else {
        "updated"
    };
    std::fs::write(&path, &body).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(format!("{verb} {}", path.display()))
}

/// The repo root, or the working directory when this is not a git checkout.
pub fn repo_dir() -> PathBuf {
    std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Where the hooks get written.
///
/// Account scope (`~/.claude/settings.json`) applies to every project on this
/// machine. Project scope (`<repo>/.claude/settings.json`) applies only inside
/// that repo and can be committed so teammates get it on clone.
#[derive(Clone, Copy, PartialEq)]
pub enum Scope {
    Account,
    Project,
}

pub fn settings_path_for(scope: Scope) -> PathBuf {
    match scope {
        Scope::Account => {
            let home = std::env::var("HOME").unwrap_or_default();
            PathBuf::from(home).join(".claude/settings.json")
        }
        Scope::Project => {
            let root = std::process::Command::new("git")
                .args(["rev-parse", "--show-toplevel"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| ".".into());
            PathBuf::from(root).join(".claude/settings.json")
        }
    }
}

/// The hook block, built against the binary's actual location so the installed
/// hooks work even when ~/.local/bin is absent from the hook process's PATH.
fn hook_json(bin: &str) -> serde_json::Value {
    let cmd = |sub: &str| serde_json::json!({ "hooks": [{ "type": "command", "command": format!("{bin} {sub}") }] });
    serde_json::json!({
        "SessionStart": [cmd("start")],
        "SessionEnd": [cmd("end")],
        "PreToolUse": [{
            "matcher": "Edit|Write|NotebookEdit",
            "hooks": [{ "type": "command", "command": format!("{bin} check") }]
        }]
    })
}

/// Absolute path to the running binary, falling back to the bare name.
fn self_path() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok())
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "robofinger".into())
}

/// True when settings.json already points at robofinger for all three events.
fn already_installed(settings: &serde_json::Value) -> bool {
    let Some(hooks) = settings.get("hooks") else {
        return false;
    };
    ["SessionStart", "SessionEnd", "PreToolUse"]
        .iter()
        .all(|k| {
            hooks
                .get(k)
                .and_then(|v| v.as_array())
                .is_some_and(|entries| {
                    entries.iter().any(|e| {
                        e.get("hooks").and_then(|h| h.as_array()).is_some_and(|hs| {
                            hs.iter().any(|h| {
                                h.get("command")
                                    .and_then(|c| c.as_str())
                                    .is_some_and(|c| c.contains("robofinger"))
                            })
                        })
                    })
                })
        })
}

/// Merge the hooks into settings.json. Backs up any existing file first.
///
/// Only the three robofinger events are touched; every other key — model,
/// plugins, permissions — is preserved exactly.
pub fn install(force: bool, scope: Scope) -> Result<String, String> {
    let path = settings_path_for(scope);
    let parent = path
        .parent()
        .ok_or_else(|| "cannot resolve ~/.claude".to_string())?;
    std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;

    let raw = std::fs::read_to_string(&path).unwrap_or_default();
    let mut settings: serde_json::Value = if raw.trim().is_empty() {
        serde_json::json!({})
    } else {
        // Refuse to touch a file we cannot parse — overwriting someone's
        // hand-edited settings because of a stray comma would be unforgivable.
        serde_json::from_str(&raw).map_err(|e| {
            format!(
                "{} is not valid JSON ({e}) — fix it or edit by hand",
                path.display()
            )
        })?
    };

    if !force && already_installed(&settings) {
        return Ok(format!("hooks already installed in {}", path.display()));
    }

    // Back up before writing. Timestamped so repeated installs don't clobber.
    if !raw.is_empty() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let backup = path.with_extension(format!("json.bak-{stamp}"));
        std::fs::write(&backup, &raw).map_err(|e| format!("backup failed: {e}"))?;
    }

    let bin = self_path();
    let new_hooks = hook_json(&bin);
    let hooks = settings
        .as_object_mut()
        .ok_or_else(|| "settings.json is not an object".to_string())?
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));
    let hooks_obj = hooks
        .as_object_mut()
        .ok_or_else(|| "settings.json \"hooks\" is not an object".to_string())?;

    // Replace only robofinger's own entries; leave other tools' hooks alone.
    for (event, entries) in new_hooks.as_object().unwrap() {
        let list = hooks_obj
            .entry(event.clone())
            .or_insert_with(|| serde_json::json!([]));
        let arr = list
            .as_array_mut()
            .ok_or_else(|| format!("settings.json hooks.{event} is not an array"))?;
        arr.retain(|e| {
            !serde_json::to_string(e)
                .unwrap_or_default()
                .contains("robofinger")
        });
        for entry in entries.as_array().unwrap() {
            arr.push(entry.clone());
        }
    }

    let out = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    std::fs::write(&path, out + "\n").map_err(|e| format!("write {}: {e}", path.display()))?;

    Ok(format!(
        "installed Claude Code hooks in {}\n   using {bin}\n   restart Claude Code for them to load",
        path.display()
    ))
}

/// Remove robofinger's hooks, leaving everything else intact.
pub fn uninstall(scope: Scope) -> Result<String, String> {
    let path = settings_path_for(scope);
    let raw =
        std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut settings: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| format!("{} is not valid JSON: {e}", path.display()))?;

    let Some(hooks) = settings.get_mut("hooks").and_then(|h| h.as_object_mut()) else {
        return Ok("no hooks to remove".into());
    };
    let mut removed = 0;
    for (_event, list) in hooks.iter_mut() {
        if let Some(arr) = list.as_array_mut() {
            let before = arr.len();
            arr.retain(|e| {
                !serde_json::to_string(e)
                    .unwrap_or_default()
                    .contains("robofinger")
            });
            removed += before - arr.len();
        }
    }
    if removed == 0 {
        return Ok("no robofinger hooks found".into());
    }
    // Drop event keys left empty, so settings.json doesn't accumulate cruft.
    hooks.retain(|_, v| v.as_array().is_none_or(|a| !a.is_empty()));
    if hooks.is_empty() {
        settings.as_object_mut().unwrap().remove("hooks");
    }

    let out = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    std::fs::write(&path, out + "\n").map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(format!(
        "removed {removed} robofinger hook(s) from {}",
        path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_installed_hooks() {
        let none = serde_json::json!({});
        assert!(!already_installed(&none));

        let partial = serde_json::json!({
            "hooks": { "SessionStart": [{"hooks":[{"command":"robofinger start"}]}] }
        });
        assert!(
            !already_installed(&partial),
            "one of three is not installed"
        );

        let full = serde_json::json!({
            "hooks": {
                "SessionStart": [{"hooks":[{"command":"robofinger start"}]}],
                "SessionEnd":   [{"hooks":[{"command":"robofinger end"}]}],
                "PreToolUse":   [{"hooks":[{"command":"robofinger check"}]}]
            }
        });
        assert!(already_installed(&full));
    }

    #[test]
    fn other_tools_hooks_are_not_matched() {
        let other = serde_json::json!({
            "hooks": {
                "SessionStart": [{"hooks":[{"command":"some-other-tool init"}]}],
                "SessionEnd":   [{"hooks":[{"command":"some-other-tool done"}]}],
                "PreToolUse":   [{"hooks":[{"command":"some-other-tool check"}]}]
            }
        });
        assert!(!already_installed(&other));
    }

    /// The managed text must not mention the user-section heading in prose.
    /// `write_workflow` splices on the first occurrence, so a stray literal
    /// truncates the file there — losing every section after it, silently, on
    /// the next `hooks install`.
    #[test]
    fn managed_text_does_not_contain_the_split_marker_twice() {
        let n = ROBOFINGER_MD.matches(USER_SECTION).count();
        assert_eq!(
            n, 1,
            "the split marker appears {n} times in the managed text; a second \
             occurrence truncates ROBOFINGER.md at the first one"
        );

        // And the round trip keeps everything the managed text ships with.
        let d = std::env::temp_dir().join(format!("rf-split-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        write_workflow(&d).unwrap();
        write_workflow(&d).unwrap();
        let got = std::fs::read_to_string(d.join("ROBOFINGER.md")).unwrap();
        assert_eq!(got, ROBOFINGER_MD, "a reinstall reproduces the full text");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The upgrade path. Losing a team's own conventions because robofinger
    /// wanted to refresh its own text would be unforgivable, and it is exactly
    /// what a naive overwrite does.
    #[test]
    fn upgrading_the_workflow_keeps_what_the_team_added() {
        let d = std::env::temp_dir().join(format!("rf-wf-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let path = d.join("ROBOFINGER.md");

        write_workflow(&d).unwrap();
        let first = std::fs::read_to_string(&path).unwrap();
        assert!(first.contains(USER_SECTION));

        // A second run changes nothing.
        write_workflow(&d).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), first, "idempotent");

        // The team adds conventions, and robofinger later ships new text.
        std::fs::write(
            &path,
            format!("# stale\n\nold managed text\n\n{USER_SECTION}\n\n- ping alice first\n"),
        )
        .unwrap();
        write_workflow(&d).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();

        assert!(
            after.contains("- ping alice first"),
            "team conventions survived"
        );
        assert!(
            !after.contains("old managed text"),
            "stale managed text replaced"
        );
        assert!(after.starts_with("# robofinger"), "fresh managed head");
        assert_eq!(
            after.matches(USER_SECTION).count(),
            1,
            "the heading must not accumulate on every upgrade"
        );

        // A user who deleted the heading gets a clean file rather than an
        // error — same as a first install.
        std::fs::write(&path, "# whatever\n").unwrap();
        write_workflow(&d).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), first);

        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn hook_json_uses_the_given_binary_path() {
        let h = hook_json("/opt/bin/robofinger");
        let s = serde_json::to_string(&h).unwrap();
        assert!(s.contains("/opt/bin/robofinger start"));
        assert!(s.contains("/opt/bin/robofinger check"));
        assert!(s.contains("Edit|Write|NotebookEdit"));
    }
}
