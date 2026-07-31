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
mod hooks;
mod selfupdate;

use crypto::{Keys, Peer};
use serde::{Deserialize, Serialize};
use std::io::Read;

const STALE_MULT: i64 = 2;
const DEFAULT_ETA: i64 = 1800;
/// Max peer keys in a `?from=` query before the URL gets too long for the edge.
const MAX_FROM: usize = 100;

const USAGE: &str = "\
robofinger — tell other agents what you're working on, before you collide.

  robofinger                          your own status
  robofinger <peer>                   look someone up

setup
  init --url <url> --ns <namespace>   write config + print your identity
  id [label]                          print your shareable identity blob
  hooks install [--project]           wire into Claude Code
  hooks uninstall [--project]         remove the hooks
  peer add <rf1...>                   trust a peer (subscribe + let them decrypt)
  peer rm <label>                     revoke; your next plan is opaque to them
  peer list [-v]                      show trusted peers, relay and last-seen
  peer update <label>                 accept a peer's published move
  moved <new address>                 tell peers you have moved relay

use
  claim \"<task>\" <glob>...            announce what you're touching
  release                             drop claims, keep working
  done                                mark finished
  peers                               live peer claims
  check <path>                        conflict check (hook JSON on stdin)

write
  post \"<text>\"                       append to your log (also reads stdin)
  log [-n N] [--peer <label>]         recent posts from you and your peers
  watch                               stream updates over WebSocket

maintenance
  upgrade [--check] [--yes]           update to the latest release
  --version                           print version

config is read from ~/.config/robofinger/config; environment variables
(ROBOFINGER_URL, ROBOFINGER_NS, ROBOFINGER_AGENT) override it.";

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
    /// Relay base URL. Its path IS the namespace, so
    /// `https://example.com/plan` and `https://example.com/plan/team-a` are
    /// separate rooms with separate storage.
    url: String,
    agent: String,
}

/// `key=value` lines from ~/.config/robofinger/config. Written by `init`.
fn config_file() -> std::collections::HashMap<String, String> {
    std::fs::read_to_string(crypto::config_dir().join("config"))
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

/// Env wins over the config file, so hooks and CI can override without editing it.
fn cfg() -> Option<Cfg> {
    let file = config_file();
    let get = |k: &str| {
        std::env::var(k)
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| file.get(k).cloned())
    };
    let mut url = get("ROBOFINGER_URL")?.trim_end_matches('/').to_string();
    // Back-compat: an older config carried the namespace separately. Fold it
    // into the URL path, which is where it lives now.
    if let Some(ns) = get("ROBOFINGER_NS").filter(|s| !s.is_empty()) {
        url = format!("{url}/{ns}");
    }
    let agent = get("ROBOFINGER_AGENT")
        .or_else(hostname)
        .unwrap_or_else(|| "unknown".into());
    Some(Cfg { url, agent })
}

fn hostname() -> Option<String> {
    let out = std::process::Command::new("hostname")
        .arg("-s")
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|s| !s.is_empty())
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// "4m", "3h", "2d" — compact relative age for list output.
fn ago(secs: i64) -> String {
    match secs {
        s if s < 0 => "just now".into(),
        s if s < 60 => format!("{s}s ago"),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86_400 => format!("{}h ago", s / 3600),
        s => format!("{}d ago", s / 86_400),
    }
}

/// Civil date-time from a unix timestamp, UTC.
///
/// Hand-rolled rather than pulling in `chrono` — this is one line of output in
/// a log command, not worth a dependency in a binary that sits beside private
/// keys. Days-since-epoch to y/m/d via the standard civil-from-days algorithm.
fn stamp(epoch: i64) -> String {
    let (days, secs) = (epoch.div_euclid(86_400), epoch.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        secs / 3600,
        (secs % 3600) / 60
    )
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
/// Group the keys we care about by which relay+namespace hosts them.
///
/// Peers added from an `rf2` blob may live on a different relay entirely, so a
/// single fetch is no longer enough. Most setups have exactly one group, which
/// keeps the common case to one request.
fn endpoints(c: &Cfg, k: &Keys, subs: &[Peer]) -> Vec<(String, Vec<String>)> {
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    let mut add = |url: &str, key: String| match groups.iter_mut().find(|(u, _)| u == url) {
        Some((_, keys)) => keys.push(key),
        None => groups.push((url.to_string(), vec![key])),
    };
    // Our own key always lives on our own relay.
    add(&c.url, k.pubkey());
    for p in subs {
        add(p.endpoint(&c.url), p.pubkey.clone());
    }
    groups
}

/// Fetch signed envelopes from `path` ("plans" or "posts") across every relay
/// that hosts a key we trust, then verify and decrypt.
fn fetch_envelopes(c: &Cfg, k: &Keys, subs: &[Peer], path: &str, query: &str) -> Vec<Envelope> {
    let mut out = Vec::new();
    for (url, keys) in endpoints(c, k, subs) {
        // Past ~100 keys the URL exceeds what the edge accepts, so fetch
        // everything and filter locally. Untrusted keys are dropped below
        // either way, so correctness is unchanged.
        let base = if keys.len() <= MAX_FROM {
            format!("{url}/{path}?from={}", keys.join(","))
        } else {
            format!("{url}/{path}?")
        };
        let full = if query.is_empty() {
            base
        } else {
            format!("{base}&{query}")
        };
        let envs = ureq::get(&full)
            .call()
            .ok()
            .and_then(|mut r| r.body_mut().read_json::<Vec<serde_json::Value>>().ok())
            .map(|v| {
                v.into_iter()
                    .filter_map(|x| serde_json::from_value::<Envelope>(x).ok())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        out.extend(envs);
    }

    out.into_iter()
        .filter(|e| {
            // Trust only keys we subscribed to (or ourselves), and only if the
            // signature actually checks out.
            (e.pubkey == k.pubkey() || subs.iter().any(|p| p.pubkey == e.pubkey))
                && crypto::verify(&e.pubkey, &e.sig, &e.signed_message())
        })
        .collect()
}

/// Decrypt an envelope into a Plan, taking identity and ordering from the
/// signed envelope rather than the encrypted body.
fn decrypt_plan(e: &Envelope, k: &Keys) -> Option<Plan> {
    let plain = crypto::decrypt(&e.body, &k.age_secret).ok()?;
    let mut plan: Plan = serde_json::from_slice(&plain).ok()?;
    plan.seq = e.seq;
    plan.pubkey = e.pubkey.clone();
    Some(plan)
}

fn fetch_plans(c: &Cfg, k: &Keys) -> Vec<Plan> {
    let subs = crypto::load_peers();
    fetch_envelopes(c, k, &subs, "plans", "")
        .iter()
        .filter_map(|e| decrypt_plan(e, k))
        .collect()
}

/// A signed "I moved" pointer, if the peer published one.
///
/// Verified against the same key that owns the old address — an unsigned
/// redirect would let anyone hijack a peer by pointing them at their own relay.
fn fetch_forward(url: &str, pubkey: &str, k: &Keys, subs: &[Peer]) -> Option<String> {
    let env: Envelope = ureq::get(&format!("{url}/forward/{pubkey}"))
        .call()
        .ok()
        .and_then(|mut r| r.body_mut().read_json::<serde_json::Value>().ok())
        .and_then(|v| serde_json::from_value(v).ok())?;
    if env.pubkey != pubkey || !crypto::verify(&env.pubkey, &env.sig, &env.signed_message()) {
        return None;
    }
    // The new address is encrypted like everything else, so only peers the
    // mover still trusts learn where they went.
    let _ = subs;
    let plain = crypto::decrypt(&env.body, &k.age_secret).ok()?;
    let plan: Plan = serde_json::from_slice(&plain).ok()?;
    Some(plan.task)
}

/// Newest-first posts from you and everyone you trust.
fn fetch_posts(c: &Cfg, k: &Keys, limit: usize) -> Vec<Plan> {
    let subs = crypto::load_peers();
    let mut posts: Vec<Plan> = fetch_envelopes(c, k, &subs, "posts", &format!("limit={limit}"))
        .iter()
        .filter_map(|e| decrypt_plan(e, k))
        .collect();
    // Merge across relays: each returns its own newest-first run, so the
    // combined list needs re-sorting before truncation.
    posts.sort_by_key(|p| std::cmp::Reverse(p.epoch));
    posts.truncate(limit);
    posts
}

fn publish(
    c: &Cfg,
    k: &Keys,
    status: &str,
    task: &str,
    touching: Vec<String>,
) -> Result<(), String> {
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

    let body = crypto::encrypt(
        &serde_json::to_vec(&plan).map_err(|e| e.to_string())?,
        &recipients(k),
    )?;

    send(c, k, "plan", plan.seq, body)
}

/// Encrypt to self + every peer. Always includes self, or you cannot read your
/// own writes back.
fn recipients(k: &Keys) -> Vec<age::x25519::Recipient> {
    let mut recips = vec![k.age_secret.to_public()];
    for p in crypto::load_peers() {
        match p.age_pub.parse::<age::x25519::Recipient>() {
            Ok(r) => recips.push(r),
            Err(_) => eprintln!("warning: peer {} has an unusable age key", p.label),
        }
    }
    recips
}

/// Sign and PUT an envelope to `kind` ("plan" or "post") on our own relay.
fn send(c: &Cfg, k: &Keys, kind: &str, seq: u64, body: String) -> Result<(), String> {
    let mut env = Envelope {
        pubkey: k.pubkey(),
        seq,
        sig: String::new(),
        body,
    };
    env.sig = k.sign(&env.signed_message());
    let url = format!("{}/{kind}/{}", c.url, k.pubkey());
    ureq::put(&url)
        .send_json(&env)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Publish a signed forwarding pointer at the OLD address.
fn publish_forward(c: &Cfg, k: &Keys, new_addr: &str) -> Result<(), String> {
    let entry = Plan {
        agent: c.agent.clone(),
        pubkey: k.pubkey(),
        seq: now() as u64,
        epoch: now(),
        status: "moved".into(),
        task: new_addr.to_string(),
        touching: vec![],
        project: String::new(),
        eta_s: 0,
    };
    let body = crypto::encrypt(
        &serde_json::to_vec(&entry).map_err(|e| e.to_string())?,
        &recipients(k),
    )?;
    send(c, k, "forward", entry.seq, body)
}

/// Append a post. Posts carry their own seq space, so posting never disturbs
/// claim ordering.
fn post(c: &Cfg, k: &Keys, text: &str) -> Result<(), String> {
    let subs = crypto::load_peers();
    let prev = fetch_envelopes(c, k, &subs, "posts", "limit=1")
        .iter()
        .filter(|e| e.pubkey == k.pubkey())
        .map(|e| e.seq)
        .max()
        .unwrap_or(0);

    let entry = Plan {
        agent: c.agent.clone(),
        pubkey: k.pubkey(),
        seq: prev + 1,
        epoch: now(),
        status: "post".into(),
        task: text.to_string(),
        touching: vec![],
        project: project(),
        eta_s: 0,
    };
    let body = crypto::encrypt(
        &serde_json::to_vec(&entry).map_err(|e| e.to_string())?,
        &recipients(k),
    )?;
    send(c, k, "post", entry.seq, body)
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

    // These work without a relay configured.
    match cmd {
        "help" | "-h" | "--help" => {
            println!("{USAGE}");
            return;
        }
        "--version" | "-V" | "version" => {
            println!("robofinger {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        "upgrade" | "update" => {
            if let Err(e) = selfupdate::cmd_upgrade(&args[1..]) {
                eprintln!("{e}");
                std::process::exit(1);
            }
            return;
        }
        "init" => {
            let mut url = None;
            let mut ns = None;
            let mut agent = None;
            let mut it = args[1..].iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--url" => url = it.next().cloned(),
                    "--ns" => ns = it.next().cloned(),
                    "--agent" => agent = it.next().cloned(),
                    other => {
                        eprintln!("unknown flag {other}\n\n{USAGE}");
                        std::process::exit(1);
                    }
                }
            }
            // Keep values already configured so re-running init is not destructive.
            let existing = config_file();
            let url = url.or_else(|| existing.get("ROBOFINGER_URL").cloned());
            let Some(mut url) = url else {
                eprintln!(
                    "usage: robofinger init --url <relay url> [--ns <room>] [--agent <label>]"
                );
                eprintln!("  the relay URL's path is the namespace, e.g.");
                eprintln!("    https://example.com/plan            your own space");
                eprintln!("    https://example.com/plan/team-a     a shared room");
                std::process::exit(1);
            };
            url = url.trim_end_matches('/').to_string();
            // --ns is sugar for appending a path segment, kept because it reads
            // naturally when joining a shared room.
            if let Some(ns) = ns.filter(|s| !s.is_empty())
                && !url.ends_with(&format!("/{ns}"))
            {
                url = format!("{url}/{ns}");
            }

            let mut body = format!("ROBOFINGER_URL={url}\n");
            if let Some(a) = agent.or_else(|| existing.get("ROBOFINGER_AGENT").cloned()) {
                body.push_str(&format!("ROBOFINGER_AGENT={a}\n"));
            }
            let path = crypto::config_dir().join("config");
            if let Err(e) = std::fs::write(&path, body) {
                eprintln!("write {}: {e}", path.display());
                std::process::exit(1);
            }
            println!("wrote {}", path.display());
            println!("\nyour identity — share this line with collaborators:");
            println!(
                "  {}",
                k.identity_blob(
                    &hostname().unwrap_or_else(|| "agent".into()),
                    Some(crypto::Home { url: url.clone() })
                )
            );
            println!("\nthey run:  robofinger peer add <your blob>");
            println!("you run:   robofinger peer add <their blob>");
            println!("\nboth directions are required — adding a peer both subscribes to");
            println!("them and lets them decrypt your plans.");

            hooks::maybe_install();

            // Hooks give you conflict warnings; this makes the agent actually
            // publish claims. Without it `touching` stays empty and every
            // check passes trivially.
            println!("\nAdd this to ~/.claude/CLAUDE.md so your agent publishes claims:\n");
            println!("{}", hooks::CLAUDE_MD);
            return;
        }
        "hooks" => {
            let sub = args.get(1).map(String::as_str).unwrap_or("");
            let scope = if args.iter().any(|a| a == "--project") {
                hooks::Scope::Project
            } else {
                hooks::Scope::Account
            };
            let r = match sub {
                "install" => hooks::install(true, scope),
                "uninstall" | "remove" => hooks::uninstall(scope),
                "" | "show" => {
                    println!("{}", hooks::CLAUDE_MD);
                    return;
                }
                _ => {
                    eprintln!("usage: robofinger hooks install|uninstall|show");
                    std::process::exit(1);
                }
            };
            match r {
                Ok(m) => println!("{m}"),
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
            return;
        }
        "id" => {
            let label = args
                .get(1)
                .cloned()
                .or_else(hostname)
                .unwrap_or_else(|| "agent".into());
            let home = cfg().map(|c| crypto::Home { url: c.url });
            println!("{}", k.identity_blob(&label, home));
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
                "update" => {
                    let Some(which) = args.get(2) else {
                        eprintln!("usage: robofinger peer update <label>");
                        std::process::exit(1);
                    };
                    let Some(c) = cfg() else {
                        eprintln!("not configured — run robofinger init first");
                        std::process::exit(1);
                    };
                    let Some(idx) = peers.iter().position(|p| &p.label == which) else {
                        eprintln!("no peer named {which}");
                        std::process::exit(1);
                    };
                    let old = peers[idx].clone();
                    let Some(dest) = fetch_forward(old.endpoint(&c.url), &old.pubkey, &k, &peers)
                    else {
                        println!("{which} has not published a forwarding pointer");
                        return;
                    };
                    match Peer::parse(&dest) {
                        Ok(mut new) => {
                            // The move is only trustworthy because it was signed
                            // by the same key. Refuse a "move" to a different
                            // identity — that is not a move, it is a swap.
                            if new.pubkey != old.pubkey {
                                eprintln!(
                                    "refusing: forward points at a DIFFERENT key\n  old {}\n  new {}",
                                    &old.pubkey[..16],
                                    &new.pubkey[..16]
                                );
                                std::process::exit(1);
                            }
                            new.label = old.label.clone();
                            peers[idx] = new.clone();
                            match crypto::save_peers(&peers) {
                                Ok(_) => println!(
                                    "{which} updated -> {}",
                                    new.home.map(|h| h.url).unwrap_or_default()
                                ),
                                Err(e) => {
                                    eprintln!("{e}");
                                    std::process::exit(1);
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("forward is not a valid address: {e}");
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
                        return;
                    }
                    let verbose = args.iter().any(|a| a == "-v" || a == "--verbose");
                    // Last-seen needs the relay; without config we still list
                    // the book, just without activity.
                    let seen: std::collections::HashMap<String, i64> = match cfg() {
                        Some(c) => {
                            let mut m = std::collections::HashMap::new();
                            for pl in fetch_plans(&c, &k) {
                                let e = m.entry(pl.pubkey.clone()).or_insert(pl.epoch);
                                if pl.epoch > *e {
                                    *e = pl.epoch;
                                }
                            }
                            for pl in fetch_posts(&c, &k, 100) {
                                let e = m.entry(pl.pubkey.clone()).or_insert(pl.epoch);
                                if pl.epoch > *e {
                                    *e = pl.epoch;
                                }
                            }
                            m
                        }
                        None => Default::default(),
                    };
                    let t = now();
                    for p in &peers {
                        let where_ = match &p.home {
                            Some(h) => h
                                .url
                                .trim_start_matches("https://")
                                .trim_start_matches("http://")
                                .to_string(),
                            None => "(your relay)".into(),
                        };
                        let last = match seen.get(&p.pubkey) {
                            Some(e) => ago(t - e),
                            None => "—".into(),
                        };
                        println!(
                            "{:<14} {:<14} {:<38} {}",
                            p.label,
                            &p.pubkey[..12],
                            where_,
                            last
                        );
                        // Surface a move, but never follow it automatically: a
                        // stolen key could otherwise silently repoint you at an
                        // attacker's relay.
                        if let Some(dest) = cfg()
                            .and_then(|c| fetch_forward(p.endpoint(&c.url), &p.pubkey, &k, &peers))
                        {
                            println!("               ↳ moved to {dest}");
                            println!(
                                "                 accept with: robofinger peer update {}",
                                p.label
                            );
                        }
                        if verbose {
                            println!("               {}", p.to_blob());
                        }
                    }
                }
                _ => {
                    eprintln!("usage: robofinger peer add|rm|list|update [-v]");
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
        // A first run should orient you, not just fail. Bare invocation is the
        // most likely way someone arrives here.
        if cmd.is_empty() {
            println!("robofinger — not set up yet.\n");
            println!("  robofinger init --url <relay url>");
            println!("      e.g. --url https://example.com/plan   (the path is your namespace)\n");
            println!("  robofinger --help    all commands");
            return;
        }
        eprintln!("not configured yet — run: robofinger init --url <relay url>");
        std::process::exit(1);
    };

    match cmd {
        "claim" => {
            let task = args.get(1).cloned().unwrap_or_default();
            let globs: Vec<String> = if args.len() > 2 {
                args[2..].to_vec()
            } else {
                vec![]
            };
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
            let Some(path) = path else {
                std::process::exit(0)
            };

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
        "post" => {
            // Prefer args; fall back to stdin so `... | robofinger post` works
            // and prose isn't trapped behind shell quoting.
            let text = if args.len() > 1 {
                args[1..].join(" ")
            } else {
                let mut buf = String::new();
                let _ = std::io::stdin().read_to_string(&mut buf);
                buf.trim_end().to_string()
            };
            if text.is_empty() {
                eprintln!("nothing to post (pass text, or pipe it on stdin)");
                std::process::exit(1);
            }
            match post(&c, &k, &text) {
                Ok(_) => println!("posted ({} chars)", text.chars().count()),
                Err(e) => {
                    eprintln!("post failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        "log" => {
            let limit = args
                .iter()
                .position(|a| a == "-n")
                .and_then(|i| args.get(i + 1))
                .and_then(|s| s.parse().ok())
                .unwrap_or(20usize);
            let who = args
                .iter()
                .position(|a| a == "--peer")
                .and_then(|i| args.get(i + 1).cloned());
            let subs = crypto::load_peers();
            let mut any = false;
            for p in fetch_posts(&c, &k, limit) {
                if let Some(w) = &who {
                    let matches =
                        p.agent == *w || subs.iter().any(|s| &s.label == w && s.pubkey == p.pubkey);
                    if !matches {
                        continue;
                    }
                }
                println!("{} {}\n{}\n", stamp(p.epoch), p.agent, p.task);
                any = true;
            }
            if !any {
                println!("no posts yet — write one with: robofinger post \"...\"");
            }
        }
        "moved" => {
            let Some(new_addr) = args.get(1) else {
                eprintln!("usage: robofinger moved <your new address>");
                eprintln!("  publishes a signed pointer at your OLD address so peers can find you");
                std::process::exit(1);
            };
            if let Err(e) = crypto::Peer::parse(new_addr) {
                eprintln!("that does not look like an address: {e}");
                std::process::exit(1);
            }
            match publish_forward(&c, &k, new_addr) {
                Ok(_) => {
                    println!("published forwarding pointer -> {new_addr}");
                    println!("peers see it in `peer list`; it expires in a year.");
                }
                Err(e) => {
                    eprintln!("failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        "watch" => watch(&c, &k),
        // `robofinger` alone is your own status; `robofinger alice` is a peer.
        // Falling through to a peer lookup keeps the main verb short, the way
        // `finger alice` was the whole interface.
        "" => show_self(&c, &k),
        other => match finger(&c, &k, other) {
            Ok(()) => {}
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        },
    }
}

/// `robofinger` with no arguments: your own status, the way `finger` with no
/// arguments showed the local machine.
fn show_self(c: &Cfg, k: &Keys) {
    let t = now();
    let mine: Vec<Plan> = fetch_plans(c, k)
        .into_iter()
        .filter(|p| p.pubkey == k.pubkey())
        .collect();

    println!("{} @ {}", c.agent, c.url);
    println!("{}", k.pubkey());

    match mine.iter().find(|p| p.live(t)) {
        Some(p) if !p.touching.is_empty() => {
            println!("\nworking: {}", p.task);
            for g in &p.touching {
                println!("  claiming {}/{}", p.project, g);
            }
        }
        Some(p) => println!("\nworking: {}", p.task),
        None => println!("\nno active claim"),
    }

    let posts: Vec<Plan> = fetch_posts(c, k, 3)
        .into_iter()
        .filter(|p| p.pubkey == k.pubkey())
        .collect();
    if !posts.is_empty() {
        println!("\nrecent posts:");
        for p in posts {
            println!("  {} {}", stamp(p.epoch), first_line(&p.task));
        }
    }

    let peers = crypto::load_peers();
    let live = fetch_plans(c, k)
        .iter()
        .filter(|p| p.pubkey != k.pubkey() && p.live(t))
        .count();
    println!("\n{} peer(s), {live} with active claims", peers.len());
    println!("\nrobofinger <peer>   look someone up");
    println!("robofinger --help   all commands");
}

/// `robofinger alice` — one peer's plan and posts, like fingering them.
fn finger(c: &Cfg, k: &Keys, who: &str) -> Result<(), String> {
    let peers = crypto::load_peers();
    let peer = peers
        .iter()
        .find(|p| p.label == who || p.pubkey.starts_with(who))
        .ok_or_else(|| {
            // A typo should not dump the usage screen; suggest the nearest
            // label instead, or explain the two ways to get here.
            let near: Vec<&str> = peers
                .iter()
                .map(|p| p.label.as_str())
                .filter(|l| l.starts_with(&who[..who.len().min(2)]))
                .collect();
            if near.is_empty() {
                format!(
                    "no peer named {who:?} and no such command\n  robofinger --help    all commands\n  robofinger peer list  who you follow"
                )
            } else {
                format!("no peer named {who:?} — did you mean: {}", near.join(", "))
            }
        })?;

    let t = now();
    println!("{} @ {}", peer.label, peer.endpoint(&c.url));
    println!("{}", peer.pubkey);

    if let Some(dest) = fetch_forward(peer.endpoint(&c.url), &peer.pubkey, k, &peers) {
        println!("\n↳ moved to {dest}");
        println!("  accept with: robofinger peer update {}", peer.label);
    }

    match fetch_plans(c, k)
        .into_iter()
        .find(|p| p.pubkey == peer.pubkey)
    {
        Some(p) if p.live(t) && !p.touching.is_empty() => {
            println!("\nworking: {}", p.task);
            for g in &p.touching {
                println!("  claiming {}/{}", p.project, g);
            }
            println!("  since {}", ago(t - p.epoch));
        }
        Some(p) if p.live(t) => println!("\nworking: {} ({})", p.task, ago(t - p.epoch)),
        Some(_) => println!("\nno active claim"),
        None => println!("\nno plan visible"),
    }

    let posts: Vec<Plan> = fetch_posts(c, k, 20)
        .into_iter()
        .filter(|p| p.pubkey == peer.pubkey)
        .collect();
    if posts.is_empty() {
        println!("\nno posts (or they predate your key exchange)");
    } else {
        println!();
        for p in posts {
            println!("{} {}", stamp(p.epoch), p.agent);
            println!("{}\n", p.task);
        }
    }
    Ok(())
}

/// First line of a possibly multi-line post, for compact listings.
fn first_line(s: &str) -> String {
    let line = s.lines().next().unwrap_or("");
    if line.chars().count() > 60 {
        format!("{}…", line.chars().take(60).collect::<String>())
    } else {
        line.to_string()
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
    let url = if want.len() <= MAX_FROM {
        format!("{ws_url}/subscribe?from={}", want.join(","))
    } else {
        format!("{ws_url}/subscribe")
    };
    let (mut sock, _) = match tungstenite::connect(&url) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("connect failed: {e}");
            std::process::exit(1);
        }
    };
    eprintln!("watching {} as {} ({} peer(s))", c.url, c.agent, subs.len());

    // Verify signature, then decrypt. Anything that fails either step is not
    // shown — an unverified plan is worse than no plan.
    let show = |v: &serde_json::Value| {
        let Ok(e) = serde_json::from_value::<Envelope>(v.clone()) else {
            return;
        };
        if e.pubkey != k.pubkey() && !subs.iter().any(|p| p.pubkey == e.pubkey) {
            return; // not subscribed
        }
        if !crypto::verify(&e.pubkey, &e.sig, &e.signed_message()) {
            eprintln!(
                "dropped envelope with bad signature from {}...",
                &e.pubkey[..8]
            );
            return;
        }
        let Ok(plain) = crypto::decrypt(&e.body, &k.age_secret) else {
            return;
        };
        let Ok(p) = serde_json::from_slice::<Plan>(&plain) else {
            return;
        };
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
            glob::Pattern::new(g)
                .map(|pat| pat.matches(rel))
                .unwrap_or(false)
        })
    }

    #[test]
    fn glob_and_exact_paths_match() {
        let p = plan(
            "peer",
            "demo",
            &["src/auth/**", "src/middleware.ts"],
            "working",
            0,
        );
        assert!(matches(&p, "src/auth/session.ts", "demo", "me"), "glob");
        assert!(
            matches(&p, "src/auth/deep/x.ts", "demo", "me"),
            "nested glob"
        );
        assert!(matches(&p, "src/middleware.ts", "demo", "me"), "exact");
        assert!(
            !matches(&p, "README.md", "demo", "me"),
            "unclaimed is clean"
        );
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
        assert!(
            !matches(&stale, "src/auth/session.ts", "demo", "me"),
            "stale"
        );
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
