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

use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;

/// Printed after install — without this the agent never publishes claims, so
/// `touching` stays empty and every conflict check passes trivially.
pub const CLAUDE_MD: &str = r#"## Agent Plan Sync
When starting a task that will edit files, claim the paths first:
`robofinger claim "<short task description>" '<glob>' '<glob>'`

Release when done: `robofinger release`
See live peer claims: `robofinger peers`

Globs are relative to the repo you're working in. Claims are advisory — a
CLAIM CONFLICT warning means another agent is working there; decide whether to
back off or proceed, don't silently ignore it. Claims auto-expire, so a
forgotten release is not fatal."#;

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

fn settings_path() -> PathBuf {
    settings_path_for(Scope::Account)
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

/// Offer to install the hooks. `[Y/n]` on a TTY, default yes; a non-TTY does
/// nothing rather than silently editing a config nobody asked it to touch —
/// the opposite of envstow's convention, because this writes to a shared file
/// (~/.claude/settings.json) rather than creating a project-local one.
pub fn maybe_install() {
    let path = settings_path();

    let existing: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));

    if already_installed(&existing) {
        eprintln!("Claude Code hooks already installed in {}", path.display());
        return;
    }

    if !io::stdin().is_terminal() {
        eprintln!(
            "\nTo wire this into Claude Code, run: robofinger hooks install\n  (adds SessionStart/SessionEnd/PreToolUse to {})",
            path.display()
        );
        return;
    }

    let project = settings_path_for(Scope::Project);
    eprintln!("\nInstall Claude Code hooks?");
    eprintln!(
        "  [a] account — {} (every project on this machine)",
        path.display()
    );
    eprintln!(
        "  [p] project — {} (this repo only, commit to share)",
        project.display()
    );
    eprintln!("  [n] skip");
    eprint!("Choice [a/p/n]: ");
    let _ = io::stderr().flush();
    let mut input = String::new();
    let scope = if io::stdin().read_line(&mut input).is_ok() {
        match input.trim().to_ascii_lowercase().as_str() {
            "p" | "project" => Scope::Project,
            "n" | "no" | "skip" => {
                eprintln!("   skipped. Install later with: robofinger hooks install [--project]");
                return;
            }
            _ => Scope::Account,
        }
    } else {
        Scope::Account
    };

    match install(false, scope) {
        Ok(msg) => eprintln!("{msg}"),
        Err(e) => eprintln!("robofinger: {e}"),
    }
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

    #[test]
    fn hook_json_uses_the_given_binary_path() {
        let h = hook_json("/opt/bin/robofinger");
        let s = serde_json::to_string(&h).unwrap();
        assert!(s.contains("/opt/bin/robofinger start"));
        assert!(s.contains("/opt/bin/robofinger check"));
        assert!(s.contains("Edit|Write|NotebookEdit"));
    }
}
