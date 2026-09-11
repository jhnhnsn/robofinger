//! robofinger — agent plan sync over a Cloudflare relay.
//!
//!   robofinger <peer>                     look someone up
//!   robofinger claim "<task>" <glob>...   publish a claim
//!   robofinger list                       who you follow and what they hold
//!   robofinger release [--note "<t>"]     drop claims (status stays working)
//!   robofinger done                       mark finished
//!   robofinger since                      what peers did since you last looked
//!   robofinger log [--since <when>]       the whole timeline
//!   robofinger ask [--to <peer>] "<t>"    raise something for the team
//!   robofinger answer --to <peer> "<t>"   reply to a question
//!   robofinger check <path>               exit 0 clean, 0 + hook JSON on conflict
//!   robofinger id [label]                 print your shareable identity blob
//!   robofinger add|rm|list                manage who you follow
//!   robofinger --upgrade                  update robofinger itself
//!
//! Plans are signed (Ed25519) and encrypted (age) client-side. The relay
//! stores opaque ciphertext and can verify signatures but never read contents.
//!
//! Env: ROBOFINGER_URL (e.g. https://example.com/plan — path is the namespace)
//!      ROBOFINGER_ALIAS (display name for this machine, defaults to hostname)
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
/// Claims shown per agent: the live one and what it was doing before.
const CLAIM_HISTORY: usize = 2;
/// Timeline entries are meant to be scanned, not read. A robot that needs the
/// full story has the repo; this is the line that says where to look.
const MAX_ENTRY: usize = 280;
/// Most entries one request will return. Matches the relay's own cap, so
/// asking for more just wastes the round trip.
const MAX_POST_LIMIT: usize = 100;
/// Entries the SessionStart hook fetches. It consumes the cursor, so this has
/// to be generous enough that a busy team's backlog is not silently skipped.
const START_ENTRIES: usize = 100;
/// Ceiling on a single relay request, for a command a person is waiting on.
/// Generous, because a slow network should still work; the point is a bound.
const NET_TIMEOUT: u64 = 10;
/// The same, for the hooks. Much tighter, because these run inside a coding
/// agent's session and their whole contract is that a broken relay costs you
/// warnings, never your session. A command makes several requests in sequence
/// — across relays, and a claim reads before it writes — so the wall-clock
/// worst case is a multiple of this, which is what makes 10s the wrong number
/// here even though it bounds each request correctly.
///
/// Two seconds is far above the ~100-300ms a healthy relay takes, so a real
/// answer still arrives; anything slower is not worth an agent's time.
const HOOK_TIMEOUT: u64 = 2;

/// How old your own claim must be before a session start calls it abandoned.
/// Long enough that resuming a session you were just working in stays silent,
/// short enough to catch one left overnight.
const STALE_GRACE: i64 = 15 * 60;
/// Entries it actually prints. The rest are counted and pointed at, because
/// this lands in a context window the agent still needs for its real work.
/// Questions are exempt — they are the reason the block exists.
const START_SHOWN: usize = 8;

/// What an agent should do with any of this. Appended to the SessionStart
/// block, because the hook is the one place every agent reliably reads —
/// CLAUDE_MD is advisory and a fresh session may not have it.
///
/// The escalation rule is deliberately concrete. "Use your judgment" produces
/// agents that either never ask or ask constantly, and which one you get
/// varies by model and by session.
const GUIDANCE: &str = "\
How to use this:
  - A question marked [FOR YOU] is addressed to this agent. Answer it with
    `robofinger answer --to <peer> --re <id> \"…\"` before starting new work.
  - Raise your own with `robofinger ask [--to <peer>] \"…\"` — and give the
    options, not just the problem: \"both of us want src/auth; I can take the
    API layer instead, or wait for your release. Which?\"
  - Ask a HUMAN, rather than deciding alone, when: two agents want the same
    path and neither has yielded; a peer's claim is well past its ETA and you
    cannot tell if it died; a peer asks you something whose answer changes work
    outside your task; or answering would mean undoing a teammate's work.
    Say what you would do by default and what the alternatives cost.
  - Everything else — an unrelated claim, a release, a note — is context.
    Read it and carry on. Do not reply to be polite; there is no audience.
  - The full workflow, including anything this team added, is in
    ROBOFINGER.md at the repo root. Read it if any of the above is unclear.";

const USAGE: &str = "\
robofinger — coordination for teams of coding agents.

  robofinger                    what you are working on
  robofinger sam                what someone else is

THE TIMELINE

  Every claim and release lands here, so a robot can see what its teammates
  have been doing without waiting for a commit.

  since [--peer <label>] [-n N] what has happened since you last looked
      robofinger since          (advances your cursor; run it again, it is empty)
      robofinger since --peer sam
      (--peer is a filtered read and does NOT advance your cursor, so it
       cannot swallow entries from everyone else)

  log [-n N] [--peer <label>] [--since <when>] [--ids]
                                the whole timeline, newest first
      robofinger log
      robofinger log --since 2h        a window; does not move your cursor
      robofinger log --peer sam
      robofinger log --ids             show entry ids, for `answer --re`

  ask [--to <peer>] \"<text>\"      raise something the team should settle
      robofinger ask --to bob \"both of us want src/auth. I can take the API
        layer instead, or wait for your release. Which?\"
      robofinger ask \"should we split the migration, or one agent takes both?\"
      (give the options, not just the problem. --to names one agent and marks
       it FOR YOU in their session; without it the whole team sees it)

  answer --to <peer> [--re <id>] \"<text>\"
      robofinger answer --to alice --re 42 \"take the API layer; auth frees up
        in ~20m\"
      (--re quotes the question back, so the asker sees what was answered.
       ids come from `robofinger since --ids`)

  post [--to <peer>] [--group <name>] \"<text>\"
                                leave a note on the timeline
      robofinger post \"blocked: needs the migration merged first\"
      git log --oneline -5 | robofinger post
      robofinger post --group work \"shipping the auth migration\"
      (no --group means everyone you follow; a group encrypts to just those
       peers, so the rest cannot decrypt it at all)

PEOPLE

  id [label]                    print your address, to share with someone
      robofinger id
      robofinger id laptop

  add <address> [--as <name>] [--group <a,b>]
                                follow them (and let them read you)
      robofinger add https://sam@relay.example.com/plan/u/kWJQ…#age1x3h…
      robofinger add <address> --as samantha    ignore their suggested name

  list [-v]                     who you follow, where, and what they hold
      robofinger list
      robofinger list -v        also print each full address

  rm <label>                    unfollow; your next post is opaque to them
      robofinger rm sam

  update <label>                accept a peer's move to a new relay
      robofinger update sam

  moved <new address>           tell your peers you have moved
      robofinger moved https://newhost.example.com/plan/u/fHC…#age1tj2…

AGENTS

  claim \"<task>\" <glob>...      announce what you are touching
      robofinger claim \"migrate session store\" 'src/auth/**'
      robofinger claim \"fix retry backoff\" 'src/http/**' 'src/net/*.rs'

  release [--note \"<text>\"]     drop your claims, stay working
      robofinger release --note \"dual-write landed, rollback is a flag\"
      (the note says what happened; without one the entry records how long
       the claim was held)

  done                          mark finished
  check <path>                  conflict check; reads hook JSON on stdin

  Your agent runs these through the hooks. You rarely type them.

SETUP

  init --url <url> [--alias <name>] [--no-hooks]
                                write config, make keys, wire in your agent
      robofinger init --url https://relay.example.com
      robofinger init --url <url> --alias laptop     name this machine
      robofinger init --url <url> --hooks-user       every repo, not just this
      robofinger init --url <url> --no-hooks         config only

      Installs hooks into <repo>/.claude/settings.json and writes
      ROBOFINGER.md at the repo root — the workflow your agent reads. Commit
      both and your teammates get them on clone. Without hooks robofinger
      does nothing: your agent neither sees peer claims nor publishes its own.

      A relay hosted under a path keeps that path as its namespace, so
      https://example.com/plan and .../plan/team-a are separate rooms. You
      rarely need one; --ns <room> appends it if you do.

  hooks install [--user]        let your coding agent use robofinger
      robofinger hooks install              just this repo, commit to share
      robofinger hooks install --user       every project on this machine
      (also writes/refreshes ROBOFINGER.md, keeping your Team conventions)
  hooks show                    print the workflow file
  hooks uninstall [--user]      remove them again

  --upgrade [--check] [--yes]   update robofinger itself
      robofinger --upgrade --check    is there a newer version?
      robofinger --upgrade            install it
  --version                     print version

Config lives in ~/.config/robofinger/config. ROBOFINGER_URL and
ROBOFINGER_ALIAS override it. Keys never leave this machine.";

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Plan {
    /// Human label for this machine. Serialised as "agent" for compatibility
    /// with plans already published; `alias` is what users type.
    #[serde(rename = "agent")]
    alias: String,
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
    /// Echoed inside the encrypted body so a reader can label a claim without
    /// trusting the cleartext envelope.
    #[serde(default)]
    instance: String,
    /// When `touching` last *changed*, as opposed to when this plan was last
    /// published. A working agent republishes on every edit, so `epoch` says
    /// nothing about how long a claim has been held — and the difference
    /// between the two is exactly what "idle" means.
    #[serde(default)]
    claimed_at: i64,
    /// Timeline entries only: which event this is — "claim", "release",
    /// "done", "ask" or "answer". Empty is a note somebody wrote by hand,
    /// which is also what every pre-0.3 post carries, so old entries still
    /// render.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    kind: String,
    /// Who this entry is addressed to, as the *publisher's* label for them.
    ///
    /// Advisory routing, not access control: the entry is still encrypted to
    /// everyone you follow, so the team sees the exchange rather than two
    /// agents negotiating in private. What it changes is whose session flags
    /// it as needing a reply.
    ///
    /// A label, not a pubkey, because that is what a human or an agent types.
    /// `addressed_to_me` resolves it against both sides' labels, since the two
    /// ends routinely disagree about what a peer is called.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    to: String,
    /// The `id` of the entry this answers, so an exchange can be followed.
    #[serde(default, skip_serializing_if = "is_zero")]
    reply_to: i64,
    /// Timeline entries only: the globs the event concerned.
    ///
    /// Not `touching`, which is the *live* claim and is empty on exactly the
    /// event that most needs to name paths — a release.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    globs: Vec<String>,
    /// Relay row id, from the envelope. Read cursor for `since`; never
    /// published — see `Envelope::id`.
    #[serde(default, skip_serializing)]
    id: i64,
}

fn is_zero(n: &i64) -> bool {
    *n == 0
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
    /// Which agent on this machine this process is. Empty for a single-agent
    /// install, which is the default and what most people run.
    instance: String,
    /// Relay base URL. Its path IS the namespace, so
    /// `https://example.com/plan` and `https://example.com/plan/team-a` are
    /// separate rooms with separate storage.
    url: String,
    alias: String,
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
    // ROBOFINGER_AGENT still works; "agent" was confusing in a tool that also
    // coordinates AI agents.
    let alias = get("ROBOFINGER_ALIAS")
        .or_else(|| get("ROBOFINGER_AGENT"))
        .or_else(hostname)
        .unwrap_or_else(|| "unknown".into());
    // Per-agent, so two Claudes in one repo differ with no setup at all.
    //
    // Deliberately NOT the pid: PreToolUse runs as a fresh process, so a
    // pid-derived instance would read as a different agent than the one
    // holding the claim and every session would conflict with itself. The
    // signal has to be stable across a session's subprocesses, which is what
    // these two are — both verified to survive subprocess inheritance.
    // Empty stays on the pre-0.2 wire format for headless use (CI, cron, a
    // pipe), where one runner is legitimately one worker.
    let instance = get("ROBOFINGER_INSTANCE")
        .or_else(|| session_key().map(|k| slot_for(&k)))
        .unwrap_or_default();
    Some(Cfg {
        url,
        alias,
        instance,
    })
}

/// A value that is stable for one working session and differs between
/// sessions, or None when there is no session concept at all.
///
/// Both survive subprocess inheritance, which is the property that matters:
/// the PreToolUse hook must resolve to the same agent that took the claim.
/// TERM_SESSION_ID covers any tool launched from a terminal — aider, codex, a
/// hand-typed `robofinger claim` — without naming a single vendor. A
/// GUI-launched agent may have neither; it still gets a distinct slot below,
/// just not a stable one across restarts.
fn session_key() -> Option<String> {
    ["CLAUDE_CODE_SESSION_ID", "TERM_SESSION_ID"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|s| !s.is_empty()))
}

/// How long an unused slot reservation is honoured. Past this the name is free
/// again, so a machine does not accumulate claude-1..claude-99 forever.
const SLOT_TTL: i64 = 24 * 3600;

/// Map an opaque session key to a short readable name: claude-1, claude-2.
///
/// Without this the instance would be a raw UUID, which is what `robofinger
/// list` would then print on every line. Reservations are keyed by session so
/// the same session keeps its name across subprocesses and restarts, and they
/// expire by timestamp exactly like claims do — a killed agent's slot ages out
/// rather than needing a liveness check.
fn slot_for(key: &str) -> String {
    slot_in(&crypto::config_dir(), key, now())
}

/// `slot_for`, with the directory and clock injected so it is testable without
/// mutating process-global env.
fn slot_in(dir: &std::path::Path, key: &str, t: i64) -> String {
    let path = dir.join("instances");
    let mut rows: Vec<(String, String, i64)> = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let mut f = l.splitn(3, '\t');
            let (k, name, ts) = (f.next()?, f.next()?, f.next()?);
            let ts: i64 = ts.parse().ok()?;
            (t - ts < SLOT_TTL).then(|| (k.to_string(), name.to_string(), ts))
        })
        .collect();

    let name = match rows.iter_mut().find(|(k, _, _)| k == key) {
        Some(row) => {
            row.2 = t; // refresh, so an active session never ages out
            row.1.clone()
        }
        None => {
            // Lowest free number, so closing tab 1 and opening a new one
            // reuses claude-1 instead of climbing forever.
            let taken: std::collections::HashSet<&str> =
                rows.iter().map(|(_, n, _)| n.as_str()).collect();
            let name = (1..)
                .map(|i| format!("claude-{i}"))
                .find(|n| !taken.contains(n.as_str()))
                .unwrap_or_else(|| "claude-1".into());
            rows.push((key.to_string(), name.clone(), t));
            name
        }
    };

    // Best effort: a losing racer just re-reads its own reservation next run,
    // and a read-only home degrades to a working (if unstable) name.
    let body: String = rows
        .iter()
        .map(|(k, n, ts)| format!("{k}\t{n}\t{ts}\n"))
        .collect();
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::write(&path, body);
    name
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
/// A bare duration, for when the sentence already supplies the tense.
fn dur(secs: i64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

fn ago(secs: i64) -> String {
    match secs {
        s if s < 0 => "just now".into(),
        s if s < 60 => format!("{s}s ago"),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86_400 => format!("{}h ago", s / 3600),
        // Past a day, "4d ago" is the point where you start wanting the
        // actual date — which is exactly when a claim is stale enough to
        // matter. Keep the relative form, since it is what you read first.
        s => format!("{}d ago ({})", s / 86_400, stamp(now() - s)),
    }
}

/// Clip to `max` characters, with an ellipsis when anything was cut.
///
/// Counts characters, not bytes: a byte slice at an arbitrary offset panics on
/// any multi-byte character, and a task description is prose.
fn truncate(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        None => s.to_string(),
        Some((i, _)) => format!("{}…", &s[..i]),
    }
}

/// Civil date-time from a unix timestamp, in local time.
///
/// Hand-rolled rather than pulling in `chrono` — this is one line of output in
/// a log command, not worth a dependency in a binary that sits beside private
/// keys. Days-since-epoch to y/m/d via the standard civil-from-days algorithm.
///
/// Local rather than UTC because these sit beside "10m ago" on the same
/// screen: a stamp seven hours off from the wall clock reads as a bug even
/// when it is correct. The offset comes from libc, so DST is handled.
fn stamp(epoch: i64) -> String {
    let epoch = epoch + utc_offset(epoch);
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

/// Seconds east of UTC at `epoch`, from the platform's zone rules.
///
/// Shelling out to `date` beats carrying a tz database, and this runs once
/// per printed line. Falls back to UTC if it fails, which is the old
/// behaviour rather than a wrong one.
fn utc_offset(epoch: i64) -> i64 {
    let out = std::process::Command::new("date")
        .args(["-r", &epoch.to_string(), "+%z"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .or_else(|| {
            // GNU date wants -d @<epoch>; BSD/macOS wants -r.
            std::process::Command::new("date")
                .args(["-d", &format!("@{epoch}"), "+%z"])
                .output()
                .ok()
                .filter(|o| o.status.success())
        });
    let Some(out) = out else { return 0 };
    let z = String::from_utf8_lossy(&out.stdout).trim().to_string();
    // "+HHMM" / "-HHMM"
    if z.len() < 5 {
        return 0;
    }
    let sign = if z.starts_with('-') { -1 } else { 1 };
    let h: i64 = z[1..3].parse().unwrap_or(0);
    let m: i64 = z[3..5].parse().unwrap_or(0);
    sign * (h * 3600 + m * 60)
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

/// Commit subjects made under a claim, for a release nobody wrote a note for.
///
/// The whole design rests on a non-empty record, and an agent that has to
/// compose a note is an agent that often does not. The commits already say
/// what happened, scoped to the paths the claim covered — so derive the note
/// rather than asking for one. An explicit `--note` always wins.
///
/// `since` is the claim time; the globs become git pathspecs, which is why
/// they are passed through unchanged — `src/auth/**` means the same thing to
/// both. Empty when nothing was committed, which is the honest answer and
/// leaves the existing task/duration fallbacks to handle it.
fn commits_since(since: i64, globs: &[String]) -> String {
    let mut args = vec![
        "log".to_string(),
        "--no-merges".to_string(),
        "--format=%s".to_string(),
        format!("--since=@{since}"),
        // Without this, git walks whatever branch HEAD is on *and* its
        // ancestors' merges into other branches.
        "--first-parent".to_string(),
        // ponytail: no --author filter. `--author=@self` reads the *global*
        // user.email, so it silently returns nothing in any repo that
        // overrides it — a common work/personal split. The path scope plus
        // the claim window is enough: a pull brings older author dates, which
        // fall outside `--since` anyway. Revisit if a shared branch turns out
        // to leak teammates' commits into a note.
    ];
    if !globs.is_empty() {
        args.push("--".to_string());
        args.extend(globs.iter().cloned());
    }
    let Ok(out) = std::process::Command::new("git")
        .current_dir(repo_root())
        .args(&args)
        .output()
    else {
        return String::new();
    };
    if !out.status.success() {
        return String::new();
    }
    let subjects: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    // Oldest first reads as a narrative; newest first reads as a stack.
    // `truncate` at the call site cuts the tail, so the earliest work — the
    // part that explains the rest — is what survives a long list.
    subjects
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("; ")
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
    /// Which agent on this identity wrote it. Cleartext because the relay has
    /// to key on it — an instance label inside the ciphertext could not stop
    /// two agents from overwriting each other's plan row.
    ///
    /// Empty means "the only one", which is what every pre-0.2 client sent and
    /// what a single-agent install still sends.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    instance: String,
    seq: u64,
    sig: String,
    body: String,
    /// Relay-assigned row id, present on posts only. Not part of the signed
    /// message and never sent — the relay assigns it, so a client that tried
    /// to would be ignored at best. It is the read cursor `since` stores,
    /// because `seq` is per-(key,instance) and so is not comparable across
    /// the peers whose entries share one timeline.
    #[serde(default, skip_serializing)]
    id: i64,
}

impl Envelope {
    /// Covers `instance`, so a relay that ignores the field rejects the write
    /// rather than silently filing it under the wrong agent. An empty instance
    /// reproduces the pre-0.2 message exactly, so old and new agree on the
    /// single-agent case.
    fn signed_message(&self) -> String {
        if self.instance.is_empty() {
            format!("{}|{}|{}", self.pubkey, self.seq, self.body)
        } else {
            format!(
                "{}|{}|{}|{}",
                self.pubkey, self.instance, self.seq, self.body
            )
        }
    }
}

/// Fetch envelopes, verify signatures, decrypt what we can read.
///
/// Anything that fails verification is dropped silently — a forged or corrupt
/// envelope must never reach the conflict check. Plans encrypted to someone
/// else simply fail to decrypt and are skipped.
/// Every request goes through this, so the timeout below cannot be forgotten
/// at a new call site.
///
/// ureq has no timeout by default, which is fine against a relay that refuses
/// a connection — that fails fast and the hooks fail open. The dangerous case
/// is a relay that ACCEPTS and then never answers: a half-open connection, a
/// wedged worker, a captive portal. `check` and `start` run inside a coding
/// agent's session, so blocking there hangs the agent itself, and the whole
/// design is that a broken relay costs you warnings, never your session.
///
/// `timeout_global` bounds the entire request including reading the body,
/// which is where the hang actually was — a response that starts and stalls
/// would slip past a connect-only timeout.
fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(net_timeout())))
        .build()
        .into()
}

/// Per-request budget for this process. Set once from the subcommand, because
/// threading it through every caller of `fetch_plans`/`fetch_posts` would
/// touch a dozen signatures to carry one constant.
static NET_BUDGET: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(NET_TIMEOUT);

fn net_timeout() -> u64 {
    NET_BUDGET.load(std::sync::atomic::Ordering::Relaxed)
}

/// Group the keys we care about by which relay hosts them.
///
/// Peers may live on a different relay entirely, so a single fetch is not
/// enough. Most setups have exactly one group, keeping the common case to one
/// request.
///
/// **This grouping is a security boundary, not just an optimisation.** Each
/// relay is only ever asked for keys whose home *is* that relay. A signature is
/// valid anywhere — nothing binds it to a host — so a hostile relay can replay
/// a peer's real envelope onto itself, including a stale one that resurrects an
/// expired claim. Because we never ask it for that key, we never see it. If this
/// is ever changed to query relays for keys they do not host, the envelope's
/// source must be checked against the peer's declared home instead.
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
    let net = agent();
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
        let envs = net
            .get(&full)
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
    // From the envelope, not the ciphertext: the relay assigns it, so a
    // publisher cannot forge a cursor position that skips its own entries.
    plan.id = e.id;
    Some(plan)
}

/// Every plan row the relay has, newest first: the live claim for each agent
/// plus the short history behind it.
fn fetch_plans(c: &Cfg, k: &Keys) -> Vec<Plan> {
    let subs = crypto::load_peers();
    fetch_envelopes(c, k, &subs, "plans", "")
        .iter()
        .filter_map(|e| decrypt_plan(e, k))
        .collect()
}

/// The live plan for each (key, agent) — what "what are you doing" means.
///
/// `fetch_plans` returns history too, so anything asking about the present
/// has to collapse it first. Newest seq wins per agent.
/// Whether a peer's live claim is worth showing at session start.
///
/// Scoped to this repo, like `check`: a claim in another project can never
/// conflict here, so showing it is noise in the one context window the agent
/// still needs for its own work.
fn worth_showing(p: &Plan, me: &str, here: &str, t: i64) -> bool {
    p.pubkey != me && p.live(t) && p.project == here
}

fn current_plans(c: &Cfg, k: &Keys) -> Vec<Plan> {
    let mut best: std::collections::HashMap<(String, String), Plan> =
        std::collections::HashMap::new();
    for p in fetch_plans(c, k) {
        let key = (p.pubkey.clone(), p.instance.clone());
        match best.get(&key) {
            Some(cur) if cur.seq >= p.seq => {}
            _ => {
                best.insert(key, p);
            }
        }
    }
    let mut v: Vec<Plan> = best.into_values().collect();
    v.sort_by_key(|p| std::cmp::Reverse(p.epoch));
    v
}

/// A signed "I moved" pointer, if the peer published one.
///
/// Verified against the same key that owns the old address — an unsigned
/// redirect would let anyone hijack a peer by pointing them at their own relay.
fn fetch_forward(url: &str, pubkey: &str, k: &Keys, subs: &[Peer]) -> Option<String> {
    let env: Envelope = agent()
        .get(format!("{url}/forward/{pubkey}"))
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

/// Does the relay hold an envelope for this key that we cannot read?
///
/// Distinguishes "they have published nothing" from "they published but did
/// not encrypt it to me" — the same empty output otherwise, but only the
/// second is something the user can act on.
fn has_unreadable(c: &Cfg, k: &Keys, pubkey: &str, path: &str) -> bool {
    let subs = crypto::load_peers();
    let raw = fetch_envelopes(c, k, &subs, path, "");
    let any = raw.iter().any(|e| e.pubkey == pubkey);
    let readable = raw
        .iter()
        .filter(|e| e.pubkey == pubkey)
        .any(|e| decrypt_plan(e, k).is_some());
    any && !readable
}

/// Newest-first posts from you and everyone you trust.
fn fetch_posts(c: &Cfg, k: &Keys, limit: usize) -> Vec<Plan> {
    fetch_posts_after(c, k, limit, &Default::default())
}

/// Newest-first posts, keeping only entries newer than the caller's watermark.
///
/// `seen` maps a relay URL to the highest row id already consumed there. Row
/// ids are assigned per relay, so they are NOT comparable across relays — a
/// single global cursor would silently cut an arbitrary slice out of a peer
/// hosted somewhere else. Hence one watermark per relay, and hence the filter
/// runs per endpoint rather than over the merged list.
fn fetch_posts_after(
    c: &Cfg,
    k: &Keys,
    limit: usize,
    seen: &std::collections::HashMap<String, i64>,
) -> Vec<Plan> {
    let subs = crypto::load_peers();
    let mut posts: Vec<Plan> = Vec::new();
    for (url, keys) in endpoints(c, k, &subs) {
        let after = seen.get(&url).copied().unwrap_or(0);
        let mut query = format!("limit={limit}");
        if after > 0 {
            query.push_str(&format!("&after={after}"));
        }
        // One relay at a time, because the cursor is only meaningful here.
        // `fetch_envelopes` re-derives every endpoint internally and always
        // includes our own, so narrowing the peer list is not enough — keep
        // only the keys this relay actually hosts.
        let only = subs
            .iter()
            .filter(|p| keys.contains(&p.pubkey))
            .cloned()
            .collect::<Vec<_>>();
        posts.extend(
            fetch_envelopes(c, k, &only, "posts", &query)
                .iter()
                .filter(|e| keys.contains(&e.pubkey))
                .filter_map(|e| decrypt_plan(e, k))
                // A relay that ignores `after` (an older deployment) would
                // resend the whole window, so re-check locally rather than
                // trusting it to have filtered.
                .filter(|p| p.id > after),
        );
    }
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
    // Scope both the sequence and the claim age to this instance. Sharing a
    // counter between two agents on one identity means each one's write looks
    // stale to the other, which the relay correctly rejects 409.
    let mine = current_plans(c, k);
    let prev = mine
        .iter()
        .find(|p| p.pubkey == k.pubkey() && p.instance == c.instance);
    let prev_seq = prev.map(|p| p.seq).unwrap_or(0);
    // Hold claimed_at steady while the same globs are being re-published, so
    // "claimed 20m ago (idle 8m)" can distinguish a long-held claim from a
    // long-abandoned one. Changing what you hold starts the clock over.
    let claimed_at = match prev {
        Some(p) if p.touching == touching && !touching.is_empty() && p.claimed_at > 0 => {
            p.claimed_at
        }
        _ => now(),
    };
    let plan = Plan {
        alias: c.alias.clone(),
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
        instance: c.instance.clone(),
        claimed_at,
        // A claim row is live state, not a timeline entry. The entry is a
        // separate post, written by `timeline` below.
        kind: String::new(),
        globs: vec![],
        to: String::new(),
        reply_to: 0,
        id: 0,
    };

    // Claims deliberately ignore groups: a claim some peers cannot see is a
    // conflict warning that silently does not fire.
    let body = crypto::encrypt(
        &serde_json::to_vec(&plan).map_err(|e| e.to_string())?,
        &recipients(k, None),
    )?;

    send(c, k, "plan", plan.seq, body)
}

/// Encrypt to self + every peer. Always includes self, or you cannot read your
/// own writes back.
/// Age recipients for a publish. Always includes self — omitting it means you
/// cannot read your own writes back, which breaks the seq lookup.
///
/// `group` restricts the list. Excluded peers cannot decrypt the result at all;
/// this is cryptographic, not a display filter. They can still see *that* you
/// published, since the envelope is cleartext for the relay.
fn recipients(k: &Keys, group: Option<&str>) -> Vec<age::x25519::Recipient> {
    let mut recips = vec![k.age_secret.to_public()];
    for p in crypto::load_peers() {
        if let Some(g) = group
            && !p.groups.iter().any(|pg| pg == g)
        {
            continue;
        }
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
        instance: c.instance.clone(),
        seq,
        sig: String::new(),
        body,
        id: 0,
    };
    env.sig = k.sign(&env.signed_message());
    let url = format!("{}/{kind}/{}", c.url, k.pubkey());
    agent()
        .put(&url)
        .send_json(&env)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Publish a signed forwarding pointer at the OLD address.
fn publish_forward(c: &Cfg, k: &Keys, new_addr: &str) -> Result<(), String> {
    let entry = Plan {
        alias: c.alias.clone(),
        pubkey: k.pubkey(),
        seq: now() as u64,
        epoch: now(),
        status: "moved".into(),
        task: new_addr.to_string(),
        touching: vec![],
        project: String::new(),
        instance: c.instance.clone(),
        claimed_at: 0,
        eta_s: 0,
        kind: String::new(),
        globs: vec![],
        to: String::new(),
        reply_to: 0,
        id: 0,
    };
    let body = crypto::encrypt(
        &serde_json::to_vec(&entry).map_err(|e| e.to_string())?,
        &recipients(k, None),
    )?;
    send(c, k, "forward", entry.seq, body)
}

/// Warn when `--to` names nobody the recipient side could resolve.
///
/// Checked against what `addressed_to_me` actually accepts on the other end —
/// their alias or instance, as seen in their published plans — and not only
/// against my local label for them. Those routinely differ: `add --as` exists
/// to override a peer's suggested label, so `--to <their alias>` is correct
/// even when I filed them under something else, and warning on it would train
/// people to ignore the warning.
///
/// Not an error either way: naming one agent of a peer's several is legitimate
/// and I may never have seen that instance publish. But a typo would otherwise
/// be silent, and the whole point of addressing an entry is that someone
/// notices it.
fn warn_unknown_peer(to: &str, c: &Cfg, k: &Keys) {
    let subs = crypto::load_peers();
    let matches_label = subs
        .iter()
        .any(|p| p.label.eq_ignore_ascii_case(to) || p.pubkey.starts_with(to));
    // Plans AND timeline entries: a peer who has posted but not yet claimed is
    // perfectly addressable, and on a fresh team that is the common case.
    let answers_to = |p: &Plan| {
        p.pubkey != k.pubkey()
            && (p.alias.eq_ignore_ascii_case(to)
                || (!p.instance.is_empty()
                    && (p.instance.eq_ignore_ascii_case(to)
                        || format!("{}/{}", p.alias, p.instance).eq_ignore_ascii_case(to))))
    };
    let matches_published = current_plans(c, k).iter().any(&answers_to)
        || fetch_posts(c, k, MAX_POST_LIMIT).iter().any(&answers_to);
    if !matches_label && !matches_published {
        eprintln!("warning: nobody you follow answers to {to:?} — sending anyway.");
        eprintln!("  it still reaches everyone you follow; only the addressing is unrecognised.");
        eprintln!("  see who you follow: robofinger list");
    }
}

/// Is this entry addressed to me?
///
/// `to` is the *publisher's* label for the recipient, and the two ends
/// routinely disagree about what a peer is called — `add --as` exists to
/// override what a peer suggests. So a match is accepted on any of the names
/// that could reasonably mean "me":
///
///   - my alias, which is what I publish under and so what a peer sees and
///     usually adopts as their label for me
///   - my instance, or `alias/instance`, so one agent of several can be named
///
/// Deliberately generous, and case-insensitive. A missed match means an agent
/// never learns a question was for it; a false match means it reads one extra
/// line. Those are not comparable costs, and nothing here is access control —
/// the entry is encrypted to the whole team either way.
fn addressed_to_me(p: &Plan, c: &Cfg) -> bool {
    if p.to.is_empty() {
        return false;
    }
    let to = p.to.trim();
    to.eq_ignore_ascii_case(&c.alias)
        || (!c.instance.is_empty()
            && (to.eq_ignore_ascii_case(&c.instance)
                || to.eq_ignore_ascii_case(&format!("{}/{}", c.alias, c.instance))))
}

/// Does this entry belong to the peer named by `--peer`? No filter matches all.
///
/// Matches either the publisher's own alias or the local label you filed them
/// under, since those routinely differ — `add --as` exists precisely to
/// override what a peer calls itself.
fn peer_matches(p: &Plan, want: Option<&str>, subs: &[Peer]) -> bool {
    match want {
        None => true,
        Some(w) => p.alias == w || subs.iter().any(|s| s.label == w && s.pubkey == p.pubkey),
    }
}

/// `--since` as a unix timestamp: a duration back from now ("2h", "30m",
/// "3d"), or an absolute epoch.
///
/// Deliberately not row ids. An id is per relay and means nothing to the
/// person typing it; the watermark in `since` is the machine-readable cursor,
/// and this is the human one.
fn parse_since(s: &str, now: i64) -> Result<i64, String> {
    let (n, unit) = s.split_at(s.len().saturating_sub(1));
    let mult = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        // No suffix: an absolute epoch.
        _ => {
            return s
                .parse()
                .map_err(|_| format!("--since wants a duration like 2h, or an epoch, not {s:?}"));
        }
    };
    let n: i64 = n
        .parse()
        .map_err(|_| format!("--since wants a duration like 2h, not {s:?}"))?;
    Ok(now.saturating_sub(n.saturating_mul(mult)))
}

/// One timeline entry. Events lead with the verb and the paths, because that
/// is what a reader is scanning for; a hand-written note is just its text.
///
/// Returns false once stdout is gone, so the caller can stop. `robofinger log
/// | head` closes the pipe early and `println!` panics on that rather than
/// exiting — noisy, and the README pipes these commands.
fn show_entry(p: &Plan, show_instance: bool, ids: bool) -> bool {
    use std::io::Write;
    writeln!(
        std::io::stdout(),
        "{}\n",
        render_entry_id(p, show_instance, ids)
    )
    .is_ok()
}

/// The text of one entry, without the trailing blank line.
///
/// Split out from `show_entry` so the SessionStart hook can embed the same
/// rendering in its JSON rather than growing a second, drifting format.
fn render_entry(p: &Plan, show_instance: bool) -> String {
    render_entry_id(p, show_instance, false)
}

/// `render_entry`, optionally prefixing the relay id so it can be quoted back
/// with `--re`. Off by default: the id is machine-facing noise on every line
/// of an ordinary read.
fn render_entry_id(p: &Plan, show_instance: bool, ids: bool) -> String {
    let head = if ids {
        format!("[{}] {} {}", p.id, stamp(p.epoch), who(p, show_instance))
    } else {
        format!("{} {}", stamp(p.epoch), who(p, show_instance))
    };
    if p.kind.is_empty() {
        return format!("{head}\n{}", p.task);
    }
    let paths = if p.globs.is_empty() {
        String::new()
    } else {
        format!(" {}", p.globs.join(" "))
    };
    let detail = if p.task.is_empty() {
        String::new()
    } else {
        format!("  — {}", p.task)
    };
    // A question needs to survive being skimmed in a wall of claim traffic,
    // which is the whole reason it is a distinct kind rather than a note.
    let verb = match p.kind.as_str() {
        "ask" => "ASKS".to_string(),
        "answer" => "answers".to_string(),
        other => other.to_string(),
    };
    let addressed = if p.to.is_empty() {
        String::new()
    } else {
        format!(" → {}", p.to)
    };
    format!("{head}\n  {verb}{addressed}{paths}{detail}")
}

/// Highest relay row id already consumed, per relay URL.
///
/// Keyed on the relay rather than the peer because that is the scope a row id
/// is unique in — see `fetch_posts_after`. A missing or unreadable file means
/// "seen nothing", so the first `since` shows the current window and a
/// read-only home degrades to showing everything every time rather than
/// failing.
fn read_seen(dir: &std::path::Path) -> std::collections::HashMap<String, i64> {
    std::fs::read_to_string(dir.join("seen"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let (url, id) = l.split_once('\t')?;
            Some((url.to_string(), id.parse().ok()?))
        })
        .collect()
}

/// Best effort, like the instance slot file: a losing racer re-reads its own
/// watermark next run, and the worst case is showing an entry twice.
fn write_seen(dir: &std::path::Path, seen: &std::collections::HashMap<String, i64>) {
    let mut rows: Vec<_> = seen.iter().collect();
    rows.sort(); // stable file, so a diff of it means something
    let body: String = rows.iter().map(|(u, id)| format!("{u}\t{id}\n")).collect();
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::write(dir.join("seen"), body);
}

/// Value of `--flag <value>`, if present.
fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

/// The message text in `args[1..]`, with `--flag value` pairs removed.
///
/// Replaces stripping the flag back out of a joined string, which broke as
/// soon as the message itself contained the flag's text — quoting the message
/// is not enough, because the shell has already discarded the quotes by the
/// time this sees it.
fn message(args: &[String], flags: &[&str]) -> String {
    let mut out: Vec<&str> = Vec::new();
    // Each flag is consumed once. A second `--to` is the message talking about
    // the flag, not a second flag — `flag()` reads the first occurrence too, so
    // this keeps the two in agreement about which one was the real one.
    let mut used: Vec<&str> = Vec::new();
    let mut skip = false;
    for a in &args[1..] {
        if skip {
            skip = false;
            continue;
        }
        if flags.contains(&a.as_str()) && !used.contains(&a.as_str()) {
            used.push(a);
            skip = true;
            continue;
        }
        out.push(a);
    }
    out.join(" ")
}

/// My own current plan row for this agent, if there is one.
///
/// Three commands need the row they are about to replace — to name what a
/// claim drops, to carry a task forward across a release, and to decide
/// whether an event is worth a timeline entry.
fn mine(c: &Cfg, k: &Keys) -> Option<Plan> {
    current_plans(c, k)
        .into_iter()
        .find(|p| p.pubkey == k.pubkey() && p.instance == c.instance)
}

/// Write a claim/release/done onto the timeline.
///
/// Best-effort and silent on failure, matching `release`/`done`/`end`: a relay
/// outage must not fail the command or break a session. The claim itself has
/// already been published by the time this runs — losing the journal entry is
/// strictly less bad than losing the coordination.
///
/// Never grouped. `publish` passes `recipients(k, None)` because "a claim some
/// peers cannot see is a conflict warning that silently does not fire", and a
/// claim *event* a peer cannot see is the same hazard one step removed.
fn timeline(c: &Cfg, k: &Keys, kind: &str, text: &str, globs: Vec<String>) {
    let _ = post(c, k, text, None, kind, globs, "", 0);
}

/// Append to the timeline. Posts carry their own seq space, so writing one
/// never disturbs claim ordering.
///
/// `kind` and `globs` are empty for a note somebody wrote by hand and set for
/// a claim/release/done event — the two share this path because they share a
/// stream, and a reader wants them interleaved in one chronological order.
#[allow(clippy::too_many_arguments)]
fn post(
    c: &Cfg,
    k: &Keys,
    text: &str,
    group: Option<&str>,
    kind: &str,
    globs: Vec<String>,
    to: &str,
    reply_to: i64,
) -> Result<(), String> {
    // Ask only for your own posts. `limit=1` over everyone returns whichever
    // peer posted most recently, and filtering that for your own key finds
    // nothing as soon as someone else is newer — seq falls back to 1 and the
    // relay rejects the post 409.
    let me = [Peer {
        label: c.alias.clone(),
        pubkey: k.pubkey(),
        age_pub: String::new(),
        home: None,
        groups: vec![],
    }];
    let prev = fetch_envelopes(c, k, &me, "posts", "limit=1")
        .iter()
        .filter(|e| e.pubkey == k.pubkey())
        .map(|e| e.seq)
        .max()
        .unwrap_or(0);

    let entry = Plan {
        alias: c.alias.clone(),
        pubkey: k.pubkey(),
        seq: prev + 1,
        epoch: now(),
        status: "post".into(),
        task: truncate(text, MAX_ENTRY),
        touching: vec![],
        project: project(),
        eta_s: 0,
        instance: c.instance.clone(),
        claimed_at: 0,
        kind: kind.to_string(),
        globs,
        to: to.to_string(),
        reply_to,
        id: 0,
    };
    let body = crypto::encrypt(
        &serde_json::to_vec(&entry).map_err(|e| e.to_string())?,
        &recipients(k, group),
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

/// Is this plan mine — same key AND same agent on it?
///
/// The instance half is load-bearing: your own key with a *different* agent on
/// it is a real conflict, and two Claudes in one repo is the case this whole
/// feature exists for. Dropping it silently disables conflict warnings between
/// tabs, which looks exactly like working correctly.
fn is_self(p: &Plan, pubkey: &str, instance: &str) -> bool {
    p.pubkey == pubkey && p.instance == instance
}

/// Does this peer have enough going on to name its agents?
///
/// Counted on agents actually holding paths, not on plan rows: a sibling that
/// released still publishes a live row holding nothing, and counting those
/// labelled a peer that only ever had one real claim.
fn names_worth_showing(rows: &[Plan]) -> bool {
    rows.iter().filter(|p| !p.touching.is_empty()).count() > 1
}

/// Peer claims matching `path`, scoped to the current project.
fn conflicts(c: &Cfg, k: &Keys, path: &str) -> Vec<(Plan, String)> {
    let rel = &relative_to_root(path);
    let here = project();
    let t = now();
    let plans = current_plans(c, k);
    let mut hits = Vec::new();
    for p in plans {
        if is_self(&p, &k.pubkey(), &c.instance) || p.project != here || !p.live(t) {
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

    // Hooks run inside an agent's session and must never be what it waits on.
    if matches!(cmd, "check" | "start" | "end") {
        NET_BUDGET.store(HOOK_TIMEOUT, std::sync::atomic::Ordering::Relaxed);
    }

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
        "--upgrade" | "upgrade" => {
            if let Err(e) = selfupdate::cmd_upgrade(&args[1..]) {
                eprintln!("{e}");
                std::process::exit(1);
            }
            return;
        }
        "init" => {
            let mut url = None;
            let mut ns = None;
            let mut alias = None;
            let mut no_hooks = false;
            let mut hook_scope = hooks::Scope::Project;
            let mut it = args[1..].iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--url" => url = it.next().cloned(),
                    "--ns" => ns = it.next().cloned(),
                    "--alias" | "--agent" => alias = it.next().cloned(),
                    // Kept as a no-op: it is in the README, in shell history,
                    // and in people's notes, and erroring on it would be a
                    // worse answer than doing what it always did.
                    "--hooks" => {}
                    "--no-hooks" => no_hooks = true,
                    "--hooks-user" => hook_scope = hooks::Scope::Account,
                    other => {
                        eprintln!("unknown flag {other}\n\n{USAGE}");
                        std::process::exit(1);
                    }
                }
            }
            // Keep values already configured so re-running init is not destructive.
            let existing = config_file();
            let url = url.or_else(|| existing.get("ROBOFINGER_URL").cloned());
            // `--hooks` without `--url` would otherwise be silently dropped:
            // init aborts below, and the user believes hooks were installed.
            // Bare `init` on a terminal asks rather than failing: it is the
            // most likely way someone arrives here, and an error teaches them
            // nothing about what a relay URL even is.
            let url = url.or_else(|| {
                eprintln!("Setting up robofinger.\n");
                eprintln!("  A relay is where your plans are stored. It is a small Worker you");
                eprintln!(
                    "  or your team deploys — see the README to run one free on Cloudflare.\n"
                );
                let u = ask("Relay URL", Some("https://relay.example.com"))?;
                let u = u.trim_end_matches('/').to_string();

                if let Some(a) = ask("Name for this machine", hostname().as_deref())
                    && !a.is_empty()
                {
                    alias = Some(a);
                }

                eprintln!("\n  A namespace splits one relay into separate rooms. Most people");
                eprintln!("  do not need one — leave it blank.");
                if let Some(n) = ask("Namespace (optional)", None)
                    && !n.is_empty()
                {
                    ns = Some(n);
                }
                eprintln!();
                Some(u)
            });

            let Some(mut url) = url else {
                if k.freshly_generated {
                    eprintln!(
                        "note: keys were generated in {} — rerun with --url to finish setup\n",
                        crypto::config_dir().display()
                    );
                }
                eprintln!("usage: robofinger init --url <relay url> [--alias <name>] [--no-hooks]");
                eprintln!("    e.g. https://relay.example.com");
                eprintln!(
                    "  no relay yet? deploy one free: https://github.com/jhnhnsn/robofinger#self-hosting"
                );
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

            let saved_alias = alias
                .clone()
                .or_else(|| existing.get("ROBOFINGER_ALIAS").cloned())
                .or_else(|| existing.get("ROBOFINGER_AGENT").cloned())
                .or_else(hostname)
                .unwrap_or_else(|| "agent".into());

            let mut body = format!("ROBOFINGER_URL={url}\n");
            if let Some(a) = alias
                .or_else(|| existing.get("ROBOFINGER_ALIAS").cloned())
                .or_else(|| existing.get("ROBOFINGER_AGENT").cloned())
            {
                body.push_str(&format!("ROBOFINGER_ALIAS={a}\n"));
            }
            let path = crypto::config_dir().join("config");
            if let Err(e) = std::fs::write(&path, body) {
                eprintln!("write {}: {e}", path.display());
                std::process::exit(1);
            }
            if k.freshly_generated {
                println!(
                    "generated a new identity in {}",
                    crypto::config_dir().display()
                );
                println!("  signing.key  proves it is you — nobody can publish as you without it");
                println!("  age.key      decrypts what peers send you");
                println!(
                    "  back these up; losing them means a new identity and re-adding every peer"
                );
                println!();
            }
            println!("wrote {}", path.display());
            println!("\nyour identity — share this line with collaborators:");
            println!(
                "  {}",
                // Use the alias we just wrote, not the hostname — otherwise
                // init prints a different address than `robofinger id` does.
                k.identity_blob(&saved_alias, Some(crypto::Home { url: url.clone() }))
            );
            println!("\nthey run:  robofinger add <your address>");
            println!("you run:   robofinger add <their address>");
            println!("\nboth directions are required — adding a peer both subscribes to");
            println!("them and lets them decrypt your plans.");

            // Project scope by default. This writes <repo>/.claude/settings.json
            // — a repo-local file the team can commit — rather than the user's
            // global config, so it does not need the consent that touching
            // ~/.claude does. Account scope still asks. Without hooks the tool
            // does nothing at all, and defaulting to off meant every scripted
            // install produced a silent no-op.
            let installed = if no_hooks {
                false
            } else {
                match hooks::install(true, hook_scope) {
                    Ok(m) => {
                        println!("\n{m}");
                        true
                    }
                    Err(e) => {
                        eprintln!("\nhook install failed: {e}");
                        false
                    }
                }
            };

            if installed {
                // The hooks give an agent conflict warnings. This is what
                // teaches it to publish claims in the first place — without
                // it `touching` stays empty and every check passes trivially.
                match hooks::write_workflow(&hooks::repo_dir()) {
                    Ok(m) => println!("{m}"),
                    Err(e) => eprintln!("could not write ROBOFINGER.md: {e}"),
                }
                println!("\ncommit both so your teammates get them on clone.");
            } else {
                println!("\n⚠  No hooks installed — robofinger will not do anything yet.");
                println!("   Your agent cannot see peer claims, and will not publish its own.");
                println!("\n   robofinger hooks install           this repo");
                println!("   robofinger hooks install --user    every repo on this machine");
            }
            return;
        }
        "hooks" => {
            let sub = args.get(1).map(String::as_str).unwrap_or("");
            let scope = if args.iter().any(|a| a == "--user") {
                hooks::Scope::Account
            } else {
                hooks::Scope::Project
            };
            let r = match sub {
                "install" => hooks::install(true, scope).map(|m| {
                    match hooks::write_workflow(&hooks::repo_dir()) {
                        Ok(w) => format!("{m}\n{w}"),
                        Err(e) => format!("{m}\ncould not write ROBOFINGER.md: {e}"),
                    }
                }),
                "uninstall" | "remove" => hooks::uninstall(scope),
                "" | "show" => {
                    println!("{}", hooks::ROBOFINGER_MD);
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
            // An explicit argument wins, then the configured alias, then the
            // hostname. Skipping the configured alias meant `--alias` set at
            // init was silently ignored here while still appearing in plans.
            let label = args
                .get(1)
                .cloned()
                .or_else(|| cfg().map(|c| c.alias))
                .or_else(hostname)
                .unwrap_or_else(|| "agent".into());
            // Without a relay there is nowhere to fetch from, so an address
            // would be unusable. Say so rather than emit a broken one.
            let Some(home) = cfg().map(|c| crypto::Home { url: c.url }) else {
                eprintln!("no relay configured — run: robofinger init --url <relay url>");
                eprintln!("your public key is {}", k.pubkey());
                std::process::exit(1);
            };
            println!("{}", k.identity_blob(&label, Some(home)));
            eprintln!("\nshare that line with a peer; they run: robofinger add <it>");
            return;
        }
        "add" | "rm" | "update" | "list" => {
            let sub = cmd;
            let mut peers = crypto::load_peers();
            match sub {
                "add" => {
                    let Some(blob) = args.get(1) else {
                        eprintln!("usage: robofinger add <address>");
                        eprintln!("  get theirs with: robofinger id  (on their machine)");
                        std::process::exit(1);
                    };
                    match Peer::parse(blob) {
                        Ok(mut p) => {
                            if p.pubkey == k.pubkey() {
                                eprintln!("that's your own identity");
                                std::process::exit(1);
                            }
                            // `--as` overrides whatever they suggested.
                            if let Some(i) = args.iter().position(|a| a == "--as")
                                && let Some(name) = args.get(i + 1)
                            {
                                p.label = name.clone();
                            }
                            if let Some(i) = args.iter().position(|a| a == "--group" || a == "-g")
                                && let Some(g) = args.get(i + 1)
                            {
                                p.groups = g
                                    .split(',')
                                    .map(str::trim)
                                    .filter(|s| !s.is_empty())
                                    .map(str::to_string)
                                    .collect();
                            }
                            if p.label.is_empty() {
                                p.label = p.pubkey[..8].to_string();
                            }
                            // The label is only a suggestion, so it must never
                            // silently shadow someone you already follow —
                            // that is how an impostor gets read as a friend.
                            if let Some(clash) = peers
                                .iter()
                                .find(|x| x.label == p.label && x.pubkey != p.pubkey)
                            {
                                let taken = clash.pubkey[..8].to_string();
                                eprintln!(
                                    "You already follow a different key as {:?} ({taken}…).",
                                    p.label
                                );
                                // On a terminal, resolve it here rather than
                                // making them re-run with a flag.
                                match prompt_for_label(&p.label, &peers) {
                                    Some(name) => p.label = name,
                                    None => {
                                        eprintln!(
                                            "  pick another: robofinger add <address> --as <name>"
                                        );
                                        std::process::exit(1);
                                    }
                                }
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
                    let Some(which) = args.get(1) else {
                        eprintln!("usage: robofinger update <label>");
                        eprintln!(
                            "  accepts a peer's published move (robofinger --upgrade updates the tool itself)"
                        );
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
                    let Some(which) = args.get(1) else {
                        eprintln!("usage: robofinger rm <label|pubkey>");
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
                "list" => {
                    if peers.is_empty() {
                        println!(
                            "no peers yet — share `robofinger id`, then `robofinger add <theirs>`"
                        );
                        return;
                    }
                    let verbose = args.iter().any(|a| a == "-v" || a == "--verbose");
                    // Last-seen needs the relay; without config we still list
                    // the book, just without activity.
                    let seen: std::collections::HashMap<String, i64> = match cfg() {
                        Some(c) => {
                            let mut m = std::collections::HashMap::new();
                            for pl in current_plans(&c, &k) {
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
                    // Live claims, indexed by key, so each peer can show what
                    // it is holding right now.
                    //
                    // All of them, not one: a peer running several agents holds
                    // several claims at once, and keying this by pubkey alone
                    // silently kept whichever happened to land last. The hidden
                    // ones are exactly the claims you might collide with.
                    let mut claims: std::collections::HashMap<String, Vec<Plan>> =
                        Default::default();
                    if let Some(c) = cfg() {
                        for p in current_plans(&c, &k) {
                            // Keep live plans holding nothing too: a peer who
                            // released on purpose is worth a line, and dropping
                            // them here made that indistinguishable from a
                            // claim that quietly rotted.
                            if p.pubkey != k.pubkey() && p.live(t) {
                                claims.entry(p.pubkey.clone()).or_default().push(p);
                            }
                        }
                    }
                    // Oldest-held first, so a long-running claim leads and the
                    // order does not shuffle between runs.
                    for v in claims.values_mut() {
                        v.sort_by_key(|p| (p.epoch, p.instance.clone()));
                    }
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
                        let groups = if p.groups.is_empty() {
                            String::new()
                        } else {
                            format!("  [{}]", p.groups.join(","))
                        };
                        println!(
                            "{:<14} {:<14} {:<38} {}{}",
                            p.label,
                            &p.pubkey[..12],
                            where_,
                            last,
                            groups
                        );
                        // What they are actively holding — the thing you most
                        // often opened this list to find out.
                        let held_by = claims.get(&p.pubkey).map(Vec::as_slice).unwrap_or(&[]);
                        // Name the agent only when there is more than one to
                        // tell apart, matching `robofinger` and `finger`.
                        let multi = names_worth_showing(held_by);
                        for pl in held_by {
                            // Age the claim itself. The column above is when
                            // they last published anything, which drifts far
                            // from when they took the claim.
                            let held = ago(t.saturating_sub(pl.epoch));
                            let tag = if multi && !pl.instance.is_empty() {
                                format!("{}: ", pl.instance)
                            } else {
                                String::new()
                            };
                            for g in &pl.touching {
                                println!(
                                    "               {}claiming {}/{}  ({})  since {}",
                                    tag, pl.project, g, pl.task, held
                                );
                            }
                            // Working but holding nothing is a handoff, and
                            // worth a line — silence here looks like a peer
                            // whose claim quietly rotted.
                            if pl.touching.is_empty() && pl.live(t) && !pl.task.is_empty() {
                                // Tasks often already start with "working",
                                // and "working on working X" reads like a bug.
                                let d = pl.task.trim();
                                let lead = if d.to_ascii_lowercase().starts_with("working") {
                                    ""
                                } else {
                                    "working on "
                                };
                                println!("               {tag}released — {lead}{d}");
                            }
                        }
                        // Surface a move, but never follow it automatically: a
                        // stolen key could otherwise silently repoint you at an
                        // attacker's relay.
                        if let Some(dest) = cfg()
                            .and_then(|c| fetch_forward(p.endpoint(&c.url), &p.pubkey, &k, &peers))
                        {
                            println!("               ↳ moved to {dest}");
                            println!(
                                "                 accept with: robofinger update {}",
                                p.label
                            );
                        }
                        if verbose {
                            println!("               {}", p.to_blob());
                        }
                    }
                }
                _ => unreachable!("outer match limits sub to known verbs"),
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
            // A mistyped flag must not become a published claim description.
            if task.starts_with('-') {
                eprintln!("usage: robofinger claim \"<task>\" '<glob>' ['<glob>' …]");
                eprintln!("  the task comes first, quoted; globs after it");
                std::process::exit(2);
            }
            let globs: Vec<String> = if args.len() > 2 {
                args[2..].to_vec()
            } else {
                vec![]
            };
            // A claim replaces your whole list rather than adding to it, so
            // an incremental `claim` silently drops what came before. Say what
            // is being let go.
            let t = now();
            let prev = mine(&c, &k);
            let dropped: Vec<String> = prev
                .iter()
                .filter(|p| p.live(t))
                .flat_map(|p| p.touching.iter().filter(|g| !globs.contains(g)).cloned())
                .collect();
            // A working agent republishes its claim on every edit. Only a real
            // change earns a timeline entry — without this gate a busy session
            // spends its whole write budget (WRITES_PER_MIN) journalling
            // itself, and the relay starts refusing the claims that matter.
            let changed = prev.as_ref().map(|p| &p.touching) != Some(&globs);
            match publish(&c, &k, "working", &task, globs.clone()) {
                Ok(_) => {
                    println!("claimed {globs:?} ({task})");
                    if !dropped.is_empty() {
                        eprintln!(
                            "  released {} — a claim replaces, it does not add",
                            dropped.join(", ")
                        );
                    }
                    if changed {
                        timeline(&c, &k, "claim", &task, globs);
                    }
                }
                Err(e) => {
                    eprintln!("publish failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        "release" => {
            let note = flag(&args, "--note");
            let prev = mine(&c, &k);
            let task = prev.as_ref().map(|p| p.task.clone()).unwrap_or_default();
            let held: Vec<String> = prev
                .as_ref()
                .map(|p| p.touching.clone())
                .unwrap_or_default();
            let _ = publish(&c, &k, "working", &task, vec![]);
            println!("released");
            // Releasing nothing is a no-op, not an event.
            if !held.is_empty() {
                // The note says what happened; the task only ever said what was
                // intended. Prefer an explicit note, then the commits made
                // under the claim — which say what happened without anyone
                // having to write it down — then the intent, then the
                // duration.
                let text = match (&note, prev.as_ref().map(|p| p.claimed_at)) {
                    (Some(n), _) => n.clone(),
                    (None, Some(at)) if at > 0 => {
                        let done = commits_since(at, &held);
                        if done.is_empty() {
                            format!("{task} (held {})", dur(now().saturating_sub(at)))
                        } else {
                            done
                        }
                    }
                    _ => task,
                };
                timeline(&c, &k, "release", &text, held);
            }
        }
        "done" => {
            let held: Vec<String> = mine(&c, &k).map(|p| p.touching).unwrap_or_default();
            let _ = publish(&c, &k, "done", "", vec![]);
            println!("done");
            timeline(&c, &k, "done", "", held);
        }
        // PreToolUse hook: hook JSON on stdin, advisory warning on stdout.
        "check" => {
            // Only read stdin when no path was given. read_to_string blocks
            // until EOF, so doing it unconditionally hangs forever when a
            // human runs `robofinger check <path>` at a terminal — stdin is
            // the tty and nothing ever closes it. Hooks pipe JSON and close,
            // so they are unaffected either way.
            let path = args.get(1).cloned().or_else(|| {
                use std::io::IsTerminal;
                if std::io::stdin().is_terminal() {
                    return None;
                }
                let mut buf = String::new();
                std::io::stdin().read_to_string(&mut buf).ok()?;
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
                    .map(|(p, g)| {
                        // Name the agent, not just the machine: two Claudes on
                        // one identity are both "macbook", and which one is
                        // holding the file is the whole question. Always shown
                        // here, unlike `list` — a conflict means there is by
                        // definition more than one agent in play.
                        format!("{} ({}) claims {}", who(p, true), p.task, g)
                    })
                    .collect();
                // Name the holder, so `--to` can be typed straight from this.
                let holder = hits
                    .first()
                    .map(|(p, _)| p.alias.clone())
                    .unwrap_or_default();
                let msg = format!(
                    "CLAIM CONFLICT on {}:\n{}\n\
                     This is advisory. Four options, roughly in order:\n\
                     1. Work elsewhere — cheapest, and usually right.\n\
                     2. Wait: poll `robofinger check {}` in the background (it prints nothing \
                     once free) and carry on with unblocked work meanwhile. Do not sit idle.\n\
                     3. Ask the holder: `robofinger ask --to {} \"<what you need, and the \
                     options>\"` — they see it at their next session start.\n\
                     4. Ask your user, if neither of you can yield, if the claim looks abandoned, \
                     or if proceeding would undo their work. Say what you would do by default \
                     and what it costs.",
                    path,
                    detail.join("\n"),
                    path,
                    holder
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
        // SessionStart hook: everything the agent needs to coordinate, with
        // no command typed.
        //
        // Three things, in order of how much they demand a response: questions
        // addressed to this agent, what peers hold right now, and what has
        // happened since this agent last looked. The timeline half is the
        // reason `since` exists; leaving it opt-in meant an agent only got it
        // if it read CLAUDE_MD and remembered.
        "start" => {
            let t = now();
            let mut blocks: Vec<String> = Vec::new();

            let live = current_plans(&c, &k);
            let here = project();
            let me = k.pubkey();
            let claims: Vec<String> = live
                .iter()
                .filter(|p| worth_showing(p, &me, &here, t))
                .flat_map(|p| {
                    p.touching
                        .iter()
                        .map(|g| {
                            format!("  {} claims {}/{} ({})", who(p, true), p.project, g, p.task)
                        })
                        .collect::<Vec<_>>()
                })
                .collect();

            // Advance the cursor here, so the agent is not shown the same
            // backlog at every session start. This is the one hook that
            // consumes, and it is the one place an agent reliably reads.
            let dir = crypto::config_dir();
            let mut seen = read_seen(&dir);
            let fresh = fetch_posts_after(&c, &k, START_ENTRIES, &seen);
            let subs = crypto::load_peers();
            for (url, keys) in endpoints(&c, &k, &subs) {
                let high = fresh
                    .iter()
                    .filter(|p| keys.contains(&p.pubkey))
                    .map(|p| p.id)
                    .max()
                    .unwrap_or(0);
                let e = seen.entry(url).or_insert(0);
                *e = (*e).max(high);
            }
            write_seen(&dir, &seen);

            // Mine, and anything I already answered, are not news to me.
            let answered: std::collections::HashSet<i64> = fresh
                .iter()
                .filter(|p| p.pubkey == k.pubkey() && p.kind == "answer")
                .map(|p| p.reply_to)
                .collect();
            let theirs: Vec<&Plan> = fresh
                .iter()
                .filter(|p| p.pubkey != k.pubkey() || p.instance != c.instance)
                .collect();

            let (mut asked, mut rest): (Vec<&Plan>, Vec<&Plan>) = theirs
                .into_iter()
                .partition(|p| p.kind == "ask" && !answered.contains(&p.id));
            // Directed questions first: they are the ones with a name on them.
            asked.sort_by_key(|p| (!addressed_to_me(p, &c), p.epoch));
            rest.sort_by_key(|p| p.epoch);

            if !asked.is_empty() {
                let lines: Vec<String> = asked
                    .iter()
                    .map(|p| {
                        let mine = if addressed_to_me(p, &c) {
                            "  [FOR YOU] "
                        } else {
                            "  "
                        };
                        format!(
                            "{mine}{} asks: {}\n    (answer it: robofinger answer --to {} --re {} \"…\")",
                            who(p, true),
                            p.task,
                            p.alias,
                            p.id
                        )
                    })
                    .collect();
                blocks.push(format!(
                    "OPEN QUESTIONS from your teammates — these are waiting on somebody:\n{}",
                    lines.join("\n")
                ));
            }

            if !claims.is_empty() {
                blocks.push(format!(
                    "Peer agents are holding these paths right now:\n{}",
                    claims.join("\n")
                ));
            }

            if !rest.is_empty() {
                // The newest START_SHOWN, still in chronological order.
                let shown: Vec<&&Plan> = rest.iter().rev().take(START_SHOWN).rev().collect();
                let more = rest.len().saturating_sub(shown.len());
                // An answer with no sight of the question is unreadable, and
                // once the cursor has passed the question it is no longer in
                // `fresh`. Fall back to a cursor-free lookup — one extra fetch,
                // and only when an answer is actually on screen.
                let needs_context = shown.iter().any(|p| p.kind == "answer" && p.reply_to > 0);
                let backfill: Vec<Plan> = if needs_context {
                    fetch_posts(&c, &k, MAX_POST_LIMIT)
                } else {
                    vec![]
                };
                let asked_text = |id: i64| {
                    fresh
                        .iter()
                        .chain(backfill.iter())
                        .find(|q| q.id == id)
                        .map(|q| truncate(&q.task, 120))
                };
                let mut body = shown
                    .iter()
                    .map(|p| {
                        let base = render_entry(p, true);
                        match (p.kind.as_str(), p.reply_to) {
                            ("answer", r) if r > 0 => match asked_text(r) {
                                Some(q) => format!("{base}\n    (you asked: {q})"),
                                None => base,
                            },
                            _ => base,
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if more > 0 {
                    body.push_str(&format!(
                        "\n  … {more} earlier — see them with `robofinger log`"
                    ));
                }
                blocks.push(format!(
                    "What your teammates did since you last looked:\n{body}"
                ));
            }

            // Your own claim, carried over from a session that ended without
            // releasing. The deadman switch frees it eventually, but "eventually"
            // is up to an hour of teammates treating a dead session as live —
            // and this agent is the only one that can say what actually
            // happened to the files.
            if let Some(m) = live.iter().find(|p| {
                is_self(p, &k.pubkey(), &c.instance)
                    && p.live(t)
                    && !p.touching.is_empty()
                    // Only a claim that predates this session. A `start` firing
                    // right after a claim — a resumed session, a hook that runs
                    // twice — would otherwise nag about work in progress, and a
                    // warning that cries wolf gets ignored exactly when it is
                    // real.
                    && t.saturating_sub(p.epoch) > STALE_GRACE
            }) {
                blocks.insert(
                    0,
                    format!(
                        "YOU are still holding these, from a session that ended without releasing:\n  \
                         {}  ({}) — claimed {}\n  \
                         Release it with what happened (`robofinger release --note \"…\"`), or \
                         re-claim it if you are still on it.",
                        m.touching.join(" "),
                        m.task,
                        ago(t.saturating_sub(if m.claimed_at > 0 { m.claimed_at } else { m.epoch })),
                    ),
                );
            }

            if !blocks.is_empty() {
                blocks.push(GUIDANCE.into());
                println!(
                    "{}",
                    serde_json::json!({
                        "hookSpecificOutput": {
                            "hookEventName": "SessionStart",
                            "additionalContext": blocks.join("\n\n")
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
            let group = flag(&args, "--group").or_else(|| flag(&args, "-g"));
            let to = flag(&args, "--to");
            // Prefer args; fall back to stdin so `... | robofinger post` works
            // and prose isn't trapped behind shell quoting.
            let text = if args.len() > 1 {
                message(&args, &["--group", "-g", "--to"])
            } else {
                let mut buf = String::new();
                let _ = std::io::stdin().read_to_string(&mut buf);
                buf.trim_end().to_string()
            };
            if text.is_empty() {
                eprintln!("nothing to post (pass text, or pipe it on stdin)");
                std::process::exit(1);
            }
            if let Some(t) = &to {
                warn_unknown_peer(t, &c, &k);
            }
            if let Some(g) = &group {
                let known: Vec<String> = crypto::load_peers()
                    .iter()
                    .flat_map(|p| p.groups.clone())
                    .collect();
                if !known.iter().any(|x| x == g) {
                    eprintln!("no peer is in group {g:?} — this would go to nobody but you.");
                    eprintln!("  tag someone first: robofinger add <address> --group {g}");
                    eprintln!("  or see who is where: robofinger list");
                    std::process::exit(1);
                }
            }
            match post(
                &c,
                &k,
                &text,
                group.as_deref(),
                "",
                vec![],
                to.as_deref().unwrap_or(""),
                0,
            ) {
                Ok(_) => println!("posted ({} chars)", text.chars().count()),
                Err(e) => {
                    eprintln!("post failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        // Raise something the team should settle: two agents wanting the same
        // path, a claim that looks abandoned, a call that needs a human. It is
        // a distinct kind rather than a note so that peers can surface it at
        // session start instead of hoping somebody reads the feed.
        "ask" | "answer" => {
            let to = flag(&args, "--to");
            let reply_to: i64 = flag(&args, "--re")
                .map(|v| {
                    v.parse().unwrap_or_else(|_| {
                        eprintln!("--re wants an entry id, as shown by `robofinger since --ids`");
                        std::process::exit(2);
                    })
                })
                .unwrap_or(0);
            let text = if args.len() > 1 {
                message(&args, &["--to", "--re"])
            } else {
                let mut buf = String::new();
                let _ = std::io::stdin().read_to_string(&mut buf);
                buf.trim_end().to_string()
            };
            if text.is_empty() {
                eprintln!("usage: robofinger {cmd} [--to <peer>] \"<text>\"");
                eprintln!("  say what you need decided, and what the options are");
                std::process::exit(2);
            }
            if cmd == "answer" && reply_to == 0 && to.is_none() {
                eprintln!("an answer needs a recipient: robofinger answer --to <peer> \"<text>\"");
                eprintln!("  or point at the question: --re <id>");
                std::process::exit(2);
            }
            if let Some(t) = &to {
                warn_unknown_peer(t, &c, &k);
            }
            // Never grouped, for the same reason claims are not: a question
            // some of the team cannot decrypt is one nobody answers.
            match post(
                &c,
                &k,
                &text,
                None,
                cmd,
                vec![],
                to.as_deref().unwrap_or(""),
                reply_to,
            ) {
                Ok(_) => {
                    let who = to.as_deref().unwrap_or("the team");
                    let verb = if cmd == "ask" { "asked" } else { "answered" };
                    println!("{verb} {who} ({} chars)", text.chars().count());
                    println!("  they see it at their next session start, or on `robofinger since`");
                }
                Err(e) => {
                    eprintln!("{cmd} failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        "log" => {
            let limit = flag(&args, "-n")
                .and_then(|s| s.parse().ok())
                .unwrap_or(20usize);
            let peer = flag(&args, "--peer");
            let ids = args.iter().any(|a| a == "--ids");
            // An explicit window. A read, not a consumption — it never moves
            // the watermark, so `log --since` and `since` do not interfere.
            let cutoff = flag(&args, "--since")
                .map(|s| parse_since(&s, now()))
                .transpose()
                .unwrap_or_else(|e| {
                    eprintln!("{e}");
                    std::process::exit(2);
                });
            let subs = crypto::load_peers();
            let mut any = false;
            for p in fetch_posts(&c, &k, limit) {
                if cutoff.is_some_and(|t| p.epoch < t) {
                    continue;
                }
                if !peer_matches(&p, peer.as_deref(), &subs) {
                    continue;
                }
                any = true;
                if !show_entry(&p, true, ids) {
                    return;
                }
            }
            if !any {
                println!("nothing on the timeline yet");
            }
        }
        // What has happened since this agent last looked. The question a robot
        // asks at the start of a session, and the reason the timeline exists —
        // git answers it eventually, this answers it now.
        "since" => {
            let limit = flag(&args, "-n")
                .and_then(|s| s.parse().ok())
                .unwrap_or(MAX_POST_LIMIT);
            let peer = flag(&args, "--peer");
            let ids = args.iter().any(|a| a == "--ids");
            let dir = crypto::config_dir();
            let mut seen = read_seen(&dir);
            let subs = crypto::load_peers();

            let fresh = fetch_posts_after(&c, &k, limit, &seen);
            // Oldest first: this reads as a narrative of what happened while
            // you were away, unlike `log`, which is a feed you scan.
            let mut shown: Vec<&Plan> = fresh
                .iter()
                .filter(|p| peer_matches(p, peer.as_deref(), &subs))
                .collect();
            shown.sort_by_key(|p| p.epoch);
            for p in &shown {
                if !show_entry(p, true, ids) {
                    break;
                }
            }
            if shown.is_empty() {
                println!("nothing new");
            }

            // A `--peer` run is a filtered read, not a consumption — the same
            // rule as `log --since`. Advancing here would silently swallow
            // everyone else's entries, which the next bare `since` would then
            // never show.
            if peer.is_some() {
                return;
            }

            // Advance past everything fetched. Per relay, and only forward:
            // ids are assigned per relay, so `endpoints` is the authority on
            // which one a given row came from, and taking the max means a
            // duplicated or out-of-order fetch cannot rewind the cursor.
            for (url, keys) in endpoints(&c, &k, &subs) {
                let high = fresh
                    .iter()
                    .filter(|p| keys.contains(&p.pubkey))
                    .map(|p| p.id)
                    .max()
                    .unwrap_or(0);
                let e = seen.entry(url).or_insert(0);
                *e = (*e).max(high);
            }
            write_seen(&dir, &seen);
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
                    println!("peers see it in `robofinger list`; it expires in a year.");
                }
                Err(e) => {
                    eprintln!("failed: {e}");
                    std::process::exit(1);
                }
            }
        }
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

/// Ask for a different label when a suggested one is already taken.
///
/// Returns None on a non-TTY or an empty answer, so a scripted `add` still
/// fails loudly rather than silently picking a name for you.
fn prompt_for_label(suggested: &str, peers: &[Peer]) -> Option<String> {
    use std::io::{BufRead, IsTerminal, Write};
    // Ask on the controlling terminal, not stdin — `add` may well be run with
    // stdin redirected, and the question must still reach a human.
    if !std::io::stderr().is_terminal() {
        return None;
    }
    let Ok(tty) = std::fs::File::options()
        .read(true)
        .write(true)
        .open("/dev/tty")
    else {
        return None;
    };
    let mut tty = std::io::BufReader::new(tty);
    loop {
        eprint!("What should this one be called instead? (blank to cancel) ");
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        if tty.read_line(&mut line).is_err() {
            return None;
        }
        let name = line.trim();
        if name.is_empty() {
            return None;
        }
        if name == suggested || peers.iter().any(|x| x.label == name) {
            eprintln!("  {name:?} is taken too.");
            continue;
        }
        return Some(name.to_string());
    }
}

/// Ask one question on the controlling terminal.
///
/// Reads from /dev/tty rather than stdin so a redirected stdin — a setup
/// script, `ssh host "robofinger init"` — does not silently skip the prompt.
/// Returns None when there is no terminal, so non-interactive callers fall
/// through to the usage error instead of hanging.
fn ask(question: &str, default: Option<&str>) -> Option<String> {
    use std::io::{BufRead, IsTerminal, Write};
    if !std::io::stderr().is_terminal() {
        return None;
    }
    let tty = std::fs::File::options()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .ok()?;
    let mut tty = std::io::BufReader::new(tty);
    match default {
        Some(d) => eprint!("{question} [{d}]: "),
        None => eprint!("{question}: "),
    }
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    tty.read_line(&mut line).ok()?;
    let answer = line.trim();
    if answer.is_empty() {
        default.map(str::to_string)
    } else {
        Some(answer.to_string())
    }
}

/// Who published a plan: "mymac", or "mymac/claude-2" when the instance needs
/// showing.
///
/// `show_instance` is false when this key has only one live instance — with
/// auto-differentiation every plan now carries an instance, and appending it
/// when you are working alone is noise for what is still the common case.
/// Solo output stays byte-identical to pre-0.2.
fn who(p: &Plan, show_instance: bool) -> String {
    if p.instance.is_empty() || !show_instance {
        p.alias.clone()
    } else {
        format!("{}/{}", p.alias, p.instance)
    }
}

/// Render one plan the way a post renders: stamp, who, then the detail.
///
/// `who` carries the instance when there is one, so two agents on a single
/// identity are told apart in the one place that matters — the line you read
/// when deciding whether to touch a file.
fn show_plan(p: &Plan, t: i64, suffix: &str, current: bool, show_instance: bool) {
    let who = who(p, show_instance);
    println!("\n{} {}{}", stamp(p.epoch), who, suffix);

    if !p.live(t) {
        if p.status == "done" {
            println!("finished {}", ago(t - p.epoch));
        } else if !p.touching.is_empty() {
            println!(
                "{} expired {} — session ended without releasing",
                p.touching.join(", "),
                ago(t.saturating_sub(p.epoch + p.eta_s * STALE_MULT))
            );
        } else {
            println!("not working on anything right now");
        }
        return;
    }

    println!("working: {}", p.task);
    if p.touching.is_empty() {
        println!("  holding nothing — released {}", ago(t - p.epoch));
        return;
    }
    for g in &p.touching {
        println!("  claiming {}/{}", p.project, g);
    }
    // claimed_at survives a republish; epoch does not. The gap between them
    // is how long the agent has been quiet while still holding the files,
    // which is the thing you actually want to know before waiting on it.
    let held = if p.claimed_at > 0 {
        p.claimed_at
    } else {
        p.epoch
    };
    let idle = t - p.epoch;
    // Only the live row can be idle. A superseded claim was not abandoned —
    // it was replaced — so its "idle" would just be the age of the row.
    if current && idle >= 120 {
        // ago() carries its own "ago", so the idle figure is a bare duration.
        println!("  claimed {} (idle {})", ago(t - held), dur(idle));
    } else {
        println!("  claimed {}", ago(t - held));
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

    println!("{} @ {}", c.alias, c.url);
    println!("{}", k.pubkey());

    // Group by agent. One instance is the ordinary case and renders exactly
    // as before; several means two Claudes sharing this identity, and each
    // gets its own block.
    let mut by_instance: std::collections::HashMap<String, Vec<Plan>> =
        std::collections::HashMap::new();
    for p in &mine {
        by_instance
            .entry(p.instance.clone())
            .or_default()
            .push(p.clone());
    }
    let mut names: Vec<String> = by_instance.keys().cloned().collect();
    names.sort();
    // Only worth naming instances when there is more than one to tell apart.
    let multi = names.len() > 1;

    if mine.is_empty() {
        println!("\nno active claim");
    }
    for name in names {
        let mut rows = by_instance.remove(&name).unwrap_or_default();
        rows.sort_by_key(|p| std::cmp::Reverse(p.seq));
        for (i, p) in rows.iter().take(CLAIM_HISTORY).enumerate() {
            let suffix = match (i, name == c.instance) {
                (0, true) if !name.is_empty() => "  (this one)",
                (0, _) => "",
                _ => "  (previous)",
            };
            show_plan(p, t, suffix, i == 0, multi);
        }
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
    // "with active claims" has to mean holding something. A peer that
    // released is live but holds nothing, and counting it made the summary
    // contradict the list right above it.
    let live = current_plans(c, k)
        .iter()
        .filter(|p| p.pubkey != k.pubkey() && p.live(t) && !p.touching.is_empty())
        .count();
    // With no peers, "look someone up" is advertising something that cannot
    // work yet. Point at the exchange instead — that is the actual next step.
    if peers.is_empty() {
        println!("\nNo peers yet. To follow someone, swap addresses:");
        println!("  1. send them yours:   robofinger id");
        println!("  2. add theirs:        robofinger add <their address>");
        println!("  3. they add yours too — both directions are required");
        println!("\nrobofinger --help   all commands");
    } else {
        let example = peers[0].label.as_str();
        let w = example.len().max(6); // widest of the example label and "--help"
        println!("\n{} peer(s), {live} with active claims", peers.len());
        println!("\nrobofinger {example:<w$}   read their plan");
        println!("robofinger {:<w$}   everyone, newest first", "log");
        println!("robofinger {:<w$}   all commands", "--help");
    }
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
                    "no peer named {who:?} and no such command\n  robofinger --help    all commands\n  robofinger list       who you follow"
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

    let theirs: Vec<Plan> = fetch_plans(c, k)
        .into_iter()
        .filter(|p| p.pubkey == peer.pubkey)
        .collect();

    if !theirs.is_empty() {
        let mut by_instance: std::collections::HashMap<String, Vec<Plan>> =
            std::collections::HashMap::new();
        for p in &theirs {
            by_instance
                .entry(p.instance.clone())
                .or_default()
                .push(p.clone());
        }
        let mut names: Vec<String> = by_instance.keys().cloned().collect();
        names.sort();
        let multi = names.len() > 1;
        for name in names {
            let mut rows = by_instance.remove(&name).unwrap_or_default();
            rows.sort_by_key(|p| std::cmp::Reverse(p.seq));
            for (i, p) in rows.iter().take(CLAIM_HISTORY).enumerate() {
                show_plan(
                    p,
                    t,
                    if i == 0 { "" } else { "  (previous)" },
                    i == 0,
                    multi,
                );
            }
        }
    }
    match theirs.first() {
        // Already rendered above, with its history.
        Some(_) => {}
        None if has_unreadable(c, k, &peer.pubkey, "plans") => {
            // Not "not to you" — publishing goes to a recipient list, not to
            // individuals, so this is a missing key exchange rather than an
            // exclusion.
            println!("\nthey have published, but you are not in their peer list yet.");
            println!("  send them your address (robofinger id) and ask them to run:");
            println!("  robofinger add <it>");
        }
        None => println!("\nnot working on anything right now"),
    }

    let posts: Vec<Plan> = fetch_posts(c, k, 20)
        .into_iter()
        .filter(|p| p.pubkey == peer.pubkey)
        .collect();
    if posts.is_empty() {
        if has_unreadable(c, k, &peer.pubkey, "posts") {
            // Two causes, same symptom: never added, or added after these
            // were written. Name both rather than guess.
            println!("\nthey have posts you cannot read — either they have not added");
            println!("  you yet, or these predate them adding you. Anything written");
            println!("  after they add you will be readable.");
        } else {
            println!("\nno posts yet");
        }
    } else {
        println!();
        for p in posts {
            println!("{} {}", stamp(p.epoch), p.alias);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(agent: &str, project: &str, touching: &[&str], status: &str, age: i64) -> Plan {
        Plan {
            alias: agent.into(),
            pubkey: format!("pk-{agent}"),
            seq: 1,
            epoch: now() - age,
            status: status.into(),
            task: "t".into(),
            touching: touching.iter().map(|s| s.to_string()).collect(),
            project: project.into(),
            eta_s: 1800,
            instance: String::new(),
            claimed_at: 0,
            kind: String::new(),
            globs: vec![],
            to: String::new(),
            reply_to: 0,
            id: 0,
        }
    }

    /// Same shape as `conflicts`, minus the network.
    ///
    /// Calls the real `is_self` rather than restating it: the previous version
    /// of this helper open-coded the identity check and omitted `instance`
    /// entirely, so it would have passed just as happily with the instance
    /// half deleted from production.
    fn matches(p: &Plan, rel: &str, here: &str, me: &str) -> bool {
        matches_as(p, rel, here, me, "")
    }

    /// `matches`, but for a caller running as a named agent on key `me`.
    fn matches_as(p: &Plan, rel: &str, here: &str, me: &str, instance: &str) -> bool {
        if is_self(p, &format!("pk-{me}"), instance) || p.project != here || !p.live(now()) {
            return false;
        }
        p.touching.iter().any(|g| {
            glob::Pattern::new(g)
                .map(|pat| pat.matches(rel))
                .unwrap_or(false)
        })
    }

    #[test]
    fn ago_gains_a_date_past_a_day() {
        assert_eq!(ago(30), "30s ago");
        assert_eq!(ago(120), "2m ago");
        assert_eq!(ago(7200), "2h ago");
        // past a day: relative form kept, absolute date appended
        let d = ago(200_000);
        assert!(d.starts_with("2d ago ("), "{d}");
        assert!(d.ends_with(')'), "{d}");
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
    fn session_start_hides_other_projects() {
        let t = now();
        let mine = plan("peer", "demo", &["src/**"], "working", 0);
        let other = plan("peer", "Cura-2026", &["lib/**"], "working", 0);
        assert!(worth_showing(&mine, "pk-me", "demo", t), "same project shows");
        assert!(
            !worth_showing(&other, "pk-me", "demo", t),
            "another project is not this agent's business"
        );
        assert!(
            !worth_showing(&mine, "pk-peer", "demo", t),
            "your own claim is not news"
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

    /// The tab case. Two Claudes share one identity, so the pubkey matches and
    /// only `instance` tells them apart. Without this the feature is inert and
    /// nothing catches it — the failure is silence, not a wrong warning.
    #[test]
    fn same_key_different_agent_is_a_conflict() {
        let mut p = plan("me", "demo", &["src/auth/**"], "working", 0);
        p.instance = "claude-1".into();
        assert!(
            matches_as(&p, "src/auth/session.ts", "demo", "me", "claude-2"),
            "claude-2 must see claude-1's claim on the same key"
        );
        assert!(
            !matches_as(&p, "src/auth/session.ts", "demo", "me", "claude-1"),
            "an agent must not conflict with itself"
        );
    }

    /// A pre-0.2 peer publishes with no instance. It must still conflict with
    /// an instanced agent, or upgrading one machine silently stops warnings.
    #[test]
    fn empty_instance_still_conflicts_with_a_named_one() {
        let p = plan("me", "demo", &["src/auth/**"], "working", 0);
        assert!(matches_as(
            &p,
            "src/auth/session.ts",
            "demo",
            "me",
            "claude-1"
        ));
    }

    /// `list` labels agents only when two are really holding paths. A sibling
    /// that released still publishes a live row holding nothing, and counting
    /// rows instead of claims tagged a peer that only ever had one.
    #[test]
    fn list_labels_agents_only_when_several_hold_paths() {
        let holding = plan("peer", "demo", &["src/a/**"], "working", 0);
        let released = plan("peer", "demo", &[], "working", 0);

        assert!(
            !names_worth_showing(std::slice::from_ref(&holding)),
            "one claim needs no label"
        );
        assert!(
            !names_worth_showing(&[holding.clone(), released]),
            "a released sibling must not trip the label"
        );
        assert!(
            names_worth_showing(&[holding.clone(), holding.clone()]),
            "two real claims are worth telling apart"
        );
    }

    #[test]
    fn who_hides_the_instance_when_alone() {
        let mut p = plan("peer", "demo", &[], "working", 0);
        p.alias = "mymac".into();
        p.instance = "claude-2".into();
        assert_eq!(who(&p, true), "mymac/claude-2");
        assert_eq!(who(&p, false), "mymac", "solo output stays as it was");
        p.instance = String::new();
        assert_eq!(who(&p, true), "mymac", "nothing to append");
    }

    /// Slots must be stable per session (the hook process has to land on the
    /// same name as the agent that took the claim) and distinct between them.
    #[test]
    fn slots_are_stable_per_session_and_reused_once_free() {
        let d = std::env::temp_dir().join(format!("rf-slots-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let t = 1_000_000;

        let a = slot_in(&d, "session-a", t);
        assert_eq!(
            slot_in(&d, "session-a", t),
            a,
            "same session keeps its name"
        );
        let b = slot_in(&d, "session-b", t);
        assert_ne!(a, b, "different sessions differ");
        assert_eq!((a.as_str(), b.as_str()), ("claude-1", "claude-2"));

        // An expired reservation frees its number for the next new session.
        std::fs::write(d.join("instances"), format!("session-a\tclaude-1\t{t}\n")).unwrap();
        assert_eq!(
            slot_in(&d, "session-c", t + SLOT_TTL + 1),
            "claude-1",
            "stale slot is reclaimed"
        );

        std::fs::remove_dir_all(&d).ok();
    }

    /// Byte slicing at an arbitrary offset panics on any multi-byte character,
    /// and a task description is prose — so this is the case that matters.
    #[test]
    fn entry_text_truncates_on_a_character_boundary() {
        assert_eq!(
            truncate("short", MAX_ENTRY),
            "short",
            "under the cap is left alone"
        );
        assert_eq!(truncate("abcdef", 3), "abc…");

        let wide = "\u{e9}".repeat(MAX_ENTRY + 50);
        let got = truncate(&wide, MAX_ENTRY);
        assert_eq!(
            got.chars().count(),
            MAX_ENTRY + 1,
            "cap counts chars, not bytes"
        );
        assert!(got.ends_with('\u{2026}'));

        // Exactly at the cap is not truncated: `nth(max)` finding nothing is
        // the whole string, and an ellipsis there would be a lie.
        let exact = "x".repeat(MAX_ENTRY);
        assert_eq!(truncate(&exact, MAX_ENTRY), exact);
    }

    /// A release with no `--note` should still say what happened, because an
    /// agent that has to compose one often will not — and an empty record
    /// makes every other feature worthless.
    ///
    /// Drives real git rather than a mock: the whole risk here is that the
    /// pathspec or the `--since` boundary is wrong, and a mock would assert
    /// the bug as readily as the fix.
    #[test]
    fn a_release_note_falls_back_to_the_commits_under_the_claim() {
        let dir = std::env::temp_dir().join(format!("rf-commits-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("docs")).unwrap();

        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .current_dir(&dir)
                .args(args)
                .output()
                .expect("git");
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "T"]);

        std::fs::write(dir.join("src/a.rs"), "1").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "fix the retry backoff"]);
        std::fs::write(dir.join("docs/x.md"), "1").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "unrelated doc edit"]);

        // `commits_since` reads `repo_root()`, which resolves from the process
        // cwd — so run it from the fixture.
        let here = std::env::current_dir().unwrap();
        std::env::set_current_dir(&dir).unwrap();
        let scoped = commits_since(0, &["src/**".to_string()]);
        let all = commits_since(0, &[]);
        // A claim released before any commit lands has nothing to say, and
        // must not borrow an older commit as its note.
        let future = commits_since(now() + 3600, &["src/**".to_string()]);
        std::env::set_current_dir(here).unwrap();

        assert!(scoped.contains("retry backoff"), "got {scoped:?}");
        assert!(
            !scoped.contains("unrelated"),
            "globs must scope the log to the claimed paths, got {scoped:?}"
        );
        assert!(all.contains("unrelated"), "no globs means the whole claim");
        assert!(
            all.find("retry").unwrap() < all.find("unrelated").unwrap(),
            "oldest first, so truncation keeps the work that explains the rest"
        );
        assert_eq!(future, "", "nothing committed since the claim");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The rate-limit gate. A working agent republishes its claim on every
    /// edit; if each one wrote a timeline entry the write budget would be gone
    /// in under a minute and the relay would start refusing real claims.
    #[test]
    fn only_a_changed_claim_is_worth_an_entry() {
        let held = plan("me", "repo", &["src/**"], "working", 0);
        let same: Vec<String> = vec!["src/**".into()];
        let other: Vec<String> = vec!["docs/**".into()];

        // Mirrors the `changed` expression in the claim arm.
        let changed =
            |prev: Option<&Plan>, want: &Vec<String>| prev.map(|p| &p.touching) != Some(want);

        assert!(
            !changed(Some(&held), &same),
            "republishing the same claim is not an event"
        );
        assert!(changed(Some(&held), &other), "claiming different paths is");
        assert!(changed(None, &same), "a first claim is");
        assert!(changed(Some(&held), &vec![]), "dropping to nothing is");
    }

    #[test]
    fn watermark_round_trips_and_only_moves_forward() {
        let d = std::env::temp_dir().join(format!("rf-seen-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();

        assert!(read_seen(&d).is_empty(), "no file means seen nothing");

        let mut seen = std::collections::HashMap::new();
        seen.insert("https://a.example/plan".to_string(), 42i64);
        // A URL with a namespace path, since that is the interesting case.
        seen.insert("https://b.example/plan/team".to_string(), 7i64);
        write_seen(&d, &seen);
        assert_eq!(read_seen(&d), seen, "round trip");

        // Advancing takes the max, so an out-of-order or duplicated fetch
        // cannot rewind the cursor and replay entries forever.
        let mut back = read_seen(&d);
        let e = back.entry("https://a.example/plan".into()).or_insert(0);
        *e = (*e).max(9);
        assert_eq!(
            back["https://a.example/plan"], 42,
            "a lower id does not rewind"
        );

        std::fs::remove_dir_all(&d).ok();
    }

    /// Addressing is what makes a question reach one agent rather than the
    /// wall. A missed match means an agent never learns something was for it.
    #[test]
    fn a_question_finds_the_agent_it_names() {
        let cfg = |alias: &str, instance: &str| Cfg {
            url: "https://relay.example/plan".into(),
            alias: alias.into(),
            instance: instance.into(),
        };
        let to = |t: &str| {
            let mut p = plan("robot1", "repo", &[], "post", 0);
            p.kind = "ask".into();
            p.to = t.into();
            p
        };

        let me = cfg("robot2", "claude-1");
        assert!(addressed_to_me(&to("robot2"), &me), "my alias");
        assert!(addressed_to_me(&to("ROBOT2"), &me), "case-insensitive");
        assert!(addressed_to_me(&to(" robot2 "), &me), "whitespace trimmed");
        assert!(addressed_to_me(&to("claude-1"), &me), "my instance");
        assert!(
            addressed_to_me(&to("robot2/claude-1"), &me),
            "alias/instance"
        );
        assert!(!addressed_to_me(&to("robot1"), &me), "somebody else");
        assert!(!addressed_to_me(&to(""), &me), "undirected is not for me");

        // A solo agent has no instance, so the instance arms must not match
        // everything by comparing against an empty string.
        let solo = cfg("robot2", "");
        assert!(addressed_to_me(&to("robot2"), &solo));
        assert!(!addressed_to_me(&to(""), &solo));
        assert!(!addressed_to_me(&to("robot2/"), &solo));
    }

    /// The flag-stripping this replaced broke as soon as the message repeated
    /// the flag's own text — the shell has already eaten the quotes by then.
    #[test]
    fn message_keeps_words_that_look_like_flags() {
        let args: Vec<String> = ["ask", "--to", "bob", "should", "--to", "mean", "bob?"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            message(&args, &["--to", "--re"]),
            "should --to mean bob?",
            "only the FIRST flag pair is consumed; a later one is message text \
             and survives verbatim, along with the word after it"
        );

        let plain: Vec<String> = ["ask", "who", "takes", "auth"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(message(&plain, &["--to"]), "who takes auth");
    }

    /// A relay that accepts a connection and never answers used to hang the
    /// client forever — and `check`/`start` run inside a coding agent's
    /// session, so that hung the agent. The bound must survive both a new call
    /// site (everything goes through `agent()`) and someone dropping the
    /// per-command budget.
    #[test]
    fn every_request_is_bounded_and_hooks_are_bounded_tighter() {
        // Read through variables: comparing the consts directly folds to a
        // constant and clippy rightly says the assertion proves nothing.
        // The bound being wired to real requests is covered by CI, which runs
        // the hooks against a socket that accepts and never answers.
        let (hook, cmd) = (HOOK_TIMEOUT, NET_TIMEOUT);
        assert!(cmd > 0, "an unbounded request hangs the session");
        assert!(
            hook < cmd,
            "hooks run inside an agent's session and must give up sooner \
             than a command a person is watching"
        );
        // Several sequential requests fit in one command, so the wall-clock
        // worst case is a multiple of this. Keep the multiple tolerable.
        assert!(hook <= 3, "hook budget multiplies across relays");

        // The default applies to anything that has not opted in.
        assert_eq!(net_timeout(), NET_TIMEOUT);
        NET_BUDGET.store(HOOK_TIMEOUT, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(net_timeout(), HOOK_TIMEOUT);
        NET_BUDGET.store(NET_TIMEOUT, std::sync::atomic::Ordering::Relaxed);
    }

    #[test]
    fn since_accepts_durations_and_epochs() {
        let t = 1_000_000i64;
        assert_eq!(parse_since("2h", t).unwrap(), t - 7200);
        assert_eq!(parse_since("30m", t).unwrap(), t - 1800);
        assert_eq!(parse_since("3d", t).unwrap(), t - 259_200);
        assert_eq!(parse_since("90s", t).unwrap(), t - 90);
        assert_eq!(
            parse_since("12345", t).unwrap(),
            12345,
            "bare number is an epoch"
        );
        assert!(parse_since("yesterday", t).is_err());
        assert!(parse_since("2x", t).is_err());
    }

    #[test]
    fn plans_with_missing_fields_still_parse() {
        let p: Plan = serde_json::from_str(r#"{"agent":"a"}"#).unwrap();
        assert_eq!(p.eta_s, DEFAULT_ETA);
        assert!(p.touching.is_empty());
    }
}
