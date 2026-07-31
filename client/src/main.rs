//! robofinger — agent plan sync over a Cloudflare relay.
//!
//!   robofinger claim "<task>" <glob>...   publish a claim
//!   robofinger release                    drop claims (status stays working)
//!   robofinger done                       mark finished
//!   robofinger peers                      list live peer claims
//!   robofinger check <path>               exit 0 clean, 0 + hook JSON on conflict
//!   robofinger watch                      stream updates over WebSocket
//!   robofinger id [label]                 print your shareable identity blob
//!   robofinger peer add|rm|list           manage trusted peers
//!
//! Plans are signed (Ed25519) and encrypted (age) client-side. The relay
//! stores opaque ciphertext and can verify signatures but never read contents.
//!
//! Env: ROBOFINGER_URL (e.g. https://robofinger.you.workers.dev)
//!      ROBOFINGER_NS  (routing key, not a secret)
//!      ROBOFINGER_AGENT (display label, defaults to hostname)
//!      ROBOFINGER_HOME (key dir, defaults to ~/.config/robofinger)

mod crypto;

use crypto::{Keys, Peer};
use serde::{Deserialize, Serialize};
use std::io::Read;

const STALE_MULT: i64 = 2;
const DEFAULT_ETA: i64 = 1800;

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Plan {
    agent: String,
    /// Publisher's Ed25519 public key — the real identity. `agent` is a label.
    #[serde(default)]
    pubkey: String,
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
    let url = std::env::var("ROBOFINGER_URL").ok()?;
    let ns = std::env::var("ROBOFINGER_NS").ok()?;
    let agent = std::env::var("ROBOFINGER_AGENT")
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

/// Cleartext envelope. The relay reads only these fields — enough to enforce
/// single-writer and monotonic ordering. `body` is age ciphertext it cannot open.
#[derive(Serialize, Deserialize, Debug, Clone)]
struct Envelope {
    pubkey: String,
    seq: u64,
    sig: String,
    body: String,
}

impl Envelope {
    fn signed_message(&self) -> String {
        format!("{}|{}|{}", self.pubkey, self.seq, self.body)
    }
}

/// Fetch envelopes, verify signatures, decrypt what we can read.
///
/// Anything that fails verification is dropped silently — a forged or corrupt
/// envelope must never reach the conflict check. Plans encrypted to someone
/// else simply fail to decrypt and are skipped.
fn fetch_plans(c: &Cfg, k: &Keys) -> Vec<Plan> {
    let subs = crypto::load_peers();
    // Ask only for keys we trust, plus our own.
    let mut want: Vec<String> = subs.iter().map(|p| p.pubkey.clone()).collect();
    want.push(k.pubkey());
    let url = format!("{}/ns/{}/plans?from={}", c.url, c.ns, want.join(","));

    let envs: Vec<Envelope> = ureq::get(&url)
        .call()
        .ok()
        .and_then(|mut r| r.body_mut().read_json::<Vec<serde_json::Value>>().ok())
        .map(|v| {
            v.into_iter()
                .filter_map(|x| serde_json::from_value::<Envelope>(x).ok())
                .collect()
        })
        .unwrap_or_default();

    envs.into_iter()
        .filter(|e| {
            // Trust only keys we subscribed to (or ourselves), and only if the
            // signature actually checks out.
            (e.pubkey == k.pubkey() || subs.iter().any(|p| p.pubkey == e.pubkey))
                && crypto::verify(&e.pubkey, &e.sig, &e.signed_message())
        })
        .filter_map(|e| {
            let plain = crypto::decrypt(&e.body, &k.age_secret).ok()?;
            let mut plan: Plan = serde_json::from_slice(&plain).ok()?;
            // seq comes from the signed envelope, not the encrypted body.
            plan.seq = e.seq;
            plan.pubkey = e.pubkey;
            Some(plan)
        })
        .collect()
}

fn publish(c: &Cfg, k: &Keys, status: &str, task: &str, touching: Vec<String>) -> Result<(), String> {
    let prev_seq = fetch_plans(c, k)
        .iter()
        .find(|p| p.pubkey == k.pubkey())
        .map(|p| p.seq)
        .unwrap_or(0);
    let plan = Plan {
        agent: c.agent.clone(),
        pubkey: k.pubkey(),
        seq: prev_seq + 1,
        epoch: now(),
        status: status.into(),
        task: task.into(),
        touching,
        project: project(),
        eta_s: std::env::var("ROBOFINGER_ETA")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_ETA),
    };

    // Encrypt to every peer plus self — omitting self means you cannot read
    // your own plan back, which breaks the seq lookup above.
    let mut recips = vec![k.age_secret.to_public()];
    for p in crypto::load_peers() {
        match p.age_pub.parse::<age::x25519::Recipient>() {
            Ok(r) => recips.push(r),
            Err(_) => eprintln!("warning: peer {} has an unusable age key", p.label),
        }
    }
    let body = crypto::encrypt(&serde_json::to_vec(&plan).map_err(|e| e.to_string())?, &recips)?;

    let mut env = Envelope { pubkey: k.pubkey(), seq: plan.seq, sig: String::new(), body };
    env.sig = k.sign(&env.signed_message());

    let url = format!("{}/ns/{}/plan/{}", c.url, c.ns, k.pubkey());
    ureq::put(&url)
        .send_json(&env)
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
fn conflicts(c: &Cfg, k: &Keys, path: &str) -> Vec<(Plan, String)> {
    let rel = &relative_to_root(path);
    let here = project();
    let t = now();
    let plans = fetch_plans(c, k);
    let mut hits = Vec::new();
    for p in plans {
        if p.pubkey == k.pubkey() || p.project != here || !p.live(t) {
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

    // Keys are generated on first use. A hook must never break a session, so
    // failure here is fatal only for interactive commands.
    let k = match Keys::load_or_create() {
        Ok(k) => k,
        Err(e) => {
            if matches!(cmd, "check" | "start" | "end") {
                std::process::exit(0);
            }
            eprintln!("key error: {e}");
            std::process::exit(1);
        }
    };

    // Identity and peer management work without a relay configured.
    match cmd {
        "id" => {
            let label = args
                .get(1)
                .cloned()
                .or_else(hostname)
                .unwrap_or_else(|| "agent".into());
            println!("{}", k.identity_blob(&label));
            eprintln!("\nshare that line with a peer; they run: robofinger peer add <blob>");
            return;
        }
        "peer" => {
            let sub = args.get(1).map(String::as_str).unwrap_or("");
            let mut peers = crypto::load_peers();
            match sub {
                "add" => {
                    let Some(blob) = args.get(2) else {
                        eprintln!("usage: robofinger peer add rf1....");
                        std::process::exit(1);
                    };
                    match Peer::parse(blob) {
                        Ok(p) => {
                            if p.pubkey == k.pubkey() {
                                eprintln!("that's your own identity");
                                std::process::exit(1);
                            }
                            peers.retain(|x| x.pubkey != p.pubkey);
                            peers.push(p.clone());
                            match crypto::save_peers(&peers) {
                                Ok(_) => println!("added peer {} ({}...)", p.label, &p.pubkey[..8]),
                                Err(e) => {
                                    eprintln!("{e}");
                                    std::process::exit(1);
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("bad identity blob: {e}");
                            std::process::exit(1);
                        }
                    }
                }
                "rm" => {
                    let Some(which) = args.get(2) else {
                        eprintln!("usage: robofinger peer rm <label|pubkey>");
                        std::process::exit(1);
                    };
                    let before = peers.len();
                    peers.retain(|p| &p.label != which && !p.pubkey.starts_with(which.as_str()));
                    if peers.len() == before {
                        eprintln!("no peer matched {which}");
                        std::process::exit(1);
                    }
                    let _ = crypto::save_peers(&peers);
                    println!("removed {which}; future plans will not be readable by them");
                }
                "list" | "" => {
                    if peers.is_empty() {
                        println!("no peers yet — share `robofinger id` and add theirs");
                    }
                    for p in &peers {
                        println!("{:<14} {}...", p.label, &p.pubkey[..12]);
                    }
                }
                _ => {
                    eprintln!("usage: robofinger peer add|rm|list");
                    std::process::exit(1);
                }
            }
            return;
        }
        _ => {}
    }

    // Unconfigured: stay silent and succeed. A hook must never break a session.
    let Some(c) = cfg() else {
        if matches!(cmd, "check" | "start" | "end") {
            std::process::exit(0);
        }
        eprintln!("set ROBOFINGER_URL and ROBOFINGER_NS");
        std::process::exit(1);
    };

    match cmd {
        "claim" => {
            let task = args.get(1).cloned().unwrap_or_default();
            let globs: Vec<String> = if args.len() > 2 { args[2..].to_vec() } else { vec![] };
            match publish(&c, &k, "working", &task, globs.clone()) {
                Ok(_) => println!("claimed {globs:?} ({task})"),
                Err(e) => {
                    eprintln!("publish failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        "release" => {
            let task = fetch_plans(&c, &k)
                .iter()
                .find(|p| p.pubkey == k.pubkey())
                .map(|p| p.task.clone())
                .unwrap_or_default();
            let _ = publish(&c, &k, "working", &task, vec![]);
            println!("released");
        }
        "done" => {
            let _ = publish(&c, &k, "done", "", vec![]);
            println!("done");
        }
        "peers" => {
            let t = now();
            let mut any = false;
            for p in fetch_plans(&c, &k) {
                if p.pubkey == k.pubkey() || !p.live(t) {
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

            let hits = conflicts(&c, &k, &path);
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
            let lines: Vec<String> = fetch_plans(&c, &k)
                .into_iter()
                .filter(|p| p.pubkey != k.pubkey() && p.live(t))
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
            let _ = publish(&c, &k, "done", "", vec![]);
            std::process::exit(0);
        }
        "watch" => watch(&c, &k),
        _ => {
            eprintln!("usage: robofinger id|peer|claim|release|done|peers|check|watch");
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
            pubkey: format!("pk-{agent}"),
            seq: 1,
            epoch: now() - age,
            status: status.into(),
            task: "t".into(),
            touching: touching.iter().map(|s| s.to_string()).collect(),
            project: project.into(),
            eta_s: 1800,
        }
    }

    /// Same shape as `conflicts`, minus the network. Identity is the pubkey,
    /// matching the real code — `agent` is only a display label.
    fn matches(p: &Plan, rel: &str, here: &str, me: &str) -> bool {
        if p.pubkey == format!("pk-{me}") || p.project != here || !p.live(now()) {
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
fn watch(c: &Cfg, k: &Keys) {
    let ws_url = c
        .url
        .replacen("https://", "wss://", 1)
        .replacen("http://", "ws://", 1);
    let subs = crypto::load_peers();
    let mut want: Vec<String> = subs.iter().map(|p| p.pubkey.clone()).collect();
    want.push(k.pubkey());
    let url = format!("{}/ns/{}/subscribe?from={}", ws_url, c.ns, want.join(","));
    let (mut sock, _) = match tungstenite::connect(&url) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("connect failed: {e}");
            std::process::exit(1);
        }
    };
    eprintln!("watching {} as {} ({} peer(s))", c.ns, c.agent, subs.len());

    // Verify signature, then decrypt. Anything that fails either step is not
    // shown — an unverified plan is worse than no plan.
    let show = |v: &serde_json::Value| {
        let Ok(e) = serde_json::from_value::<Envelope>(v.clone()) else { return };
        if e.pubkey != k.pubkey() && !subs.iter().any(|p| p.pubkey == e.pubkey) {
            return; // not subscribed
        }
        if !crypto::verify(&e.pubkey, &e.sig, &e.signed_message()) {
            eprintln!("dropped envelope with bad signature from {}...", &e.pubkey[..8]);
            return;
        }
        let Ok(plain) = crypto::decrypt(&e.body, &k.age_secret) else { return };
        let Ok(p) = serde_json::from_slice::<Plan>(&plain) else { return };
        println!(
            "{} [{}] {} -> {}",
            p.agent,
            p.status,
            p.task,
            p.touching.join(",")
        );
    };

    loop {
        match sock.read() {
            Ok(tungstenite::Message::Text(t)) => {
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) else {
                    continue;
                };
                match v["type"].as_str() {
                    Some("snapshot") => {
                        let plans = v["plans"].as_array().cloned().unwrap_or_default();
                        eprintln!("snapshot: {} plan(s)", plans.len());
                        for p in &plans {
                            show(p);
                        }
                    }
                    Some("plan") => show(&v["plan"]),
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
