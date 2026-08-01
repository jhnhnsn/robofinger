//! Self-update: `robofinger --upgrade` and its `--check`.
//!
//! Deliberately dependency-free — the version check follows the
//! `/releases/latest` redirect via `curl` (no HTTP crate, no JSON parsing) and
//! the install re-runs the published installer. Same approach as envstow: this
//! is convenience, not function, and shouldn't grow the dependency surface of a
//! binary that sits beside your private keys.

use std::io::{self, IsTerminal, Write};
use std::process::Command;

const REPO_URL: &str = "https://github.com/jhnhnsn/robofinger";

/// The published shell installer — the command the README tells you to run.
/// `upgrade` re-runs it so you don't have to remember it. That IS the feature.
///
/// POSIX-only: Windows installs via the PowerShell installer and takes a
/// different branch in `cmd_upgrade`.
#[cfg(not(windows))]
const INSTALLER_URL: &str =
    "https://github.com/jhnhnsn/robofinger/releases/latest/download/robofinger-installer.sh";

/// `/releases/latest` 302s to `/releases/tag/vX.Y.Z`, so the redirect target
/// names the newest version. No JSON to parse, no API token, and not subject to
/// the unauthenticated API rate limit.
const LATEST_URL: &str = "https://github.com/jhnhnsn/robofinger/releases/latest";

/// Latest released version, read off the `/releases/latest` redirect.
pub fn latest_version() -> Result<String, String> {
    let out = Command::new("curl")
        .args([
            "-sSL",
            "--proto",
            "=https",
            "--tlsv1.2",
            "--max-time",
            "15",
            "-o",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
            "-w",
            "%{url_effective}",
            LATEST_URL,
        ])
        .output()
        .map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => {
                "curl not found — install it, or update manually from:\n  ".to_string() + REPO_URL
            }
            _ => format!("could not run curl: {e}"),
        })?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if err.is_empty() {
            "could not reach GitHub to check for updates".to_string()
        } else {
            format!("could not check for updates: {err}")
        });
    }
    let url = String::from_utf8_lossy(&out.stdout);
    // No redirect means no published release yet (or the repo is private and
    // curl got the login page). Either way there is nothing to upgrade to.
    let tag = url
        .rsplit('/')
        .next()
        .filter(|t| !t.is_empty() && *t != "latest")
        .ok_or_else(|| {
            format!("no published releases found at {REPO_URL}/releases — nothing to upgrade to")
        })?;
    Ok(tag.trim_start_matches('v').to_string())
}

/// Compare dotted numeric versions. String compare gets 0.1.9 vs 0.1.11
/// backwards, which would tell everyone to "upgrade" downward forever.
/// Unparseable input never claims an update — better silent than wrong.
pub fn version_is_newer(candidate: &str, current: &str) -> bool {
    let parse = |v: &str| -> Vec<u64> {
        v.split(['.', '-', '+'])
            .map_while(|p| p.parse::<u64>().ok())
            .collect()
    };
    let (c, u) = (parse(candidate), parse(current));
    if c.is_empty() || u.is_empty() {
        return false;
    }
    for i in 0..c.len().max(u.len()) {
        let (a, b) = (
            c.get(i).copied().unwrap_or(0),
            u.get(i).copied().unwrap_or(0),
        );
        if a != b {
            return a > b;
        }
    }
    false
}

/// How this copy was installed, from the cargo-dist receipt. `None` means no
/// receipt — a package manager, `cargo install`, or a hand-placed binary.
fn install_receipt() -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let path = std::path::Path::new(&home).join(".config/robofinger/robofinger-receipt.json");
    let text = std::fs::read_to_string(path).ok()?;
    // Not parsing JSON — that would mean a serde dep for one field. We only
    // need to know whether OUR installer wrote this.
    if text.contains("\"source\": \"cargo-dist\"") || text.contains("\"source\":\"cargo-dist\"") {
        Some("cargo-dist".to_string())
    } else {
        Some("unknown".to_string())
    }
}

/// `robofinger --upgrade [--check] [--yes]`.
///
/// Refuses to self-update an install we didn't perform: overwriting a
/// Homebrew-managed binary desynchronizes it from the package database, or
/// drops a second copy on PATH that shadows the managed one.
pub fn cmd_upgrade(args: &[String]) -> Result<(), String> {
    let mut check_only = false;
    let mut yes = false;
    for a in args {
        match a.as_str() {
            "--check" => check_only = true,
            "--yes" | "-y" => yes = true,
            s => {
                return Err(format!(
                    "unknown argument '{s}'\nusage: robofinger --upgrade [--check] [--yes]"
                ));
            }
        }
    }

    let current = env!("CARGO_PKG_VERSION");
    let latest = latest_version()?;

    if !version_is_newer(&latest, current) {
        eprintln!("robofinger {current} is up to date (latest: {latest}).");
        return Ok(());
    }
    eprintln!("robofinger {latest} is available (you have {current}).");
    eprintln!("   {REPO_URL}/releases/tag/v{latest}");

    if check_only {
        return Ok(());
    }

    if install_receipt().as_deref() != Some("cargo-dist") {
        return Err(format!(
            "this copy wasn't installed by the robofinger installer, so `--upgrade` won't touch it.\n\
             \x20  Update it with whatever installed it — e.g. `brew upgrade`, your distro's\n\
             \x20  package manager, or `cargo install --path client` from a fresh checkout of\n\
             \x20  {REPO_URL}."
        ));
    }

    // Confirm before replacing the binary. A non-interactive run does NOT
    // proceed by default: this downloads and executes a remote script over the
    // running executable, and a CI job silently swapping itself out would be a
    // nasty surprise.
    if !yes {
        if !io::stdin().is_terminal() {
            return Err(
                "refusing to upgrade non-interactively — pass `--yes` to confirm:\n\
                 \x20  robofinger --upgrade --yes"
                    .to_string(),
            );
        }
        eprint!("Download and install robofinger {latest}? [Y/n] ");
        let _ = io::stderr().flush();
        let mut input = String::new();
        if io::stdin().read_line(&mut input).is_ok() {
            let ans = input.trim().to_ascii_lowercase();
            if ans == "n" || ans == "no" {
                eprintln!("   skipped.");
                return Ok(());
            }
        }
    }

    #[cfg(windows)]
    {
        eprintln!(
            "\nrobofinger: run the PowerShell installer to upgrade:\n\
             \x20  powershell -c \"irm {REPO_URL}/releases/latest/download/robofinger-installer.ps1 | iex\""
        );
        Ok(())
    }

    #[cfg(not(windows))]
    {
        eprintln!("   running the official installer…");
        let status = Command::new("sh")
            .arg("-c")
            .arg(format!(
                "curl --proto '=https' --tlsv1.2 -LsSf {INSTALLER_URL} | sh"
            ))
            .status()
            .map_err(|e| format!("could not run the installer: {e}"))?;
        if status.success() {
            eprintln!("updated to robofinger {latest}. Open a new shell (or `hash -r`) to use it.");
            Ok(())
        } else {
            Err(format!(
                "the installer exited with {}. Try it by hand:\n\
                 \x20  curl --proto '=https' --tlsv1.2 -LsSf {INSTALLER_URL} | sh",
                status.code().unwrap_or(-1)
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compare_is_numeric_not_lexical() {
        // The bug this exists to prevent: "0.1.9" > "0.1.11" as strings, which
        // would tell everyone on 0.1.11 to "upgrade" to 0.1.9, forever.
        assert!(version_is_newer("0.1.11", "0.1.9"));
        assert!(!version_is_newer("0.1.9", "0.1.11"));
        assert!(version_is_newer("0.2.0", "0.1.11"));
        assert!(version_is_newer("1.0.0", "0.99.99"));
        assert!(!version_is_newer("0.1.11", "0.1.11"), "equal is not newer");

        // Differing component counts: missing parts are zero.
        assert!(version_is_newer("0.2", "0.1.11"));
        assert!(!version_is_newer("0.1", "0.1.0"), "0.1 == 0.1.0");

        // Pre-release suffixes: compare the numeric lead, don't panic.
        assert!(version_is_newer("0.2.0-beta.1", "0.1.11"));

        // Unparseable input must never claim an update.
        assert!(!version_is_newer("garbage", "0.1.11"));
        assert!(!version_is_newer("", "0.1.11"));
    }
}
