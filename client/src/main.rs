//! planrelay — agent plan sync over a Cloudflare relay.
//!
//!   planrelay claim "<task>" <glob>...   publish a claim
//!   planrelay release                    drop claims (status stays working)
//!   planrelay done                       mark finished
//!   planrelay peers                      list live peer claims
//!   planrelay check <path>               exit 0 clean, 0 + hook JSON on conflict
//!   planrelay watch                      stream updates over WebSocket
//!
//! Env: PLANRELAY_URL (e.g. https://planrelay.you.workers.dev)
//!      PLANRELAY_NS  (shared namespace, acts as the team secret)
//!      PLANRELAY_AGENT (defaults to hostname)

use serde::{Deserialize, Serialize};
use std::io::Read;

const STALE_MULT: i64 = 2;
const DEFAULT_ETA: i64 = 1800;

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Plan {
    agent: String,
    #[serde(default)]
    seq: u64,
    #[serde(default)]
    epoch: i64,
    #[serde(default)]
    status: String,
    #[serde(default)]
    task: String,
    #[serde(default)]
    touching: Vec<String>,
    #[serde(default)]
    project: String,
    #[serde(default = "default_eta")]
    eta_s: i64,
}

fn default_eta() -> i64 {
    DEFAULT_ETA
}

impl Plan {
    /// A claim is live while the agent is working and its ETA hasn't doubled.
    /// This is the deadman switch: a crashed agent's claims release themselves.
    fn live(&self, now: i64) -> bool {
        self.status != "done" && (now - self.epoch) < self.eta_s * STALE_MULT
    }
}

struct Cfg {
    url: String,
    ns: String,
    agent: String,
}

fn cfg() -> Option<Cfg> {
    let url = std::env::var("PLANRELAY_URL").ok()?;
    let ns = std::env::var("PLANRELAY_NS").ok()?;
    let agent = std::env::var("PLANRELAY_AGENT")
        .ok()
        .or_else(hostname)
        .unwrap_or_else(|| "unknown".into());
    Some(Cfg { url: url.trim_end_matches('/').to_string(), ns, agent })
}

fn hostname() -> Option<String> {
    let out = std::process::Command::new("hostname").arg("-s").output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|s| !s.is_empty())
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn git_toplevel() -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|s| !s.is_empty())
}

/// Repo name, so `src/**` in one project never collides with `src/**` in another.
fn project() -> String {
    git_toplevel()
        .and_then(|p| p.rsplit('/').next().map(String::from))
        .unwrap_or_else(|| "-".into())
}

fn repo_root() -> String {
    git_toplevel().unwrap_or_else(|| {
        std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    })
}

fn fetch_plans(c: &Cfg) -> Vec<Plan> {
    let url = format!("{}/ns/{}/plans", c.url, c.ns);
    ureq::get(&url)
        .call()
        .ok()
        .and_then(|mut r| r.body_mut().read_json::<Vec<serde_json::Value>>().ok())
        .map(|v| {
            v.into_iter()
                .filter_map(|x| serde_json::from_value::<Plan>(x).ok())
                .collect()
        })
        .unwrap_or_default()
}

fn publish(c: &Cfg, status: &str, task: &str, touching: Vec<String>) -> Result<(), String> {
    let prev_seq = fetch_plans(c)
        .iter()
        .find(|p| p.agent == c.agent)
        .map(|p| p.seq)
        .unwrap_or(0);
    let plan = Plan {
        agent: c.agent.clone(),
        seq: prev_seq + 1,
        epoch: now(),
        status: status.into(),
        task: task.into(),
        touching,
        project: project(),
        eta_s: std::env::var("PLANRELAY_ETA")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_ETA),
    };
    let url = format!("{}/ns/{}/plan/{}", c.url, c.ns, c.agent);
    ureq::put(&url)
        .send_json(&plan)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Make `path` relative to the repo root.
///
/// Purely lexical — the target file often does not exist yet (Write creates
/// it), so `fs::canonicalize` is not an option. Instead strip the macOS
/// `/private` prefix on both sides, which is the one case where git and the
/// hook disagree about the same directory.
fn relative_to_root(path: &str) -> String {
    let norm = |p: &str| p.strip_prefix("/private").unwrap_or(p).to_string();
    let abs = norm(path);
    let root = norm(&repo_root());
    abs.strip_prefix(&format!("{root}/"))
        .unwrap_or(&abs)
        .to_string()
}

/// Peer claims matching `path`, scoped to the current project.
fn conflicts(c: &Cfg, path: &str) -> Vec<(Plan, String)> {
    let rel = &relative_to_root(path);
    let here = project();
    let t = now();
    let plans = fetch_plans(c);
    let mut hits = Vec::new();
    for p in plans {
        if p.agent == c.agent || p.project != here || !p.live(t) {
            continue;
        }
        for g in &p.touching {
            let m = glob::Pattern::new(g)
                .map(|pat| pat.matches(rel) || pat.matches(path))
                .unwrap_or(false);
            if m {
                hits.push((p.clone(), g.clone()));
                break;
            }
        }
    }
    hits
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("");

    // Unconfigured: stay silent and succeed. A hook must never break a session.
    let Some(c) = cfg() else {
        if matches!(cmd, "check" | "start" | "end") {
            std::process::exit(0);
        }
        eprintln!("set PLANRELAY_URL and PLANRELAY_NS");
        std::process::exit(1);
    };

    match cmd {
        "claim" => {
            let task = args.get(1).cloned().unwrap_or_default();
            let globs: Vec<String> = if args.len() > 2 { args[2..].to_vec() } else { vec![] };
            match publish(&c, "working", &task, globs.clone()) {
                Ok(_) => println!("claimed {globs:?} ({task})"),
                Err(e) => {
                    eprintln!("publish failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        "release" => {
            let task = fetch_plans(&c)
                .iter()
                .find(|p| p.agent == c.agent)
                .map(|p| p.task.clone())
                .unwrap_or_default();
            let _ = publish(&c, "working", &task, vec![]);
            println!("released");
        }
        "done" => {
            let _ = publish(&c, "done", "", vec![]);
            println!("done");
        }
        "peers" => {
            let t = now();
            let mut any = false;
            for p in fetch_plans(&c) {
                if p.agent == c.agent || !p.live(t) {
                    continue;
                }
                for g in &p.touching {
                    println!("{:<14} {:<30} {}/{}", p.agent, p.task, p.project, g);
                    any = true;
                }
            }
            if !any {
                println!("no live peer claims");
            }
        }
        // PreToolUse hook: hook JSON on stdin, advisory warning on stdout.
        "check" => {
            let mut buf = String::new();
            let _ = std::io::stdin().read_to_string(&mut buf);
            let path = args.get(1).cloned().or_else(|| {
                serde_json::from_str::<serde_json::Value>(&buf)
                    .ok()?
                    .pointer("/tool_input/file_path")?
                    .as_str()
                    .map(String::from)
            });
            let Some(path) = path else { std::process::exit(0) };

            let hits = conflicts(&c, &path);
            if !hits.is_empty() {
                let detail: Vec<String> = hits
                    .iter()
                    .map(|(p, g)| format!("{} ({}) claims {}", p.agent, p.task, g))
                    .collect();
                let msg = format!(
                    "CLAIM CONFLICT on {}:\n{}\nThis is advisory. Consider working elsewhere, or coordinate first.",
                    path,
                    detail.join("\n")
                );
                println!(
                    "{}",
                    serde_json::json!({
                        "hookSpecificOutput": {
                            "hookEventName": "PreToolUse",
                            "additionalContext": msg
                        }
                    })
                );
            }
            std::process::exit(0);
        }
        // SessionStart hook: surface live peer claims into the agent's context.
        "start" => {
            let t = now();
            let lines: Vec<String> = fetch_plans(&c)
                .into_iter()
                .filter(|p| p.agent != c.agent && p.live(t))
                .flat_map(|p| {
                    p.touching
                        .iter()
                        .map(|g| format!("  {} claims {}/{} ({})", p.agent, p.project, g, p.task))
                        .collect::<Vec<_>>()
                })
                .collect();
            if !lines.is_empty() {
                println!(
                    "{}",
                    serde_json::json!({
                        "hookSpecificOutput": {
                            "hookEventName": "SessionStart",
                            "additionalContext": format!(
                                "Peer agent claims currently active:\n{}\nBefore editing a claimed path, consider whether to coordinate.",
                                lines.join("\n"))
                        }
                    })
                );
            }
            std::process::exit(0);
        }
        "end" => {
            let _ = publish(&c, "done", "", vec![]);
            std::process::exit(0);
        }
        "watch" => watch(&c),
        _ => {
            eprintln!("usage: planrelay claim|release|done|peers|check|watch");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(agent: &str, project: &str, touching: &[&str], status: &str, age: i64) -> Plan {
        Plan {
            agent: agent.into(),
            seq: 1,
            epoch: now() - age,
            status: status.into(),
            task: "t".into(),
            touching: touching.iter().map(|s| s.to_string()).collect(),
            project: project.into(),
            eta_s: 1800,
        }
    }

    /// Same shape as `conflicts`, minus the network.
    fn matches(p: &Plan, rel: &str, here: &str, me: &str) -> bool {
        if p.agent == me || p.project != here || !p.live(now()) {
            return false;
        }
        p.touching.iter().any(|g| {
            glob::Pattern::new(g).map(|pat| pat.matches(rel)).unwrap_or(false)
        })
    }

    #[test]
    fn glob_and_exact_paths_match() {
        let p = plan("peer", "demo", &["src/auth/**", "src/middleware.ts"], "working", 0);
        assert!(matches(&p, "src/auth/session.ts", "demo", "me"), "glob");
        assert!(matches(&p, "src/auth/deep/x.ts", "demo", "me"), "nested glob");
        assert!(matches(&p, "src/middleware.ts", "demo", "me"), "exact");
        assert!(!matches(&p, "README.md", "demo", "me"), "unclaimed is clean");
    }

    #[test]
    fn other_project_never_conflicts() {
        let p = plan("peer", "demo", &["src/auth/**"], "working", 0);
        assert!(!matches(&p, "src/auth/session.ts", "otherproj", "me"));
    }

    #[test]
    fn own_claim_is_not_a_conflict() {
        let p = plan("me", "demo", &["src/auth/**"], "working", 0);
        assert!(!matches(&p, "src/auth/session.ts", "demo", "me"));
    }

    #[test]
    fn stale_and_done_claims_expire() {
        let stale = plan("peer", "demo", &["src/auth/**"], "working", 7200);
        assert!(!matches(&stale, "src/auth/session.ts", "demo", "me"), "stale");
        let done = plan("peer", "demo", &["src/auth/**"], "done", 0);
        assert!(!matches(&done, "src/auth/session.ts", "demo", "me"), "done");
    }

    /// Regression: the file usually does NOT exist yet (Write creates it), and
    /// on macOS git says /private/tmp while the hook says /tmp. Both broke this.
    #[test]
    fn relative_paths_survive_private_prefix_and_missing_files() {
        let norm = |p: &str| p.strip_prefix("/private").unwrap_or(p).to_string();
        let strip = |abs: &str, root: &str| -> String {
            let (a, r) = (norm(abs), norm(root));
            a.strip_prefix(&format!("{r}/")).unwrap_or(&a).to_string()
        };
        assert_eq!(strip("/tmp/demo/src/a.ts", "/private/tmp/demo"), "src/a.ts");
        assert_eq!(strip("/private/tmp/demo/src/a.ts", "/tmp/demo"), "src/a.ts");
        assert_eq!(strip("/home/x/repo/src/a.ts", "/home/x/repo"), "src/a.ts");
    }

    #[test]
    fn plans_with_missing_fields_still_parse() {
        let p: Plan = serde_json::from_str(r#"{"agent":"a"}"#).unwrap();
        assert_eq!(p.eta_s, DEFAULT_ETA);
        assert!(p.touching.is_empty());
    }
}

/// Long-lived WebSocket subscription. For humans and /loop, not for hooks —
/// a per-invocation hook can't hold a socket open.
fn watch(c: &Cfg) {
    let ws_url = c
        .url
        .replacen("https://", "wss://", 1)
        .replacen("http://", "ws://", 1);
    let url = format!("{}/ns/{}/subscribe", ws_url, c.ns);
    let (mut sock, _) = match tungstenite::connect(&url) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("connect failed: {e}");
            std::process::exit(1);
        }
    };
    eprintln!("watching {} as {}", c.ns, c.agent);
    loop {
        match sock.read() {
            Ok(tungstenite::Message::Text(t)) => {
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) else {
                    continue;
                };
                match v["type"].as_str() {
                    Some("snapshot") => {
                        let n = v["plans"].as_array().map(|a| a.len()).unwrap_or(0);
                        eprintln!("snapshot: {n} plans");
                    }
                    Some("plan") => {
                        let p = &v["plan"];
                        println!(
                            "{} [{}] {} -> {}",
                            p["agent"].as_str().unwrap_or("?"),
                            p["status"].as_str().unwrap_or("?"),
                            p["task"].as_str().unwrap_or(""),
                            p["touching"]
                                .as_array()
                                .map(|a| a
                                    .iter()
                                    .filter_map(|x| x.as_str())
                                    .collect::<Vec<_>>()
                                    .join(","))
                                .unwrap_or_default()
                        );
                    }
                    _ => {}
                }
            }
            Ok(tungstenite::Message::Close(_)) | Err(_) => {
                eprintln!("disconnected");
                std::process::exit(1);
            }
            _ => {}
        }
    }
}
