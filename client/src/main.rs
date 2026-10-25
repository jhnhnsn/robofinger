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

mod activity;
mod crypto;
mod dash;
mod hooks;
mod selfupdate;
mod tracker;

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
const MAX_ENTRY: usize = 140;
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
/// Default port for `serve`. Arbitrary, just unlikely to be taken.
const SERVE_PORT: u16 = 7380;

/// What an agent should do with any of this. Appended to the SessionStart
/// block, because the hook is the one place every agent reliably reads —
/// CLAUDE_MD is advisory and a fresh session may not have it.
///
/// Rules only. The escalation rule is deliberately concrete, because "use your
/// judgment" produces agents that either never ask or ask constantly, and which
/// one you get varies by model and by session — that is the one behaviour the
/// design says does not emerge on its own, so it is worth spending context on.
///
/// Everything softer than a rule — how to phrase a question, whether to
/// acknowledge, what order to do things in — lives in ROBOFINGER.md instead.
/// Agents given a channel work out their own etiquette; what they cannot infer
/// is when a human has to be involved. Prescribing style here also spends the
/// context window the agent still needs for its actual work, every session.
const GUIDANCE: &str = "\
  - A question marked [FOR YOU] is addressed to this agent. Answer it with
    `robofinger answer --to <peer> --re <id> \"…\"`.
  - Raise your own with `robofinger ask [--to <peer>] \"…\"`.
  - Ask a HUMAN, rather than deciding alone, when: two agents want the same
    path and neither has yielded; a peer's claim is well past its ETA and you
    cannot tell if it died; a peer asks you something whose answer changes work
    outside your task; or answering would mean undoing a teammate's work.
    Say what you would do by default and what the alternatives cost.
  - Everything else is context. Read it and carry on.
  - The full workflow, including anything this team added, is in
    ROBOFINGER.md at the repo root.";

/// The one rule every session gets, quiet or not: a solo agent that never
/// claims publishes nothing, and the first teammate to arrive is warned of
/// nothing.
const CLAIM_RULE: &str = "\
  - Before editing files, claim them: `robofinger claim \"<task>\" '<glob>' …`
    (a claim replaces your previous one). `robofinger release` when done.";

/// What a subagent gets on top of the claim rule. It works inside its
/// parent's claim, so without this it reads that claim as covering it — and
/// its siblings, who are not covered by it, never see what it touches. It
/// talks to nobody but its parent (ADR 0005), so it is told that too.
const SUBAGENT_RULE: &str = "\
  - You are a subagent: your claims are your own, filed under your parent's
    name, and released for you when you finish. Claim what you will edit even
    inside your parent's claim — it is how your sibling subagents see you.\n\
  - You report to your parent only. Do not ask teammates or people, and do not
    post. If a claim blocks you or a decision is beyond your task, stop and say
    so in what you return to your parent.";

/// What an agent is told about the subagents it may start: it is their only
/// line to the team (ADR 0005).
const PARENT_RULE: &str = "\
  - If you start subagents, you direct them: pass on what they need from this,
    and handle what they report back. They do not hear from the team directly.";

/// What SessionStart hands the agent. With nothing to act on, the rules for
/// acting on it are just context spent, so only the claim rule goes in.
fn start_context(mut blocks: Vec<String>, me: &str) -> String {
    let rest = if blocks.is_empty() {
        "  - The full workflow is in ROBOFINGER.md at the repo root."
    } else {
        GUIDANCE
    };
    blocks.push(format!(
        "How to use this:\n{}\n{CLAIM_RULE}\n{PARENT_RULE}\n{rest}",
        you_are(me, None)
    ));
    blocks.join("\n\n")
}

/// The line that tells an agent its own agent path. Nothing else does: the
/// name is worked out afresh in every hook, and an agent that cannot say who
/// it is cannot tell anyone where to find it.
fn you_are(me: &str, parent: Option<&str>) -> String {
    let page = dashboard_base()
        .map(|b| format!(" Its page: {b}/{me}"))
        .unwrap_or_default();
    let of = parent
        .map(|p| format!(", a subagent of `{p}`"))
        .unwrap_or_default();
    format!(
        "- You are `{me}`{of}: your agent path, the name your claims and entries are filed under.{page}"
    )
}

/// Where this team's dashboard is served, if it says. A deployment's address,
/// not an agent's: the same path works on any dashboard that can read it.
fn dashboard_base() -> Option<String> {
    std::env::var("ROBOFINGER_DASHBOARD")
        .ok()
        .or_else(|| config_file().get("ROBOFINGER_DASHBOARD").cloned())
        .map(|s| s.trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
}

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

  activity                      what each agent on this machine is doing now
      (read from the hooks' local spool; nothing is sent anywhere)

  log [-n N] [--peer <label>] [--since <when>] [--ids] [--url]
                                the whole timeline, newest first
      robofinger log
      robofinger log --since 2h        a window; does not move your cursor
      robofinger log --peer sam
      robofinger log --ids             show entry ids, for `answer --re`
      robofinger log --url             expand commit shas into links
      (github, gitlab and bitbucket are derived from `origin`; any other
       forge needs ROBOFINGER_COMMIT_URL with a {sha} placeholder)

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

  serve [--port N]              the same record as a live page in your browser
      robofinger serve          then open http://127.0.0.1:7380
      (the plan comes from GitHub via `gh`; work items link to the tracker —
       github, gitlab and bitbucket from `origin`, anything else via
       ROBOFINGER_TICKET_URL, e.g. https://acme.atlassian.net/browse/{key})
      …/hazel-hare/claude-1     one agent and the subagents under it
      …/hazel-hare/claude-1?at=2h
                                as it stood two hours ago, stepping event
                                by event (at= also takes an epoch or
                                2026-10-24T18:51)
      (who holds what, who has gone quiet, open questions, what landed.
       localhost only: it decrypts with your keys, so it is yours alone)

  post [--to <peer>] [--group <name>] \"<text>\"
                                leave a note on the timeline
      robofinger post \"blocked: needs the migration merged first\"
      git log --oneline -5 | robofinger post
      robofinger post --group work \"shipping the auth migration\"
      (no --group means everyone you follow; a group encrypts to just those
       peers, so the rest cannot decrypt it at all. 140 characters — say the
       one thing the diff does not, the rest belongs in the commit)

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

  Your agent runs these through the hooks. You rarely type them. A subagent
  is its own agent, named under its parent (claude-1/explore-2): the hooks
  tag its robofinger calls with --subagent and release its claim when it
  finishes (subagent-start, subagent-stop).

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

Config layers, lowest first:
  ~/.config/robofinger/config   you, on this machine (written by init)
  <repo>/.robofinger            the project — commit it to share
  <repo>/.robofinger.local      you, in this repo — gitignore it
  environment                   ROBOFINGER_URL, ROBOFINGER_ALIAS, ...

Keys are not layered: identity is global, so cloning a repo cannot make you
publish as someone else. Keys never leave this machine.";

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Plan {
    /// What a publisher called itself, from v0.8.0 and earlier.
    ///
    /// No longer written. A name in the envelope is asserted by whoever sent
    /// it, so it could name anyone — and every reader now derives a name from
    /// the pubkey it just verified, which nobody can assert. It stays here,
    /// and stays read, so entries published before v0.9.0 still filter by the
    /// name they were filed under.
    ///
    /// Serialised as "agent" for the same reason it is still parsed at all.
    /// `skip_serializing`, not a conditional skip: a plan parsed from an old
    /// entry carries a name, and re-publishing one — a forward, a rebroadcast
    /// — would put an asserted name back on the wire by accident. Read-only,
    /// like `id`.
    #[serde(rename = "agent", default, skip_serializing)]
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
    /// Timeline entries only: short SHAs of the commits this event covered.
    ///
    /// A pointer rather than prose. `task` is capped at `MAX_ENTRY` and a
    /// derived note is the first thing to hit that cap, so the reference that
    /// survives truncation has to live outside the text. The commit message is
    /// already in git; what the record is missing is the way back to it.
    ///
    /// Short SHAs, not URLs: building a forge link means parsing `origin` and
    /// knowing GitHub from GitLab, and a reader that already knows the repo can
    /// construct it. The entry carries the ref, the viewer makes it clickable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    commits: Vec<String>,
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

/// Settings, merged across three layers. Written by `init` at the global
/// layer; the two repo layers are written by hand or committed by a team.
fn config_file() -> std::collections::HashMap<String, String> {
    // Lowest precedence first: each layer overwrites the one before it.
    //
    //   ~/.config/robofinger/config   you, on this machine  (and the keys)
    //   <repo>/.robofinger            the project, committed
    //   <repo>/.robofinger.local      you, in this repo, ignored by git
    //
    // Keys are deliberately not layered — identity is global, so a repo you
    // clone cannot make you publish as someone else.
    let root = std::path::PathBuf::from(repo_root());
    let mut merged = parse_config(&crypto::config_dir().join("config"));
    for f in [".robofinger", ".robofinger.local"] {
        merged.extend(parse_config(&root.join(f)));
    }
    merged
}

/// `key=value` lines, `#` comments, missing file is empty.
fn parse_config(path: &std::path::Path) -> std::collections::HashMap<String, String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

/// Env wins over every config layer, so hooks and CI can override without
/// editing a file — and so a committed `.robofinger` can never silently
/// repoint a relay that was set explicitly for this process.
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
        .or_else(|| session_key().map(|(family, k)| slot_for(family, &k)))
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
///
/// The family comes back with the key because it is what the record calls the
/// agent, and that has to reflect what actually launched it: CLAUDE_CODE_SESSION_ID
/// means Claude Code, while TERM_SESSION_ID is any terminal-launched tool —
/// aider, codex, or a human typing `robofinger claim` — none of which are
/// Claude. Labelling those `claude-2` misreads the record later, when who
/// produced an entry is most of what you want from it.
fn session_key() -> Option<(&'static str, String)> {
    [
        ("claude", "CLAUDE_CODE_SESSION_ID"),
        ("agent", "TERM_SESSION_ID"),
    ]
    .iter()
    .find_map(|(family, k)| {
        std::env::var(k)
            .ok()
            .filter(|s| !s.is_empty())
            .map(|v| (*family, v))
    })
}

/// How long an unused slot reservation is honoured. Past this the name is free
/// again, so a machine does not accumulate claude-1..claude-99 forever.
const SLOT_TTL: i64 = 24 * 3600;

/// Map an opaque session key to a short readable name: claude-1, agent-2.
///
/// Without this the instance would be a raw UUID, which is what `robofinger
/// list` would then print on every line. Reservations are keyed by session so
/// the same session keeps its name across subprocesses and restarts, and they
/// expire by timestamp exactly like claims do — a killed agent's slot ages out
/// rather than needing a liveness check.
fn slot_for(family: &str, key: &str) -> String {
    slot_in(&crypto::config_dir(), family, key, now())
}

/// `slot_for`, with the directory and clock injected so it is testable without
/// mutating process-global env.
fn slot_in(dir: &std::path::Path, family: &str, key: &str, t: i64) -> String {
    let path = dir.join("instances");
    // Held across the read and the write. A swarm starts its subagents in
    // the same instant, and without this each hook rewrote the file from its
    // own read: the last writer won, the others' reservations vanished, and a
    // subagent told it was `explore-3` filed its claims as `explore-1` — or
    // two subagents got one name. Released when `_lock` drops. A home where
    // the lock cannot be taken degrades to the old best effort.
    let _ = std::fs::create_dir_all(dir);
    let _lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("instances.lock"))
        .ok()
        .filter(|f| f.lock().is_ok());
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
            // Numbering is per family, which falls out of the name carrying
            // it: `agent-1` is free on a machine that already holds `claude-1`.
            let name = (1..)
                .map(|i| format!("{family}-{i}"))
                .find(|n| !taken.contains(n.as_str()))
                .unwrap_or_else(|| format!("{family}-1"));
            rows.push((key.to_string(), name.clone(), t));
            name
        }
    };

    // A read-only home degrades to a working (if unstable) name.
    let body: String = rows
        .iter()
        .map(|(k, n, ts)| format!("{k}\t{n}\t{ts}\n"))
        .collect();
    let _ = std::fs::write(&path, body);
    name
}

/// A subagent's instance: its parent's, then its type and a number, so the
/// record reads as a tree — `claude-1/explore-2`.
///
/// The subagent inherits its parent's session, so `session_key` cannot tell
/// them apart; what can is the id the tool hands every hook a subagent fires.
/// That id is the reservation key, so the edit check (which reads it from hook
/// input) and a claim (which gets it from `--subagent`) land on the same name.
/// Numbered per parent, so one session's swarm does not shift another's.
fn subagent_instance(parent: &str, agent_type: &str, agent_id: &str) -> String {
    subagent_instance_in(&crypto::config_dir(), parent, agent_type, agent_id, now())
}

/// `subagent_instance`, with the directory and clock injected.
fn subagent_instance_in(
    dir: &std::path::Path,
    parent: &str,
    agent_type: &str,
    agent_id: &str,
    t: i64,
) -> String {
    let kind = agent_kind(agent_type);
    let family = if parent.is_empty() {
        kind
    } else {
        format!("{parent}/{kind}")
    };
    slot_in(dir, &family, agent_id, t)
}

/// A subagent type reduced to something safe as part of a relay storage key
/// and inside a shell command: lowercase, alphanumerics and single dashes,
/// short. A plugin-scoped type (`my-plugin:reviewer`) keeps only its own name.
fn agent_kind(agent_type: &str) -> String {
    let own = agent_type.rsplit(':').next().unwrap_or("");
    let mut out = String::new();
    for ch in own.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.truncate(20);
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "agent".into()
    } else {
        out
    }
}

/// An id the tool generated, accepted only as a plain token: it is spliced
/// unquoted into the command `tag_robofinger` writes.
fn plain_token(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The subagent a hook fired in, as (kind, id), or None in the main
/// conversation — Claude Code and Codex both send `agent_id` and `agent_type`
/// only from inside a subagent.
fn hook_subagent(input: &serde_json::Value) -> Option<(String, String)> {
    let id = input.get("agent_id")?.as_str().filter(|s| plain_token(s))?;
    let kind = input.get("agent_type").and_then(|v| v.as_str()).unwrap_or("");
    Some((agent_kind(kind), id.to_string()))
}

/// Whether a subagent hook names a type. Claude Code's own helpers (prompt
/// suggestions, /btw) start and stop through the subagent hooks too, with no
/// type of their own, and never claim.
fn typed(input: &serde_json::Value) -> bool {
    input
        .get("agent_type")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty())
}

/// The `--subagent kind:id` value the hook wrote, back into (kind, id).
fn parse_subagent(s: &str) -> Option<(String, String)> {
    let (kind, id) = s.rsplit_once(':')?;
    plain_token(id).then(|| (agent_kind(kind), id.to_string()))
}

/// Tag every robofinger invocation in a shell command with `--subagent`, or
/// None when there is none to tag.
///
/// A subagent's `robofinger claim` is a shell command, and a shell command
/// sees the session's environment but not which subagent ran it — only the
/// hook does. The flag goes after the subcommand because anything in front of
/// `robofinger` stops a `Bash(robofinger:*)` allow rule matching, and the
/// subagent's call is then refused outright.
///
/// Only a `robofinger` in command position is touched — at the start, or after
/// `;`, `&`, `|`, `(`, a backtick or a newline, past any `VAR=value` — and
/// nothing inside quotes, so a path or an argument that contains the word is
/// left alone. A shell parser this is not; a miss costs a claim filed under
/// the parent, which is where it would have gone anyway.
fn tag_robofinger(cmd: &str, tag: &str) -> Option<String> {
    if cmd.contains("--subagent") {
        return None;
    }
    let breaks = |c: u8| b" \t\n;&|()`'\"".contains(&c);
    let word_end = |mut i: usize| {
        while i < cmd.len() && !breaks(cmd.as_bytes()[i]) {
            i += 1;
        }
        i
    };
    let b = cmd.as_bytes();
    let mut at = Vec::new();
    let (mut i, mut command_position, mut quote) = (0, true, None::<u8>);
    while i < b.len() {
        let ch = b[i];
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            } else if ch == b'\\' && q == b'"' {
                i += 1;
            }
            i += 1;
            continue;
        }
        match ch {
            b'\'' | b'"' => {
                quote = Some(ch);
                command_position = false;
                i += 1;
            }
            b';' | b'&' | b'|' | b'(' | b'`' | b'\n' => {
                command_position = true;
                i += 1;
            }
            b')' => {
                command_position = false;
                i += 1;
            }
            b' ' | b'\t' => i += 1,
            b'\\' => {
                command_position = false;
                i += 2;
            }
            _ => {
                let start = i;
                i = word_end(i);
                let word = &cmd[start..i];
                let assignment = word
                    .split_once('=')
                    .is_some_and(|(k, _)| !k.is_empty() && k.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_'));
                if command_position && word.rsplit('/').next() == Some("robofinger") {
                    // After the subcommand, when one follows.
                    let mut j = i;
                    while j < b.len() && (b[j] == b' ' || b[j] == b'\t') {
                        j += 1;
                    }
                    if j < b.len() && b[j].is_ascii_lowercase() {
                        i = word_end(j);
                    }
                    at.push(i);
                    command_position = false;
                } else if !(command_position && assignment) {
                    command_position = false;
                }
            }
        }
    }
    if at.is_empty() {
        return None;
    }
    let mut out = String::with_capacity(cmd.len() + at.len() * (tag.len() + 12));
    let mut from = 0;
    for p in at {
        out.push_str(&cmd[from..p]);
        out.push_str(" --subagent ");
        out.push_str(tag);
        from = p;
    }
    out.push_str(&cmd[from..]);
    Some(out)
}

/// Two words derived from the public key: the default display name for an
/// identity that has not chosen one.
///
/// The alias rides *outside* the encryption, because the relay has to tell
/// agents apart — so the old default, the machine's hostname, published
/// `johns-macbook-pro` to everyone who follows you. A derived name leaks
/// nothing, is stable for the life of the key, and cannot be picked: you get
/// the name your key gives you, which is the same reason it cannot be used to
/// impersonate anyone.
///
/// FNV-1a rather than DefaultHasher, whose output is explicitly not stable
/// across Rust releases. This name must never change under a user.
fn derived_alias(pubkey: &str) -> String {
    const ADJ: [&str; 64] = [
        "amber", "ashen", "azure", "basalt", "brisk", "bronze", "calm", "cedar", "clay", "cobalt",
        "coral", "crisp", "dusk", "ember", "fern", "flint", "frost", "ginger", "glass", "gold",
        "grey", "hazel", "indigo", "iron", "ivory", "jade", "lilac", "lunar", "mauve", "mint",
        "moss", "night", "ochre", "olive", "onyx", "opal", "pale", "pearl", "pine", "plum",
        "quiet", "rapid", "river", "rose", "rust", "sable", "sage", "sand", "sepia", "silver",
        "slate", "snow", "solar", "steel", "stone", "swift", "teal", "tidal", "umber", "velvet",
        "violet", "warm", "wheat", "willow",
    ];
    const NOUN: [&str; 64] = [
        "otter", "heron", "falcon", "marten", "badger", "lynx", "raven", "finch", "stoat", "ibis",
        "hare", "vole", "crane", "shrew", "tern", "wren", "kite", "gull", "pike", "perch", "roach",
        "eel", "seal", "whale", "orca", "shark", "ray", "crab", "prawn", "snail", "moth", "beetle",
        "mantis", "cicada", "hornet", "wasp", "ant", "bee", "spider", "newt", "toad", "frog",
        "adder", "viper", "gecko", "skink", "turtle", "weasel", "ferret", "marmot", "beaver",
        "bison", "ibex", "tapir", "okapi", "lemur", "gibbon", "macaw", "toucan", "puffin",
        "osprey", "merlin", "kestrel", "curlew",
    ];
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in pubkey.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!(
        "{}-{}",
        ADJ[(h % ADJ.len() as u64) as usize],
        NOUN[((h >> 32) % NOUN.len() as u64) as usize]
    )
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
/// Where a commit can be read on the web, as a template with `{sha}` in it.
///
/// Derived from `origin` rather than configured, because the remote already
/// names the forge and the repo — a setting for something sitting in `git
/// config` is a setup step nobody performs. `ROBOFINGER_COMMIT_URL` is how a
/// user supplies their own: a full template with a `{sha}` placeholder, which
/// also covers any forge this does not recognise.
///
/// A template rather than a provider name, so an unknown forge is one config
/// line instead of a patch.
///
/// Returns `None` for an unrecognised host or an unusable remote rather than
/// guessing a path segment — a link that looks right and 404s is worse than no
/// link, because nothing distinguishes it from a commit that was rebased away.
fn commit_url_template() -> Option<String> {
    if let Some(t) = std::env::var("ROBOFINGER_COMMIT_URL")
        .ok()
        .or_else(|| config_file().get("ROBOFINGER_COMMIT_URL").cloned())
        .filter(|s| !s.is_empty())
    {
        return Some(t);
    }
    let base = origin_base()?;
    // The three big forges agree on everything but one path segment.
    let host = base
        .split('/')
        .nth(2)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let seg = match host.as_str() {
        h if h.ends_with("github.com") => "/commit/",
        h if h.ends_with("gitlab.com") => "/-/commit/",
        h if h.ends_with("bitbucket.org") => "/commits/",
        // Known hosts only. A guessed segment yields a link that looks right
        // and 404s, and a reader cannot tell that from a commit rebased away.
        _ => return None,
    };
    Some(format!("{base}{seg}{{sha}}"))
}

/// This repo's `origin` as a browsable `https://host/org/repo`, or None.
fn origin_base() -> Option<String> {
    let out = std::process::Command::new("git")
        .current_dir(repo_root())
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    normalize_remote(String::from_utf8_lossy(&out.stdout).trim())
}

/// `git@host:org/repo.git` and `https://host/org/repo.git` to `https://host/org/repo`.
///
/// Anything with credentials in it is refused: those end up in a rendered link
/// and a published page, and stripping them silently would be a guess about
/// what the user meant.
fn normalize_remote(raw: &str) -> Option<String> {
    let raw = raw.trim().trim_end_matches('/');
    let rest = match raw.strip_prefix("git@") {
        // scp-style: host:path
        Some(r) => {
            let (host, path) = r.split_once(':')?;
            format!("{host}/{}", path.trim_start_matches('/'))
        }
        None => raw
            .strip_prefix("https://")
            .or_else(|| raw.strip_prefix("http://"))
            .or_else(|| raw.strip_prefix("ssh://git@"))
            .or_else(|| raw.strip_prefix("git://"))?
            .to_string(),
    };
    if rest.contains('@') {
        return None;
    }
    let rest = rest.trim_end_matches(".git");
    // host + at least one path segment, or there is nothing to link to.
    if rest.split('/').filter(|s| !s.is_empty()).count() < 2 {
        return None;
    }
    Some(format!("https://{rest}"))
}

/// Expand a template for one sha. Separate so a caller with many shas builds
/// the template once.
fn commit_url(template: &str, sha: &str) -> String {
    template.replace("{sha}", sha)
}

/// Short SHAs of the commits a claim covered — the pointer form of
/// `commits_since`, same window and same pathspecs.
///
/// Lives in its own field rather than the note because `task` is capped and a
/// derived note is the first thing to hit that cap. A truncated sentence still
/// leaves the commits findable.
fn commit_shas(since: i64, globs: &[String]) -> Vec<String> {
    git_log(since, globs, "--format=%h")
        .into_iter()
        .rev()
        .collect()
}

#[cfg(test)]
fn commits_since_in(dir: &std::path::Path, since: i64, globs: &[String]) -> String {
    git_log_in(dir, since, globs, "--format=%s")
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("; ")
}

fn commits_since(since: i64, globs: &[String]) -> String {
    // Oldest first reads as a narrative; newest first reads as a stack.
    // `truncate` at the call site cuts the tail, so the earliest work — the
    // part that explains the rest — is what survives a long list.
    git_log(since, globs, "--format=%s")
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("; ")
}

/// One `git log` over the claim window, scoped to the claimed paths.
fn git_log(since: i64, globs: &[String], format: &str) -> Vec<String> {
    git_log_in(std::path::Path::new(&repo_root()), since, globs, format)
}

/// `git_log` against an explicit directory.
///
/// Split out so the tests can point at a fixture without mutating the process
/// cwd — which is shared by every test in the binary, and on Windows resolves
/// the temp dir through a path `git rev-parse` does not agree with.
fn git_log_in(dir: &std::path::Path, since: i64, globs: &[String], format: &str) -> Vec<String> {
    let mut args = vec![
        "log".to_string(),
        "--no-merges".to_string(),
        format.to_string(),
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
        .current_dir(dir)
        .args(&args)
        .output()
    else {
        return vec![];
    };
    if !out.status.success() {
        return vec![];
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

/// Repo name, so `src/**` in one project never collides with `src/**` in another.
///
/// Named after the shared `.git`, not the checkout: a worktree lives in a
/// directory like `.claude/worktrees/<name>`, and naming it by that put its
/// claims in a project nobody in the main checkout was looking at.
fn project() -> String {
    let out = std::process::Command::new("git")
        .args([
            "rev-parse",
            "--path-format=absolute",
            "--git-common-dir",
            "--show-toplevel",
        ])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let mut lines = out.lines();
    project_name(lines.next(), lines.next())
}

/// The directory holding the shared `.git`, else the checkout's own directory
/// (a submodule's common dir is `.git/modules/<name>`, which says nothing).
fn project_name(common_dir: Option<&str>, toplevel: Option<&str>) -> String {
    common_dir
        .and_then(|d| d.trim_end_matches('/').strip_suffix("/.git"))
        .or(toplevel)
        .and_then(|p| p.trim_end_matches('/').rsplit('/').next())
        .filter(|s| !s.is_empty())
        .map(String::from)
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
        alias: String::new(),
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
        commits: vec![],
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
        alias: String::new(),
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
        commits: vec![],
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
            && (name_for(&p.pubkey).eq_ignore_ascii_case(to)
                || p.alias.eq_ignore_ascii_case(to)
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
        // The rendered name first, because that is what a user reads off the
        // screen and types back. `p.alias` stays accepted: it is what older
        // entries were filed under, and a filter that stops matching history
        // is worse than one that is generous.
        //
        // Past the first `/` is an agent, the way the byline prints one —
        // `hazel-hare/claude-1` is that agent and every subagent under it.
        Some(w) => {
            let (who, agent) = w.split_once('/').unwrap_or((w, ""));
            let agent = agent.trim_end_matches('/');
            let named = derived_alias(&p.pubkey).eq_ignore_ascii_case(who)
                || label_in(&p.pubkey, names_cached(), subs)
                    .is_some_and(|l| l.eq_ignore_ascii_case(who))
                || p.alias == who;
            named && (agent.is_empty() || p.instance == agent || nested_in(&p.instance, agent))
        }
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
    // Trailing, because it is a reference rather than something to read: the
    // eye skips it until it is wanted.
    let refs = if p.commits.is_empty() {
        String::new()
    } else {
        format!("  [{}]", p.commits.join(" "))
    };
    format!("{head}\n  {verb}{addressed}{paths}{detail}{refs}")
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

/// Remove `name` and its value from `args`, wherever they sit, returning the
/// value. For a flag every command accepts, which has to be gone before any
/// of them reads its arguments.
fn take_flag(args: &mut Vec<String>, name: &str) -> Option<String> {
    let i = args.iter().position(|a| a == name)?;
    args.remove(i);
    (i < args.len()).then(|| args.remove(i))
}

/// The JSON a hook was handed on stdin, or Null.
///
/// Never blocks at a terminal: read_to_string waits for EOF, and a human
/// typing a hook command by hand never sends one. Hooks pipe and close.
fn hook_input() -> serde_json::Value {
    use std::io::IsTerminal;
    if std::io::stdin().is_terminal() {
        return serde_json::Value::Null;
    }
    let mut buf = String::new();
    let _ = std::io::stdin().read_to_string(&mut buf);
    serde_json::from_str(&buf).unwrap_or(serde_json::Value::Null)
}

/// Drop this agent's claim, journalling what happened under it. Returns the
/// globs that were held, empty when there were none.
fn release(c: &Cfg, k: &Keys, note: Option<String>) -> Vec<String> {
    let prev = mine(c, k);
    let task = prev.as_ref().map(|p| p.task.clone()).unwrap_or_default();
    let held: Vec<String> = prev
        .as_ref()
        .map(|p| p.touching.clone())
        .unwrap_or_default();
    let _ = publish(c, k, "working", &task, vec![]);
    // Releasing nothing is a no-op, not an event.
    if !held.is_empty() {
        // The note says what happened; the task only ever said what was
        // intended. Prefer an explicit note, then the commits made
        // under the claim — which say what happened without anyone
        // having to write it down — then the intent, then the
        // duration.
        let at = prev.as_ref().map(|p| p.claimed_at).unwrap_or(0);
        // The commits are recorded whether or not they supplied the
        // note: an explicit --note says what happened, and the SHAs
        // still say where to look.
        let shas = if at > 0 {
            commit_shas(at, &held)
        } else {
            vec![]
        };
        let text = match (&note, at) {
            (Some(n), _) => n.clone(),
            (None, at) if at > 0 => {
                let done = commits_since(at, &held);
                if done.is_empty() {
                    format!("{task} (held {})", dur(now().saturating_sub(at)))
                } else {
                    done
                }
            }
            _ => task.clone(),
        };
        let text = carry_ticket(&task, &text);
        timeline_with_commits(c, k, "release", &text, held.clone(), shas);
    }
    held
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
    let _ = post(c, k, text, None, kind, globs, vec![], "", 0);
}

/// `timeline`, for the one event that can name the commits it covered.
fn timeline_with_commits(
    c: &Cfg,
    k: &Keys,
    kind: &str,
    text: &str,
    globs: Vec<String>,
    commits: Vec<String>,
) {
    let _ = post(c, k, text, None, kind, globs, commits, "", 0);
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
    commits: Vec<String>,
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
        alias: String::new(),
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
        commits,
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

/// Is `child` a subagent of `parent`, at any depth?
fn nested_in(child: &str, parent: &str) -> bool {
    !parent.is_empty()
        && child
            .strip_prefix(parent)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Could this claim conflict with an edit by `instance` on key `pubkey`?
///
/// Not when it is this agent's own, nor its parent's or one of its own
/// subagents': a parent's claim covers the work it hands down (ADR 0003).
/// Siblings are not exempt — parallel subagents in one session are the
/// collision a swarm makes most likely.
fn may_conflict(p: &Plan, pubkey: &str, instance: &str, here: &str, t: i64) -> bool {
    let family = p.pubkey == pubkey
        && (p.instance == instance || nested_in(&p.instance, instance) || nested_in(instance, &p.instance));
    !family && p.project == here && p.live(t)
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
        if !may_conflict(&p, &k.pubkey(), &c.instance, &here, t) {
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

/// Commands a hook runs, inside an agent's session.
fn is_hook(cmd: &str) -> bool {
    matches!(
        cmd,
        "check" | "start" | "end" | "subagent-start" | "subagent-stop" | "activity"
    )
}

/// Note what an agent just did in this machine's activity spool. No network.
fn spool(agent: &str, kind: &str, tool: &str, target: &str) {
    activity::append(
        &activity::dir(&crypto::config_dir()),
        &activity::Event {
            t: now(),
            agent: agent.into(),
            kind: kind.into(),
            tool: tool.into(),
            target: target.into(),
        },
    );
}

/// Tools whose target is a file this agent is about to change: the only ones
/// a claim can conflict with.
fn edits(tool: &str) -> bool {
    matches!(tool, "Edit" | "MultiEdit" | "Write" | "NotebookEdit")
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // Written by the PreToolUse hook onto a subagent's robofinger calls; see
    // `tag_robofinger`. Lifted out before any command reads its arguments.
    let subagent = take_flag(&mut args, "--subagent").and_then(|s| parse_subagent(&s));
    let cmd = args.first().map(String::as_str).unwrap_or("");

    // Hooks run inside an agent's session and must never be what it waits on.
    if is_hook(cmd) {
        NET_BUDGET.store(HOOK_TIMEOUT, std::sync::atomic::Ordering::Relaxed);
    }

    // Keys are generated on first use. A hook must never break a session, so
    // failure here is fatal only for interactive commands.
    let k = match Keys::load_or_create() {
        Ok(k) => k,
        Err(e) => {
            if is_hook(cmd) {
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
            //
            // The global layer only: `config_file()` merges the repo layers in,
            // and copying one of those up here would promote a value scoped to
            // one project into the default for every project.
            let existing = parse_config(&crypto::config_dir().join("config"));
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

            // Written unconditionally now, and that is what keeps an existing
            // user's name stable: `cfg()` still falls back to the hostname at
            // runtime, so anyone who never pinned an alias keeps the name their
            // peers already know until they rerun `init`. A rename that happens
            // on upgrade reads, to everyone following you, as one person
            // leaving and a stranger arriving.
            let saved_alias = alias
                .clone()
                .or_else(|| existing.get("ROBOFINGER_ALIAS").cloned())
                .or_else(|| existing.get("ROBOFINGER_AGENT").cloned())
                .unwrap_or_else(|| derived_alias(&k.pubkey()));

            let mut body = format!("ROBOFINGER_URL={url}\n");
            body.push_str(&format!("ROBOFINGER_ALIAS={saved_alias}\n"));
            // `init` rewrites the file wholesale, so anything it does not know
            // to carry across is silently lost on the next run.
            if let Some(t) = existing.get("ROBOFINGER_COMMIT_URL") {
                body.push_str(&format!("ROBOFINGER_COMMIT_URL={t}\n"));
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
            println!("publishing as \"{saved_alias}\" — change it with --alias");
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
        "name" => {
            let mut names = load_names();
            let peers = crypto::load_peers();
            // Resolve however a user would type it: a full key, a prefix, or
            // the label they already filed that peer under.
            let resolve = |w: &str| -> Option<String> {
                peers
                    .iter()
                    .find(|s| {
                        s.pubkey == w || s.pubkey.starts_with(w) || s.label.eq_ignore_ascii_case(w)
                    })
                    .map(|s| s.pubkey.clone())
            };
            let (key, label) = match (args.get(1), args.get(2)) {
                (None, _) => {
                    if names.is_empty() {
                        println!("no local names yet");
                        println!("  robofinger name <label>           name this identity");
                        println!("  robofinger name <peer> <label>    name someone you follow");
                    }
                    let mut rows: Vec<_> = names.iter().collect();
                    rows.sort();
                    for (k2, v) in rows {
                        let mine = if *k2 == k.pubkey() { "  (you)" } else { "" };
                        println!("{} ({}){}", derived_alias(k2), v, mine);
                    }
                    return;
                }
                // One argument names your own key, because that is the case
                // `add --as` cannot reach.
                (Some(l), None) => (k.pubkey(), l.clone()),
                (Some(w), Some(l)) => match resolve(w) {
                    Some(pk) => (pk, l.clone()),
                    None => {
                        eprintln!("nobody you follow matches {w:?} — see: robofinger list");
                        std::process::exit(1);
                    }
                },
            };
            if label.is_empty() {
                names.remove(&key);
            } else {
                names.insert(key.clone(), label.clone());
            }
            let body: String = names.iter().map(|(k2, v)| format!("{k2}\t{v}\n")).collect();
            if let Err(e) = std::fs::write(names_path(), body) {
                eprintln!("write {}: {e}", names_path().display());
                std::process::exit(1);
            }
            println!("{} ({})", derived_alias(&key), label);
            eprintln!("local only — never published, and peers keep their own names for you.");
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
    let Some(mut c) = cfg() else {
        if is_hook(cmd) {
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
    if let Some((kind, id)) = &subagent {
        c.instance = subagent_instance(&c.instance, kind, id);
    }

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
                    println!(
                        "claimed {globs:?} as {} ({task})",
                        agent_path(&k.pubkey(), &c.instance)
                    );
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
            release(&c, &k, flag(&args, "--note"));
            println!("released");
        }
        "done" => {
            let held: Vec<String> = mine(&c, &k).map(|p| p.touching).unwrap_or_default();
            let _ = publish(&c, &k, "done", "", vec![]);
            println!("done");
            timeline(&c, &k, "done", "", held);
        }
        // PreToolUse hook: hook JSON on stdin, advisory warning on stdout.
        "check" => {
            // Only read stdin when no path was given: `robofinger check
            // <path>` is also typed by hand.
            let input = if args.get(1).is_some() {
                serde_json::Value::Null
            } else {
                hook_input()
            };
            let sub = hook_subagent(&input);
            if let Some((kind, id)) = &sub {
                c.instance = subagent_instance(&c.instance, kind, id);
            }

            // Every tool call is a line in the activity spool; only edits and
            // shell commands go any further. Matching every tool keeps the
            // live view honest about reads and searches, and costs a local
            // append — never the network.
            let tool = input.get("tool_name").and_then(|v| v.as_str()).unwrap_or("");
            if !tool.is_empty() {
                let (tool, target) = activity::describe(&input);
                spool(&c.instance, "tool", &tool, &target);
            }
            if !tool.is_empty() && tool != "Bash" && !edits(tool) {
                std::process::exit(0);
            }

            // A subagent's shell command: tag its robofinger calls with who
            // ran them, so its claims are filed under its own name. No
            // network, and nothing at all for the main agent's commands.
            if input.get("tool_name").and_then(|v| v.as_str()) == Some("Bash") {
                let line = input.pointer("/tool_input/command").and_then(|v| v.as_str());
                if let (Some((kind, id)), Some(line)) = (&sub, line)
                    && let Some(tagged) = tag_robofinger(line, &format!("{kind}:{id}"))
                {
                    // updatedInput replaces the whole input, so carry the
                    // rest of it. No permission decision: whether the
                    // command may run is still the user's rules to say.
                    let mut updated = input["tool_input"].clone();
                    updated["command"] = serde_json::Value::String(tagged);
                    println!(
                        "{}",
                        serde_json::json!({
                            "hookSpecificOutput": {
                                "hookEventName": "PreToolUse",
                                "updatedInput": updated
                            }
                        })
                    );
                }
                std::process::exit(0);
            }

            let path = args.get(1).cloned().or_else(|| {
                input
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
                    .map(|(p, _)| name_for(&p.pubkey))
                    .unwrap_or_default();
                // A subagent talks only to its parent (ADR 0005), so its way
                // out of a conflict is to say so in what it returns.
                let msg = if sub.is_some() {
                    format!(
                        "CLAIM CONFLICT on {}:\n{}\n\
                         This is advisory. Two options:\n\
                         1. Work elsewhere within your task, if you can.\n\
                         2. Otherwise stop and report it to your parent in what you return: \
                         the path, who holds it, and what you would do by default. Your parent \
                         decides; do not ask the holder or a person yourself.",
                        path,
                        detail.join("\n"),
                    )
                } else {
                    format!(
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
                    )
                };
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
            activity::prune(&activity::dir(&crypto::config_dir()));
            spool(&c.instance, "appear", "", "");

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
                            name_for(&p.pubkey),
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

            println!(
                "{}",
                serde_json::json!({
                    "hookSpecificOutput": {
                        "hookEventName": "SessionStart",
                        "additionalContext": start_context(blocks, &agent_path(&me, &c.instance))
                    }
                })
            );
            std::process::exit(0);
        }
        "end" => {
            spool(&c.instance, "gone", "", "");
            // Journal a session that ends still holding paths. The timeline is
            // all a past view has, and without an entry it shows the claim
            // held on until it would have lapsed. One write, and only then.
            let t = now();
            let held: Vec<String> = mine(&c, &k)
                .filter(|p| p.live(t))
                .map(|p| p.touching)
                .unwrap_or_default();
            let _ = publish(&c, &k, "done", "", vec![]);
            if !held.is_empty() {
                timeline(&c, &k, "done", "session ended without releasing", held);
            }
            std::process::exit(0);
        }
        // SubagentStart hook. A subagent never sees the SessionStart block,
        // so it gets the one rule it needs here. No network: subagents are
        // spawned in bursts, and this runs before each one's first prompt.
        "subagent-start" => {
            let input = hook_input();
            let parent = agent_path(&k.pubkey(), &c.instance);
            // Named the way its hooks will name it, so what it is told matches
            // what its claims are filed under. Claude Code's own untyped
            // helpers get no name: they never claim, and naming one would
            // spend a slot.
            let you = match hook_subagent(&input) {
                Some((kind, id)) if !kind.is_empty() && typed(&input) => {
                    let inst = subagent_instance(&c.instance, &kind, &id);
                    spool(&inst, "appear", "", "");
                    let me = agent_path(&k.pubkey(), &inst);
                    format!("{}\n", you_are(&me, Some(&parent)))
                }
                _ => String::new(),
            };
            println!(
                "{}",
                serde_json::json!({
                    "hookSpecificOutput": {
                        "hookEventName": "SubagentStart",
                        "additionalContext": format!("{you}{CLAIM_RULE}\n{SUBAGENT_RULE}")
                    }
                })
            );
            std::process::exit(0);
        }
        // SubagentStop hook: release whatever the subagent still holds, since
        // the tool says when it is finished rather than leaving the claim to
        // lapse. Prints nothing — a decision on stdout, or exit 2, would keep
        // the subagent running.
        "subagent-stop" => {
            let input = hook_input();
            if let (true, Some((kind, id))) = (typed(&input), hook_subagent(&input)) {
                c.instance = subagent_instance(&c.instance, &kind, &id);
                spool(&c.instance, "done", "", "");
                // Only a subagent that claimed has a row to release. Publishing
                // one for every read-only subagent would spend the write budget
                // and push real claims out of the relay's instance cap.
                let t = now();
                if mine(&c, &k).is_some_and(|p| p.live(t) && !p.touching.is_empty()) {
                    release(&c, &k, None);
                }
            }
            std::process::exit(0);
        }
        // PostToolUse, UserPromptSubmit, Notification and Stop: a line in the
        // activity spool and nothing else. Prints nothing — Stop would read a
        // decision from stdout, and UserPromptSubmit adds it to the prompt.
        "activity" => {
            let kind = args.get(1).map(String::as_str).unwrap_or("");
            if kind.is_empty() {
                show_activity(&c, &k);
                return;
            }
            if !matches!(kind, "idle" | "prompt" | "waiting" | "stop") {
                std::process::exit(0);
            }
            let input = hook_input();
            if let Some((sub_kind, id)) = hook_subagent(&input) {
                c.instance = subagent_instance(&c.instance, &sub_kind, &id);
            }
            let message = input.get("message").and_then(|v| v.as_str()).unwrap_or("");
            spool(&c.instance, kind, "", &truncate(message, 72));
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
            // Built once for the whole read, and only when asked: it shells
            // out to git, and a link is noise on a feed being skimmed.
            let want_links = args.iter().any(|a| a == "--url");
            let links = want_links.then(commit_url_template).flatten();
            // Asked for links and got none: say why, or the flag looks broken.
            if want_links && links.is_none() {
                eprintln!(
                    "no commit links: origin is not a forge I recognise.\n  set ROBOFINGER_COMMIT_URL with a {{sha}} placeholder, e.g.\n  ROBOFINGER_COMMIT_URL=https://git.example.com/org/repo/commit/{{sha}}"
                );
            }
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
                if let Some(t) = &links {
                    for sha in &p.commits {
                        println!("    {}", commit_url(t, sha));
                    }
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
        "serve" => {
            let port: u16 = flag(&args, "--port")
                .and_then(|s| s.parse().ok())
                .unwrap_or(SERVE_PORT);
            if let Err(e) = serve(&c, &k, port) {
                eprintln!("serve: {e}");
                std::process::exit(1);
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

/// Peers, read once per process. `who` runs per rendered line and inside the
/// PreToolUse hook, where the budget is ~100ms and a file read per entry is a
/// cost for nothing — the peer list cannot change while one command runs.
fn peers_cached() -> &'static Vec<Peer> {
    static PEERS: std::sync::OnceLock<Vec<Peer>> = std::sync::OnceLock::new();
    PEERS.get_or_init(crypto::load_peers)
}

/// `pubkey<TAB>name`, one per line. Local, never published, and separate from
/// the peer list because the key you most want to name is your own — and you
/// do not follow yourself.
fn names_path() -> std::path::PathBuf {
    crypto::config_dir().join("names")
}

fn load_names() -> std::collections::HashMap<String, String> {
    std::fs::read_to_string(names_path())
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .filter(|(k, v)| !k.is_empty() && !v.is_empty())
        .collect()
}

fn names_cached() -> &'static std::collections::HashMap<String, String> {
    static NAMES: std::sync::OnceLock<std::collections::HashMap<String, String>> =
        std::sync::OnceLock::new();
    NAMES.get_or_init(load_names)
}

/// What *you* call a key: the `names` file first, then the label you filed a
/// peer under with `add --as`. Rendered in parentheses beside the derived
/// name rather than replacing it, so the fingerprint never leaves the line.
fn label_in(
    pubkey: &str,
    names: &std::collections::HashMap<String, String>,
    subs: &[Peer],
) -> Option<String> {
    names.get(pubkey).cloned().or_else(|| {
        subs.iter()
            .find(|s| s.pubkey == pubkey && !s.label.is_empty())
            .map(|s| s.label.clone())
    })
}

fn local_label(pubkey: &str) -> Option<String> {
    label_in(pubkey, names_cached(), peers_cached())
}

/// What to call the key that published something.
///
/// Deliberately never `p.alias`. The alias rides in the envelope, so a peer
/// can change how it appears to you between one entry and the next — publish
/// as "alice" and be rendered as alice, on a timeline whose whole job is
/// saying who did what. A name is only worth reading if it cannot move under
/// you, so it comes from one of the two places the publisher does not control:
///
///   1. the label you filed them under (`add --as`) — a petname you chose
///      beats anything they assert about themselves
///   2. failing that, the name their public key derives to, which is a
///      fingerprint you can check out loud: "does yours say hazel-hare?"
///
/// The published alias keeps its one job, which is suggesting a label at `add`
/// time. That is what the address format has always called it: a suggestion.
fn name_for(pubkey: &str) -> String {
    derived_alias(pubkey)
}

/// An agent's path: the name its key derives to, then its instance —
/// `hazel-hare/claude-1/explore-2`. What a dashboard URL and a byline both say.
fn agent_path(pubkey: &str, instance: &str) -> String {
    match instance {
        "" => name_for(pubkey),
        i => format!("{}/{i}", name_for(pubkey)),
    }
}

/// Who published a plan: "hazel-hare", or "hazel-hare/claude-2" when the
/// instance needs showing.
///
/// `show_instance` is false when this key has only one live instance — with
/// auto-differentiation every plan now carries an instance, and appending it
/// when you are working alone is noise for what is still the common case.
/// Solo output stays byte-identical to pre-0.2.
fn who(p: &Plan, show_instance: bool) -> String {
    let mut name = name_for(&p.pubkey);
    if show_instance && !p.instance.is_empty() {
        name = format!("{name}/{}", p.instance);
    }
    match local_label(&p.pubkey) {
        Some(l) => format!("{name} ({l})"),
        None => name,
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

/// How long an agent with nothing new in the spool still counts as around.
/// An idle session waiting at its prompt is real; one that crashed without a
/// SessionEnd looks the same, and this is where it stops being shown.
const ACTIVITY_WINDOW: i64 = 2 * 3600;
/// How long a finished agent stays on screen, so finishing is something you
/// see happen rather than a row that silently vanishes.
const FINISHED_SHOWN: i64 = 60;

/// Each agent on this machine and what it is doing, from the activity spool:
/// `(instance, status)`, in tree order.
fn local_activity(t: i64) -> Vec<(String, activity::Status)> {
    local_spool(t)
        .1
        .into_iter()
        .filter(|(_, s)| !finished_long_ago(s, t))
        .collect()
}

/// Finished, and past the minute it stays on screen. Still known to have
/// finished — its own page says so — but no longer worth a row.
fn finished_long_ago(s: &activity::Status, t: i64) -> bool {
    matches!(s.state, activity::State::Done | activity::State::Gone) && t - s.since >= FINISHED_SHOWN
}

/// The spool as it stood at `t`, and each agent's status then. It keeps a
/// few hundred lines per session, so a moment scrubbed to recently still
/// shows what this machine's agents were doing in it.
fn local_spool(t: i64) -> (Vec<activity::Event>, Vec<(String, activity::Status)>) {
    let mut events = activity::read(&activity::dir(&crypto::config_dir()));
    events.retain(|e| e.t <= t);
    let statuses = statuses(&events, t);
    (events, statuses)
}

fn statuses(events: &[activity::Event], t: i64) -> Vec<(String, activity::Status)> {
    let mut agents: Vec<&str> = events.iter().map(|e| e.agent.as_str()).collect();
    agents.sort();
    agents.dedup();
    agents
        .into_iter()
        .filter_map(|a| {
            let mine: Vec<&activity::Event> = events.iter().filter(|e| e.agent == a).collect();
            // A finished agent is kept as long as any other, so a page that
            // names it says it finished rather than falling back to the
            // timeline's "last seen", which reads as still around.
            let s = activity::status(&mine)?;
            (t - s.since < ACTIVITY_WINDOW).then(|| (a.to_string(), s))
        })
        .collect()
}

/// A status as one line: glyph, then what it is doing and for how long.
fn status_line(s: &activity::Status, t: i64) -> String {
    use activity::State::*;
    let for_ = dur(t - s.since);
    match s.state {
        Working | Thinking => format!("✻ {} · {for_}", s.text),
        NeedsYou | YourTurn => format!("‖ {} · {for_}", s.text),
        Done | Gone => format!("✓ {} · {}", s.text, ago(t - s.since)),
    }
}

/// `robofinger activity`: this machine's agents, live, the way `serve` shows them.
fn show_activity(c: &Cfg, k: &Keys) {
    let t = now();
    let rows = local_activity(t);
    if rows.is_empty() {
        println!("no agents active on this machine");
        return;
    }
    for (inst, s) in rows {
        let depth = inst.matches('/').count();
        let name = inst.rsplit('/').next().unwrap_or(&inst);
        let mark = if inst == c.instance { "  (this one)" } else { "" };
        println!(
            "{}{:<w$} {}{mark}",
            "  ".repeat(depth),
            name,
            status_line(&s, t),
            w = 22usize.saturating_sub(2 * depth)
        );
    }
    println!("
as {}", name_for(&k.pubkey()));
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
    println!("you are {}", agent_path(&k.pubkey(), &c.instance));

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
            println!("{} {}", stamp(p.epoch), name_for(&p.pubkey));
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

/// `robofinger serve`: the record as a page, for the human at the top.
///
/// Local and read-only, rendered where the keys already are — the fallback
/// `docs/design.md` §5 names for the team view. The relay keeps serving
/// ciphertext and learns nothing new. Bound to 127.0.0.1 because the page is
/// decrypted content: anyone who can reach the port reads it as you.
///
/// ponytail: one thread, one relay fetch per request. A human refreshing a
/// tab is the only load; go async only if several viewers ever share one.
fn serve(c: &Cfg, k: &Keys, port: u16) -> std::io::Result<()> {
    use std::io::{BufRead, Write};
    let listener = std::net::TcpListener::bind(("127.0.0.1", port))?;
    println!("serving the record at http://127.0.0.1:{port}  (ctrl-c to stop)");
    // Shells out to git, so once, not per refresh. The template is this
    // repo's origin, so it only links entries from this repo — a teammate's
    // sha from another project would get a link that 404s.
    let links = commit_url_template().map(|t| (project(), t));
    let tracker = tracker::Tracker::load();
    let mut cache = Cache::default();
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        // Browsers open a spare connection ahead of time and may never send
        // on it. One thread blocked reading that socket stalls every real
        // request behind it, so give up on a silent one quickly.
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(1)));
        // Only the request line matters. Anything that is not a page — a
        // favicon, a probe — gets an empty 404 rather than a relay round trip.
        let mut line = String::new();
        let _ = std::io::BufReader::new(&stream).read_line(&mut line);
        let route = line
            .strip_prefix("GET ")
            .and_then(|r| r.split_whitespace().next())
            .and_then(parse_route);
        let (status, body) = match route.map(|r| page(c, k, &r, links.as_ref(), &mut cache, &tracker)) {
            Some(Ok(html)) => ("200 OK", html),
            Some(Err(e)) => ("400 Bad Request", esc(&e)),
            None => ("404 Not Found", String::new()),
        };
        let _ = write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\n\
             Content-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
    }
    Ok(())
}

/// A dashboard page's address: `/<who>/<agent>/<subagent>…?at=<when>`.
///
/// The path is the byline the record already prints, so any name on the page
/// is also a link to that agent and everything under it. `who` is the
/// key-derived name, the one every reader computes alike, so a link means
/// the same agent to everyone it is shared with.
struct Route {
    who: Option<String>,
    agent: String,
    at: Option<String>,
}

fn parse_route(target: &str) -> Option<Route> {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let segs: Vec<&str> = path
        .strip_prefix('/')?
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    // Names, slots and agent types are all plain tokens, so anything else —
    // `..`, a favicon, markup — is not a page.
    if segs.iter().any(|s| !plain_token(s)) {
        return None;
    }
    let at = query
        .split('&')
        .find_map(|kv| kv.strip_prefix("at="))
        .filter(|v| !v.is_empty())
        // A browser form sends `stamp`'s space form as `+` or `%20`.
        .map(|v| v.replace("%3A", ":").replace("%3a", ":").replace('+', " ").replace("%20", " "));
    Some(Route {
        who: segs.first().map(|s| s.to_string()),
        agent: segs.get(1..).unwrap_or_default().join("/"),
        at,
    })
}

/// When `?at=` means: what `--since` takes, or a local time as `stamp`
/// prints it (`2026-10-24T18:51`, or with a space).
fn parse_at(s: &str, now: i64) -> Result<i64, String> {
    parse_since(s, now).or_else(|_| {
        local_minute(s).ok_or_else(|| {
            format!("at= wants an epoch, a duration like 2h, or a local time like 2026-10-24T18:51, not {s:?}")
        })
    })
}

/// `stamp`, backwards: a local wall-clock time to an epoch.
fn local_minute(s: &str) -> Option<i64> {
    let (date, time) = s.split_once(['T', ' '])?;
    let num = |x: Option<&str>| x?.parse::<i64>().ok();
    let mut d = date.splitn(3, '-');
    let (y, m, day) = (num(d.next())?, num(d.next())?, num(d.next())?);
    let mut hm = time.splitn(3, ':');
    let (h, min) = (num(hm.next())?, num(hm.next())?);
    let sec = num(hm.next()).unwrap_or(0);
    if !(1..=12).contains(&m) || !(1..=31).contains(&day) || h > 23 || min > 59 || sec > 59 {
        return None;
    }
    // Days from the civil date: the inverse of what `stamp` does.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let naive = (era * 146_097 + doe - 719_468) * 86_400 + h * 3600 + min * 60 + sec;
    Some(naive - utc_offset(naive))
}

/// The events either side of `t`: where scrubbing steps to. By event rather
/// than by interval, because the moments worth looking at are the ones where
/// something changed.
fn neighbours(epochs: &[i64], t: i64) -> (Option<i64>, Option<i64>) {
    (
        epochs.iter().copied().filter(|&e| e < t).max(),
        epochs.iter().copied().filter(|&e| e > t).min(),
    )
}

/// What each agent held at `t`, replayed from the timeline. The relay's claim
/// rows keep only the last few per agent, so the past has to come from here.
///
/// A claim holds until its agent releases, finishes or claims again. The
/// timeline records changes, not the republish a working agent does on every
/// edit, so a claim whose agent has written nothing for longer than a claim
/// lives is taken to have lapsed — the deadman switch, judged from the
/// agent's own last word.
fn claims_at(posts: &[Plan], t: i64) -> Vec<Plan> {
    let mut seen: Vec<&Plan> = posts.iter().filter(|p| p.epoch <= t).collect();
    seen.sort_by_key(|p| (p.epoch, p.id));
    let mut held: std::collections::HashMap<(String, String), Plan> =
        std::collections::HashMap::new();
    let mut last: std::collections::HashMap<(String, String), i64> =
        std::collections::HashMap::new();
    for p in seen {
        let key = (p.pubkey.clone(), p.instance.clone());
        last.insert(key.clone(), p.epoch);
        match p.kind.as_str() {
            "claim" => {
                let mut c = p.clone();
                c.status = "working".into();
                c.touching = p.globs.clone();
                c.claimed_at = p.epoch;
                c.eta_s = DEFAULT_ETA;
                c.kind = String::new();
                held.insert(key, c);
            }
            "release" | "done" => {
                held.remove(&key);
            }
            _ => {}
        }
    }
    held.into_iter()
        .filter_map(|(key, mut c)| {
            c.epoch = last[&key];
            c.live(t).then_some(c)
        })
        .collect()
}

/// Where a dashboard page sits: whose record, which agent, and when — with
/// the events either side, for stepping through it.
#[derive(Default)]
struct View {
    who: Option<String>,
    agent: String,
    /// None for the live page.
    at: Option<i64>,
    prev: Option<i64>,
    next: Option<i64>,
    /// The earliest entry this page could see, so a moment before it says so
    /// rather than looking quiet.
    oldest: Option<i64>,
}

impl View {
    fn path(&self) -> String {
        match &self.who {
            None => "/".into(),
            Some(w) if self.agent.is_empty() => format!("/{w}"),
            Some(w) => format!("/{w}/{}", self.agent),
        }
    }

    /// `?at=` for links that stay at this page's moment.
    fn moment(&self) -> String {
        self.at.map(|t| format!("?at={t}")).unwrap_or_default()
    }

    /// Breadcrumb, then the scrub controls.
    fn nav(&self, t: i64) -> String {
        let q = self.moment();
        let mut crumbs = vec![format!(r#"<a href="/{q}">everyone</a>"#)];
        if let Some(w) = &self.who {
            let mut href = format!("/{w}");
            crumbs.push(format!(r#"<a href="{}{q}">{}</a>"#, esc(&href), esc(w)));
            for seg in self.agent.split('/').filter(|s| !s.is_empty()) {
                href = format!("{href}/{seg}");
                crumbs.push(format!(r#"<a href="{}{q}">{}</a>"#, esc(&href), esc(seg)));
            }
        }
        let path = esc(&self.path());
        let step = |at: Option<i64>, label: &str| match at {
            Some(e) => format!(r#"<a href="{path}?at={e}">{label}</a>"#),
            None => format!("<span class=dim>{label}</span>"),
        };
        let mut scrub = vec![step(self.prev, "◀ previous event")];
        if self.at.is_some() {
            scrub.push(format!("<b>as of {}</b>", stamp(t)));
            scrub.push(step(self.next, "next event ▶"));
            scrub.push(format!(r#"<a href="{path}">now</a>"#));
        }
        if let Some(o) = self.oldest.filter(|&o| t < o) {
            scrub.push(format!(
                "<span class=dim>nothing this page can see goes back that far — it starts {}</span>",
                stamp(o)
            ));
        }
        format!(
            "<nav class=crumbs>{}</nav><nav class=scrub>{}</nav>",
            crumbs.join(" / "),
            scrub.join(" · ")
        )
    }
}

/// What `serve` keeps between requests. A live page re-fetches itself every
/// two seconds, and a relay round trip each time would spend the per-IP
/// limit the hooks on this machine also live under; the tracker is slower
/// still. The activity spool is local and read fresh every time.
#[derive(Default)]
struct Cache {
    posts: Option<(i64, Vec<Plan>)>,
    live: Option<(i64, Vec<Plan>)>,
    plan: Option<(i64, Option<Vec<tracker::Group>>)>,
}

/// How old a cached relay answer may be: what the page's refresh used to be.
const RELAY_TTL: i64 = 15;
const PLAN_TTL: i64 = 60;

impl Cache {
    fn posts(&mut self, c: &Cfg, k: &Keys) -> Vec<Plan> {
        let t = now();
        if self.posts.as_ref().is_none_or(|(at, _)| t - at >= RELAY_TTL) {
            // ponytail: the newest MAX_POST_LIMIT entries are all a page can
            // scrub through; going further back means paging with `before=`.
            self.posts = Some((t, fetch_posts(c, k, MAX_POST_LIMIT)));
        }
        self.posts.as_ref().map(|(_, p)| p.clone()).unwrap_or_default()
    }

    fn live(&mut self, c: &Cfg, k: &Keys) -> Vec<Plan> {
        let t = now();
        if self.live.as_ref().is_none_or(|(at, _)| t - at >= RELAY_TTL) {
            self.live = Some((t, current_plans(c, k)));
        }
        self.live.as_ref().map(|(_, p)| p.clone()).unwrap_or_default()
    }

    fn plan(&mut self) -> Option<Vec<tracker::Group>> {
        let t = now();
        if self.plan.as_ref().is_none_or(|(at, _)| t - at >= PLAN_TTL) {
            self.plan = Some((t, tracker::plan()));
        }
        self.plan.as_ref().and_then(|(_, p)| p.clone())
    }
}

/// One dashboard page: everyone, one person, or one agent and its subagents,
/// now or as it stood at a moment.
fn page(
    c: &Cfg,
    k: &Keys,
    r: &Route,
    links: Option<&(String, String)>,
    cache: &mut Cache,
    tracker: &tracker::Tracker,
) -> Result<String, String> {
    let now_t = now();
    let at = r.at.as_deref().map(|s| parse_at(s, now_t)).transpose()?;
    // The first segment is the key-derived name and nothing else: a local
    // label or a published alias would make a link mean one agent to you and
    // another to whoever you send it to. The same rule as the tree and the
    // feed, so every panel on the page agrees on who is in scope.
    let scope = View {
        who: r.who.clone(),
        agent: r.agent.clone(),
        ..View::default()
    };
    let keep = |p: &Plan| dash::in_scope(&scope, &p.pubkey, &p.instance);

    let all_posts = cache.posts(c, k);
    let posts: Vec<Plan> = all_posts.iter().filter(|p| keep(p)).cloned().collect();
    let epochs: Vec<i64> = posts.iter().map(|p| p.epoch).collect();
    let t = at.unwrap_or(now_t);
    let (prev, next) = neighbours(&epochs, t);
    let view = View {
        who: r.who.clone(),
        agent: r.agent.clone(),
        at,
        prev,
        next,
        oldest: epochs.iter().min().copied(),
    };
    let all_posts: Vec<Plan> = all_posts.into_iter().filter(|p| p.epoch <= t).collect();
    let all_live = match at {
        None => cache.live(c, k),
        Some(t) => claims_at(&all_posts, t),
    };
    let live: Vec<Plan> = all_live.iter().filter(|p| keep(p)).cloned().collect();
    let posts: Vec<Plan> = posts.into_iter().filter(|p| p.epoch <= t).collect();
    let (events, local) = local_spool(t);
    let plan = cache.plan();
    let me = k.pubkey();
    let b = dash::Board {
        live: &live,
        posts: &posts,
        all_live: &all_live,
        all_posts: &all_posts,
        local: &local,
        events: &events,
        me: &me,
        plan: plan.as_deref(),
        tracker,
    };
    Ok(render(&b, c, t, links, &view))
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// A live claim that has gone quiet past its ETA but not yet expired.
///
/// The window between `eta_s` and `eta_s * STALE_MULT` is exactly the "well
/// past its ETA" case the escalation rule names: still blocking teammates,
/// and nobody can tell yet whether the agent is thinking or dead.
fn stuck(p: &Plan, t: i64) -> bool {
    p.live(t) && !p.touching.is_empty() && t - p.epoch > p.eta_s
}

/// Asks nobody has answered yet, the ones addressed to this node first.
fn open_asks<'a>(posts: &'a [Plan], c: &Cfg) -> Vec<&'a Plan> {
    let answered: std::collections::HashSet<i64> = posts
        .iter()
        .filter(|p| p.kind == "answer" && p.reply_to > 0)
        .map(|p| p.reply_to)
        .collect();
    let mut v: Vec<&Plan> = posts
        .iter()
        .filter(|p| p.kind == "ask" && !answered.contains(&p.id))
        .collect();
    v.sort_by_key(|p| (!addressed_to_me(p, c), std::cmp::Reverse(p.epoch)));
    v
}

/// The ticket an entry is filed under, as (key, link).
///
/// Recognises what a tracker hands an agent: `#123`, an issue URL, or a
/// local-markdown ticket file. A GitHub or GitLab URL keys as `#123`, so a
/// claim that says `#88` and a release that pastes the URL land in one row.
/// Deliberately not `ABC-123`: that is also how ADRs and RFCs are numbered, and
/// a decision record is not a ticket.
///
/// ponytail: first match wins, one ticket per entry. Work spanning tickets
/// files under the first one named.
fn ticket_ref(task: &str) -> Option<(String, Option<String>)> {
    task.split_whitespace().find_map(|raw| {
        let w = raw.trim_matches(|c: char| "()[]{},.;:\"'".contains(c));
        if let Some(n) = w.strip_prefix('#') {
            return (!n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
                .then(|| (w.to_string(), None));
        }
        if !(w.starts_with("https://") || w.starts_with("http://")) {
            // The trim above would eat the dot that makes `.scratch` the tracker's.
            let path = raw
                .trim_start_matches(|c: char| "([{\"'".contains(c))
                .trim_end_matches(|c: char| ")]},.;:\"'".contains(c));
            return scratch_ticket(path).map(|key| (key, None));
        }
        // github .../issues/88, gitlab .../-/issues/88, linear .../issue/ENG-1/slug,
        // jira .../browse/ENG-1
        let path = w.split(['?', '#']).next().unwrap_or(w);
        let mut parts = path.split('/');
        let seg = parts.find(|seg| matches!(*seg, "issues" | "issue" | "browse"))?;
        let id = parts.next().filter(|id| !id.is_empty())?;
        // Bitbucket Server browses files under the same word Jira browses issues.
        if seg == "browse" && !tracker::is_key(id) {
            return None;
        }
        let key = if id.bytes().all(|b| b.is_ascii_digit()) {
            format!("#{id}")
        } else {
            id.to_string()
        };
        Some((key, Some(w.to_string())))
    })
}

/// `.scratch/<feature>/issues/NN-slug.md`, the local-markdown tracker's ticket
/// file, keyed `feature#NN`.
fn scratch_ticket(path: &str) -> Option<String> {
    let segs: Vec<&str> = path.split('/').collect();
    let i = segs.iter().position(|s| *s == ".scratch")?;
    let (feature, issues, file) = (segs.get(i + 1)?, segs.get(i + 2)?, segs.get(i + 3)?);
    let n: String = file.chars().take_while(char::is_ascii_digit).collect();
    (*issues == "issues" && !feature.is_empty() && !n.is_empty() && file.ends_with(".md"))
        .then(|| format!("{feature}#{n}"))
}

/// Keep the ticket on a release whose note dropped it.
///
/// A release note replaces the claim's text, and neither a hand-written note
/// nor one derived from commit subjects knows which ticket the work was for.
/// Prefixed, not appended, so the 140-character cap trims prose, not the ref.
fn carry_ticket(claim: &str, note: &str) -> String {
    match (ticket_ref(claim), ticket_ref(note)) {
        // `#88` is short and parses back; a Linear key alone would not, so
        // that one keeps its URL.
        (Some((key, link)), None) => match link {
            Some(u) if !key.starts_with('#') => format!("{u} {note}"),
            _ => format!("{key} {note}"),
        },
        _ => note.to_string(),
    }
}

/// One row per ticket in a repo: where it stands, who touched it last, and
/// the commits filed under it.
#[allow(clippy::too_many_arguments)]
fn ticket_rows(
    claims: &[&&Plan],
    happened: &[&Plan],
    r: &str,
    repo: &dyn Fn(&Plan) -> String,
    t: i64,
    shas: &dyn Fn(&Plan) -> String,
    agent: &dyn Fn(&Plan) -> String,
    tr: &tracker::Tracker,
) -> String {
    // (key, link, entries newest first). Claims first, so a live claim is the
    // row's newest word even when an older release shares its ticket.
    let mut rows: Vec<(String, Option<String>, Vec<&Plan>)> = Vec::new();
    let entries = claims
        .iter()
        .map(|p| **p)
        .chain(happened.iter().copied().filter(|p| repo(p) == r));
    for p in entries {
        let Some((key, link)) = tr.find(&p.task) else {
            continue;
        };
        match rows.iter_mut().find(|x| x.0 == key) {
            Some(x) => {
                x.1 = x.1.take().or(link);
                x.2.push(p);
            }
            None => rows.push((key, link, vec![p])),
        }
    }
    if rows.is_empty() {
        return String::new();
    }
    let body = rows
        .iter()
        .map(|(key, link, es)| {
            let head = es[0];
            let live = claims
                .iter()
                .find(|c| tr.find(&c.task).is_some_and(|x| x.0 == *key));
            let state = match (live, head.kind.as_str()) {
                (Some(c), _) if stuck(c, t) => r#"<span class="tag bad">quiet</span>"#,
                (Some(_), _) => r#"<span class="tag">working</span>"#,
                (None, "release") => r#"<span class="tag">released</span>"#,
                (None, "done") => r#"<span class="tag">done</span>"#,
                _ => r#"<span class="tag">noted</span>"#,
            };
            let name = match link {
                Some(u) => format!(r#"<a href="{}">{}</a>"#, esc(u), esc(key)),
                None => esc(key),
            };
            let mut all = head.clone();
            all.commits = Vec::new();
            for e in es {
                for c in &e.commits {
                    if !all.commits.contains(c) {
                        all.commits.push(c.clone());
                    }
                }
            }
            format!(
                "<tr><td>{name}</td><td>{state}</td><td>{} {}</td><td>{}</td><td>{}</td></tr>",
                esc(&head.task),
                shas(&all),
                agent(head),
                ago(t - head.epoch),
            )
        })
        .collect::<String>();
    format!(
        "<table class=tickets><tr><th>Ticket</th><th>Status</th><th>Latest</th><th>Agent</th><th>Updated</th></tr>{body}</table>"
    )
}

/// The page from what tests hand it: no spool, no tracker, nothing unscoped.
#[cfg(test)]
fn dashboard(
    live: &[Plan],
    posts: &[Plan],
    c: &Cfg,
    t: i64,
    links: Option<&(String, String)>,
    view: &View,
) -> String {
    let tr = tracker::Tracker::none();
    let b = dash::Board {
        live,
        posts,
        all_live: live,
        all_posts: posts,
        local: &[],
        events: &[],
        me: "",
        plan: None,
        tracker: &tr,
    };
    render(&b, c, t, links, view)
}

/// `links` is (project, commit url template): shas link only within that repo.
fn render(
    b: &dash::Board,
    c: &Cfg,
    t: i64,
    links: Option<&(String, String)>,
    view: &View,
) -> String {
    let (live, posts) = (b.live, b.posts);
    let tr = b.tracker;
    let here = project();
    // Every name on the page is a link to that agent's own page, at the same
    // moment — the byline and the path are the same string.
    let moment = view.moment();
    let agent_href = |p: &Plan| match p.instance.as_str() {
        "" => format!("/{}", name_for(&p.pubkey)),
        i => format!("/{}/{i}", name_for(&p.pubkey)),
    };
    let agent = |p: &Plan| {
        format!(
            r#"<a href="{}{moment}">{}</a>"#,
            esc(&agent_href(p)),
            esc(&who(p, true))
        )
    };
    let held = |p: &Plan| {
        t.saturating_sub(if p.claimed_at > 0 {
            p.claimed_at
        } else {
            p.epoch
        })
    };
    let shas = |p: &Plan| {
        p.commits
            .iter()
            .map(|s| match links {
                Some((repo, tpl)) if *repo == p.project => {
                    format!(r#"<a href="{}">{}</a>"#, esc(&commit_url(tpl, s)), esc(s))
                }
                _ => format!("<code>{}</code>", esc(s)),
            })
            .collect::<Vec<_>>()
            .join(" ")
    };
    let empty = |what: &str| format!(r#"<p class="none">{what}</p>"#);

    let claims: Vec<&Plan> = live
        .iter()
        .filter(|p| p.live(t) && !p.touching.is_empty())
        .collect();
    let repo = |p: &Plan| match p.project.as_str() {
        "" | "-" => "(no repo)".to_string(),
        r => r.to_string(),
    };
    // What happened, as opposed to traffic about it: claims are already in
    // the table and questions have their own panel.
    let happened: Vec<&Plan> = posts
        .iter()
        .filter(|p| matches!(p.kind.as_str(), "release" | "done" | ""))
        .collect();

    // One panel per repo, the way the record is already partitioned: every
    // entry carries the publisher's repo. Busiest-now first, then most recent.
    let mut projects: Vec<(bool, i64, String)> = Vec::new();
    for p in claims.iter().chain(happened.iter()) {
        let r = repo(p);
        let holding = claims.iter().any(|c| repo(c) == r);
        match projects.iter_mut().find(|x| x.2 == r) {
            Some(x) => x.1 = x.1.max(p.epoch),
            None => projects.push((holding, p.epoch, r)),
        }
    }
    projects.sort_by_key(|x| std::cmp::Reverse((x.0, x.1)));

    let project_panels = if projects.is_empty() {
        format!("<section>{}</section>", empty("Nothing on the record yet"))
    } else {
        projects
            .iter()
            .map(|(_, last, r)| {
                let mine: Vec<&&Plan> = claims.iter().filter(|p| repo(p) == *r).collect();
                // The plan already shows this repo's tickets, with who is on them.
                let tickets = if b.plan.is_some() && *r == here {
                    String::new()
                } else {
                    ticket_rows(&mine, &happened, r, &repo, t, &shas, &agent, tr)
                };
                // Anything filed under a ticket is summarised in its row; the
                // claims table and the recent list keep only the rest.
                let untagged: Vec<&&Plan> =
                    mine.iter().filter(|p| tr.find(&p.task).is_none()).copied().collect();
                let table = if untagged.is_empty() {
                    String::new()
                } else {
                    let rows = untagged
                        .iter()
                        .map(|p| {
                            let state = if stuck(p, t) {
                                r#"<span class="tag bad">quiet</span>"#
                            } else {
                                r#"<span class="tag">working</span>"#
                            };
                            format!(
                                "<tr><td>{state}</td><td>{}</td><td>{}</td><td><code>{}</code></td><td>{}</td></tr>",
                                agent(p),
                                esc(&p.task),
                                esc(&p.touching.join(" ")),
                                dur(held(p)),
                            )
                        })
                        .collect::<String>();
                    format!(
                        "<table><tr><th>Status</th><th>Agent</th><th>Task</th><th>Paths</th><th>Held</th></tr>{rows}</table>"
                    )
                };
                let recent = happened
                    .iter()
                    .filter(|p| repo(p) == *r && tr.find(&p.task).is_none())
                    .take(6)
                    .map(|p| {
                        // A note is prose somebody chose to write; a release
                        // is a headline with refs. Weight them differently.
                        let text = if p.kind.is_empty() {
                            esc(&p.task)
                        } else {
                            format!("<b>{}</b> {}", esc(&p.task), shas(p))
                        };
                        let paths = if p.globs.is_empty() {
                            String::new()
                        } else {
                            format!(" · <code>{}</code>", esc(&p.globs.join(" ")))
                        };
                        format!(
                            "<div class=item>{text}<div class=sub>{} · {}{paths}</div></div>",
                            agent(p),
                            ago(t - p.epoch),
                        )
                    })
                    .collect::<String>();
                let status = if mine.is_empty() {
                    format!("<span class=dim>last activity {}</span>", ago(t - last))
                } else {
                    format!("<span class=n>{} working</span>", mine.len())
                };
                format!(
                    "<section><div class=ph><h2 class=proj>{}</h2>{status}</div>{tickets}{table}{recent}</section>",
                    esc(r)
                )
            })
            .collect::<String>()
    };

    let quiet: Vec<&&Plan> = claims.iter().filter(|p| stuck(p, t)).collect();
    let stuck_panel = if quiet.is_empty() {
        r#"<p class="ok">Nothing stuck</p>"#.to_string()
    } else {
        quiet
            .iter()
            .map(|p| {
                format!(
                    "<div class=item><b>{}</b> in {} — silent {}, claim lapses in {}<div class=sub><code>{}</code> {}</div></div>",
                    agent(p),
                    esc(&repo(p)),
                    dur(t - p.epoch),
                    dur(p.epoch + p.eta_s * STALE_MULT - t),
                    esc(&p.touching.join(" ")),
                    esc(&p.task),
                )
            })
            .collect()
    };

    let asks = open_asks(posts, c);
    let asks_panel = if asks.is_empty() {
        empty("No open questions")
    } else {
        asks.iter()
            .map(|p| {
                let mine = if addressed_to_me(p, c) { r#"<span class="tag bad">for you</span> "# } else { "" };
                let to = if p.to.is_empty() { "the team".to_string() } else { esc(&p.to) };
                format!(
                    "<div class=item>{mine}<b>{}</b><div class=sub>{} → {to} · {} · {} · <code>robofinger answer --to {} --re {} \"…\"</code></div></div>",
                    esc(&p.task),
                    agent(p),
                    esc(&repo(p)),
                    ago(t - p.epoch),
                    esc(&p.alias),
                    p.id,
                )
            })
            .collect()
    };

    // The whole team around the agent being looked at, scoped broad to
    // narrow left to right: the project, the agents, then the one entity.
    let ns = dash::nodes(b, t);
    let team = dash::counts(&ns);
    let agents_panel = dash::tree(b, &ns, c, t, view);
    let feed_panel = dash::feed(b, t, view);
    let detail_panel = dash::detail(b, &ns, c, t, view);
    let plan_panel = b
        .plan
        .map(|g| dash::plan(g, b, &ns, t, view, &here))
        .unwrap_or_default();

    let count = |n: usize| {
        if n > 0 {
            format!(" <span class=n>{n}</span>")
        } else {
            String::new()
        }
    };
    // A page fixed at a moment must not refresh itself away from it. A live
    // one re-fetches itself; without script it falls back to reloading.
    let (refresh, live_js) = if view.at.is_none() {
        (
            r#"<noscript><meta http-equiv="refresh" content="15"></noscript>"#,
            dash::LIVE_JS,
        )
    } else {
        ("", "")
    };
    let when = match view.at {
        Some(_) => format!("as of {}", stamp(t)),
        None => format!("live · {}", stamp(t)),
    };
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
{refresh}<title>robofinger · {alias}</title>
<style>
:root{{--bg:#111;--panel:#1a1a1a;--line:#333;--fg:#ddd;--dim:#999;--hi:#f5a623;--ok:#3fb950;--bad:#f85149;--wait:#79c0ff}}
@media (prefers-color-scheme: light){{:root{{--bg:#f6f6f6;--panel:#fff;--line:#ddd;--fg:#222;--dim:#666;--hi:#b36b00;--ok:#1a7f37;--bad:#cf222e;--wait:#0969da}}}}
body{{background:var(--bg);color:var(--fg);font:14px/1.45 system-ui,sans-serif;margin:0;padding:16px}}
header{{border-bottom:2px solid var(--hi);padding-bottom:8px;margin-bottom:16px}}
h1{{color:var(--hi);font-size:20px;display:inline;margin-right:16px}}
section{{background:var(--panel);border:1px solid var(--line);border-radius:4px;padding:10px 12px;margin-bottom:12px;overflow-x:auto}}
h2{{font-size:12px;text-transform:uppercase;letter-spacing:.05em;margin:0 0 8px}} .n{{color:var(--hi)}}
table{{border-collapse:collapse;width:100%}} th,td{{text-align:left;padding:4px 8px;border-bottom:1px solid var(--line);vertical-align:top}} th{{color:var(--dim);font-weight:normal;font-size:12px}}
.tag{{border:1px solid var(--dim);border-radius:3px;padding:0 6px;font-size:12px;white-space:nowrap}} .tag.bad{{border-color:var(--bad);color:var(--bad)}}
.item{{padding:4px 0 8px}} .sub{{color:var(--dim);font-size:12px}} .none{{color:var(--dim);font-style:italic;margin:0}} .ok{{color:var(--ok);font-weight:bold;margin:0}}
code{{font-size:12px}} a{{color:var(--hi)}} .dim{{color:var(--dim)}}
nav{{font-size:12px;margin-top:6px}} nav.scrub b{{color:var(--hi)}}
.ph{{display:flex;justify-content:space-between;align-items:baseline;gap:8px;margin-bottom:6px}} h2.proj{{font-size:15px;text-transform:none;letter-spacing:0;color:var(--hi);margin:0}} .ph span{{font-size:12px}} table.tickets{{margin-bottom:10px}}
{css}
</style></head><body>
<header><h1>robofinger</h1><span class=dim id=summary>{alias} · {team} · {n} live claims across {np} repos · {when}</span>{nav}</header>
<div class=cols id=board>
<div class=col-l>
{plan_panel}
{project_panels}
</div>
<div class=col-m>
<section><h2>Agents{c_agents}</h2>{agents_panel}</section>
<section><h2>Activity</h2>{feed_panel}</section>
</div>
<div class=col-r>
{detail_panel}
<section><h2>Stuck{c_stuck}</h2>{stuck_panel}</section>
<section><h2>Open questions{c_asks}</h2>{asks_panel}</section>
</div>
</div>
{live_js}</body></html>"#,
        alias = esc(&c.alias),
        team = dash::summary(&team),
        css = dash::CSS,
        n = claims.len(),
        np = projects.len(),
        nav = view.nav(t),
        c_agents = count(ns.iter().filter(|n| dash::shown(n, t)).count()),
        c_stuck = count(quiet.len()),
        c_asks = count(asks.len()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_worktree_files_claims_under_the_main_checkout() {
        let main = project_name(Some("/src/robofinger/.git"), Some("/src/robofinger"));
        let tree = project_name(
            Some("/src/robofinger/.git"),
            Some("/src/robofinger/.claude/worktrees/agent-a1"),
        );
        assert_eq!((main.as_str(), tree.as_str()), ("robofinger", "robofinger"));
        // Submodule: the common dir is `.git/modules/<name>`, so the checkout names it.
        assert_eq!(
            project_name(Some("/src/app/.git/modules/lib"), Some("/src/app/lib")),
            "lib"
        );
        assert_eq!(project_name(None, None), "-");
    }

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
            commits: vec![],
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
        if !may_conflict(p, &format!("pk-{me}"), instance, here, now()) {
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
        assert!(
            worth_showing(&mine, "pk-me", "demo", t),
            "same project shows"
        );
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
        let name = derived_alias(&p.pubkey);
        assert_eq!(who(&p, true), format!("{name}/claude-2"));
        assert_eq!(who(&p, false), name, "solo output hides the instance");
        p.instance = String::new();
        assert_eq!(who(&p, true), name, "nothing to append");
    }

    /// Publishing a name again would hand every reader a string the publisher
    /// chose, which is the thing reader-side derivation exists to prevent.
    #[test]
    fn the_envelope_carries_no_name() {
        let mut p = plan("peer", "demo", &[], "working", 0);
        p.alias = "alice".into();
        let json = serde_json::to_string(&p).expect("serialise");
        assert!(!json.contains("\"agent\""), "no name field: {json}");
        assert!(!json.contains("alice"), "and nothing carrying one: {json}");

        // Entries published before v0.9.0 still parse, so history keeps
        // filtering under the name it was filed with.
        let old: Plan = serde_json::from_str(
            r#"{"agent":"mymac","pubkey":"pk","seq":1,"epoch":0,"status":"working",
                "task":"t","touching":[],"project":"demo","eta_s":1800}"#,
        )
        .expect("an old plan still parses");
        assert_eq!(old.alias, "mymac");
    }

    /// The alias travels inside the envelope, so without this a peer renames
    /// itself to "alice" and is rendered as alice — on a timeline whose only
    /// job is saying who did what.
    #[test]
    fn a_published_alias_never_names_its_publisher() {
        let mut p = plan("peer", "demo", &[], "working", 0);
        p.alias = "alice".into();

        // The published alias is ignored outright: the key names them.
        assert_eq!(name_for(&p.pubkey), derived_alias(&p.pubkey));
        assert_ne!(name_for(&p.pubkey), "alice");

        let empty = std::collections::HashMap::new();
        let subs = vec![Peer {
            label: "sam".into(),
            pubkey: p.pubkey.clone(),
            age_pub: String::new(),
            home: None,
            groups: vec![],
        }];

        // A local name accompanies the derived one rather than replacing it,
        // so the fingerprint never leaves the line.
        assert_eq!(label_in(&p.pubkey, &empty, &subs).as_deref(), Some("sam"));
        assert_eq!(label_in("pk-other", &empty, &subs), None);

        // The names file wins over a peer label, and reaches keys the peer
        // list cannot — your own included.
        let names = std::collections::HashMap::from([
            (p.pubkey.clone(), "cachy-g14".to_string()),
            ("pk-mine".to_string(), "desktop".to_string()),
        ]);
        assert_eq!(
            label_in(&p.pubkey, &names, &subs).as_deref(),
            Some("cachy-g14")
        );
        assert_eq!(label_in("pk-mine", &names, &[]).as_deref(), Some("desktop"));
    }

    /// Slots must be stable per session (the hook process has to land on the
    /// same name as the agent that took the claim) and distinct between them.
    #[test]
    fn slots_are_stable_per_session_and_reused_once_free() {
        let d = std::env::temp_dir().join(format!("rf-slots-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let t = 1_000_000;

        let a = slot_in(&d, "claude", "session-a", t);
        assert_eq!(
            slot_in(&d, "claude", "session-a", t),
            a,
            "same session keeps its name"
        );
        let b = slot_in(&d, "claude", "session-b", t);
        assert_ne!(a, b, "different sessions differ");
        assert_eq!((a.as_str(), b.as_str()), ("claude-1", "claude-2"));

        // Families number independently, so a terminal-launched agent on a
        // machine already running Claude Code is agent-1, not agent-3.
        assert_eq!(slot_in(&d, "agent", "session-d", t), "agent-1");

        // An expired reservation frees its number for the next new session.
        std::fs::write(d.join("instances"), format!("session-a\tclaude-1\t{t}\n")).unwrap();
        assert_eq!(
            slot_in(&d, "claude", "session-c", t + SLOT_TTL + 1),
            "claude-1",
            "stale slot is reclaimed"
        );

        std::fs::remove_dir_all(&d).ok();
    }

    /// A subagent's name nests under its parent's, so the record reads as a
    /// tree, and it is a function of the tool's id — the edit check and the
    /// claim each work it out separately and have to land on the same name.
    #[test]
    fn subagents_nest_under_their_parent_and_number_per_parent() {
        let d = std::env::temp_dir().join(format!("rf-sub-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let t = 1_000_000;

        let a = subagent_instance_in(&d, "claude-1", "Explore", "a8c0d38b", t);
        let b = subagent_instance_in(&d, "claude-1", "Explore", "a45eda7d", t);
        assert_eq!((a.as_str(), b.as_str()), ("claude-1/explore-1", "claude-1/explore-2"));
        assert_eq!(
            subagent_instance_in(&d, "claude-1", "Explore", "a8c0d38b", t + 60),
            a,
            "the same subagent keeps its name from one hook to the next"
        );
        assert_eq!(
            subagent_instance_in(&d, "claude-2", "Explore", "c838c838", t),
            "claude-2/explore-1",
            "numbering is per parent, so one session's swarm does not shift another's"
        );
        assert_eq!(
            subagent_instance_in(&d, "claude-1", "general-purpose", "d01dd01d", t),
            "claude-1/general-purpose-1"
        );

        std::fs::remove_dir_all(&d).ok();
    }

    /// The type comes from the tool and lands in an instance (a relay storage
    /// key, capped at 64) and in a shell command the hook rewrites, so it is
    /// reduced to something safe in both.
    #[test]
    fn agent_kind_is_safe_in_a_name_and_a_shell() {
        assert_eq!(agent_kind("Explore"), "explore");
        assert_eq!(agent_kind("my-plugin:reviewer"), "reviewer", "plugin scope dropped");
        assert_eq!(agent_kind("rm -rf /; echo"), "rm-rf-echo");
        assert_eq!(agent_kind(""), "agent");
        assert_eq!(agent_kind("$(whoami)"), "whoami");
        assert!(agent_kind(&"x".repeat(200)).len() <= 20);
    }

    /// A parent's claim covers the work it hands down, so a parent and its own
    /// subagents never warn each other. Siblings do: parallel subagents in one
    /// session are the collision a swarm makes most likely.
    #[test]
    fn a_parent_and_its_subagents_never_conflict_but_siblings_do() {
        let claim_as = |instance: &str| {
            let mut p = plan("me", "demo", &["src/auth/**"], "working", 0);
            p.instance = instance.into();
            p
        };
        let f = "src/auth/session.ts";
        assert!(!matches_as(&claim_as("claude-1"), f, "demo", "me", "claude-1/explore-1"));
        assert!(!matches_as(&claim_as("claude-1/explore-1"), f, "demo", "me", "claude-1"));
        assert!(
            matches_as(&claim_as("claude-1/explore-1"), f, "demo", "me", "claude-1/explore-2"),
            "siblings conflict"
        );
        assert!(
            matches_as(&claim_as("claude-1/explore-1"), f, "demo", "me", "claude-2"),
            "another session's subagent is just another agent"
        );
        assert!(
            matches_as(&claim_as("claude-10"), f, "demo", "me", "claude-1/explore-1"),
            "a name that merely starts the same is not a parent"
        );
        let mut peer = claim_as("claude-1");
        peer.pubkey = "pk-peer".into();
        assert!(
            matches_as(&peer, f, "demo", "me", "claude-1/explore-1"),
            "the same slot name on someone else's key is no relation"
        );
    }

    /// The hook tags each robofinger invocation a subagent runs, after the
    /// subcommand — anything in front of `robofinger` stops a
    /// `Bash(robofinger:*)` allow rule matching, and the subagent's call is
    /// then refused. Only a command-position `robofinger` is touched: a path
    /// or an argument containing the word is left alone.
    #[test]
    fn the_rewrite_tags_only_robofinger_at_command_position() {
        let tag = "explore:a8c0";
        let r = |c: &str| tag_robofinger(c, tag);
        assert_eq!(
            r("robofinger claim \"write a\" a.txt").as_deref(),
            Some("robofinger claim --subagent explore:a8c0 \"write a\" a.txt")
        );
        assert_eq!(r("robofinger release").as_deref(), Some("robofinger release --subagent explore:a8c0"));
        assert_eq!(
            r("cd src && robofinger claim x 'a/**' && robofinger release").as_deref(),
            Some("cd src && robofinger claim --subagent explore:a8c0 x 'a/**' && robofinger release --subagent explore:a8c0")
        );
        assert_eq!(
            r("~/.cargo/bin/robofinger check a.rs").as_deref(),
            Some("~/.cargo/bin/robofinger check --subagent explore:a8c0 a.rs")
        );
        assert_eq!(r("robofinger").as_deref(), Some("robofinger --subagent explore:a8c0"));
        assert_eq!(
            r("robofinger --version").as_deref(),
            Some("robofinger --subagent explore:a8c0 --version")
        );
        assert_eq!(r("cat ~/github/robofinger/README.md"), None, "a path");
        assert_eq!(r("grep robofinger notes.txt"), None, "an argument");
        assert_eq!(r("echo \"then robofinger claim\""), None, "inside quotes");
        assert_eq!(r("cargo build -p robofinger-client"), None);
        assert_eq!(r("ls"), None);
        assert_eq!(
            r("robofinger claim --subagent explore:a8c0 x a"),
            None,
            "already tagged"
        );
    }

    /// The flag can sit anywhere the hook put it, and must be gone before any
    /// command reads its arguments — `claim` would otherwise publish it as a
    /// glob.
    #[test]
    fn the_subagent_flag_is_lifted_out_wherever_it_sits() {
        let mut a: Vec<String> = ["claim", "--subagent", "explore:a8c0", "task", "a/**"]
            .map(String::from)
            .to_vec();
        assert_eq!(take_flag(&mut a, "--subagent").as_deref(), Some("explore:a8c0"));
        assert_eq!(a, ["claim", "task", "a/**"]);

        let mut none: Vec<String> = ["release"].map(String::from).to_vec();
        assert_eq!(take_flag(&mut none, "--subagent"), None);
        assert_eq!(none, ["release"]);
    }

    /// Hook input names the subagent with the tool's own id and type. Absent
    /// in the main conversation, which keeps its session name. The id is
    /// spliced into a shell command, so anything but a plain token is refused
    /// rather than quoted.
    #[test]
    fn subagent_identity_comes_from_hook_input() {
        let v = serde_json::json!({"agent_id": "a8c0d38b4542dc838", "agent_type": "my-plugin:Reviewer"});
        assert_eq!(
            hook_subagent(&v),
            Some(("reviewer".into(), "a8c0d38b4542dc838".into()))
        );
        assert_eq!(hook_subagent(&serde_json::json!({"session_id": "x"})), None, "main agent");
        assert_eq!(hook_subagent(&serde_json::json!({"agent_id": ""})), None);
        assert_eq!(
            hook_subagent(&serde_json::json!({"agent_id": "a; rm -rf ~"})),
            None,
            "not a plain token"
        );
        assert_eq!(
            hook_subagent(&serde_json::json!({"agent_id": "abc"})),
            Some(("agent".into(), "abc".into())),
            "no type still names the subagent"
        );
        assert_eq!(
            parse_subagent("reviewer:a8c0d38b4542dc838"),
            Some(("reviewer".into(), "a8c0d38b4542dc838".into())),
            "the flag round-trips what the hook wrote"
        );
        assert_eq!(parse_subagent("a8c0"), None);
    }

    /// The derived name must be a pure function of the key and never move: it
    /// is published, and peers file their own labels against it.
    #[test]
    fn derived_alias_is_stable_and_key_dependent() {
        let a = derived_alias("fHC-SO9S_nQ4aWnyFVJdFhBqLh1nZPqwJHFPNGOZbnE");
        assert_eq!(
            a,
            derived_alias("fHC-SO9S_nQ4aWnyFVJdFhBqLh1nZPqwJHFPNGOZbnE")
        );
        assert_ne!(
            a,
            derived_alias("3KR2vzooEqN1mYyWQ8dPl0tXsA7bCfUgHjKmNpQrStU")
        );
        let (adj, noun) = a.split_once('-').expect("two words joined by a hyphen");
        assert!(!adj.is_empty() && !noun.is_empty());
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

    /// The layering is the whole feature: a project file that a team commits,
    /// a local file that one person overrides it with, and a global file that
    /// still supplies whatever neither mentions.
    #[test]
    fn repo_config_layers_over_global() {
        let dir = std::env::temp_dir().join(format!("rf-cfg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let w = |name: &str, body: &str| std::fs::write(dir.join(name), body).unwrap();
        w(
            "global",
            "ROBOFINGER_URL=https://global\nROBOFINGER_ALIAS=macbook\n",
        );
        w(
            ".robofinger",
            "ROBOFINGER_URL=https://project\nROBOFINGER_COMMIT_URL=https://forge/{sha}\n",
        );
        w(".robofinger.local", "ROBOFINGER_URL=https://mine\n");

        let mut merged = parse_config(&dir.join("global"));
        for f in [".robofinger", ".robofinger.local"] {
            merged.extend(parse_config(&dir.join(f)));
        }

        assert_eq!(merged["ROBOFINGER_URL"], "https://mine", ".local wins");
        assert_eq!(
            merged["ROBOFINGER_COMMIT_URL"], "https://forge/{sha}",
            "project layer supplies what .local omits"
        );
        assert_eq!(
            merged["ROBOFINGER_ALIAS"], "macbook",
            "global survives when no repo layer mentions it"
        );

        // A missing layer is empty, not an error.
        let only_global = parse_config(&dir.join("global"));
        assert_eq!(only_global.len(), 2);
        assert!(parse_config(&dir.join("nope")).is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A remote is user input from `git config`, and the result ends up in a
    /// rendered link — so the parser is the part worth pinning down.
    #[test]
    fn remotes_normalize_or_are_refused() {
        let ok = |raw: &str| normalize_remote(raw).expect(raw);
        assert_eq!(ok("https://github.com/o/r.git"), "https://github.com/o/r");
        assert_eq!(ok("https://github.com/o/r"), "https://github.com/o/r");
        assert_eq!(ok("git@github.com:o/r.git"), "https://github.com/o/r");
        assert_eq!(ok("ssh://git@gitlab.com/o/r.git"), "https://gitlab.com/o/r");
        assert_eq!(
            ok("git@git.acme.dev:team/sub/r.git"),
            "https://git.acme.dev/team/sub/r"
        );
        // Trailing slash, and a bare .git in the repo name.
        assert_eq!(ok("https://github.com/o/r.git/"), "https://github.com/o/r");

        // Credentials must never reach a link, and a local path is not a forge.
        assert!(normalize_remote("https://user:pw@github.com/o/r.git").is_none());
        assert!(normalize_remote("/srv/git/r.git").is_none());
        assert!(normalize_remote("https://github.com").is_none());
        assert!(normalize_remote("").is_none());
    }

    /// The guess this deliberately does not make: an unknown host gets no
    /// link, because a plausible 404 is indistinguishable from a rebased
    /// commit and the user cannot tell the tool got it wrong.
    #[test]
    fn unknown_forges_get_no_link() {
        let seg = |base: &str| {
            let host = base
                .split('/')
                .nth(2)
                .unwrap_or_default()
                .to_ascii_lowercase();
            match host.as_str() {
                h if h.ends_with("github.com") => Some("/commit/"),
                h if h.ends_with("gitlab.com") => Some("/-/commit/"),
                h if h.ends_with("bitbucket.org") => Some("/commits/"),
                _ => None,
            }
        };
        assert_eq!(seg("https://github.com/o/r"), Some("/commit/"));
        assert_eq!(seg("https://gitlab.com/o/r"), Some("/-/commit/"));
        assert_eq!(seg("https://bitbucket.org/o/r"), Some("/commits/"));
        assert_eq!(seg("https://git.acme.internal/o/r"), None, "no guess");
        assert_eq!(seg("https://codeberg.org/o/r"), None, "no guess");
    }

    #[test]
    fn commit_url_fills_every_placeholder() {
        assert_eq!(
            commit_url("https://github.com/o/r/commit/{sha}", "abc1234"),
            "https://github.com/o/r/commit/abc1234"
        );
    }

    /// SHAs ride in their own field so a truncated note still leaves the work
    /// findable — the whole reason `commits` is not folded into the text.
    #[test]
    fn commits_survive_a_truncated_note() {
        let mut p = plan("me", "demo", &[], "post", 0);
        p.kind = "release".into();
        p.task = "x".repeat(MAX_ENTRY + 40);
        p.commits = vec!["a1b2c3d".into(), "e4f5a6b".into()];

        let entry = Plan {
            task: truncate(&p.task, MAX_ENTRY),
            ..p.clone()
        };
        let out = render_entry(&entry, false);
        assert!(
            entry.task.chars().count() <= MAX_ENTRY + 1,
            "note is capped"
        );
        assert!(out.contains("a1b2c3d"), "first sha survives: {out}");
        assert!(out.contains("e4f5a6b"), "second sha survives: {out}");

        // And an entry with no commits gains no brackets.
        let bare = Plan {
            commits: vec![],
            ..entry
        };
        assert!(!render_entry(&bare, false).contains('['));
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

        // Check each step. Discarding git's status meant a fixture that failed
        // to build looked exactly like a `git log` that found nothing, which
        // is how this test spent two CI runs reporting `got ""` on Windows
        // without saying which half was broken.
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .current_dir(&dir)
                .args(args)
                .output()
                .expect("git must be on PATH");
            assert!(
                out.status.success(),
                "git {args:?} failed: {}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        };
        // `init.defaultBranch` is unset on the CI runners, and on Windows the
        // global template can leave core.autocrlf on, which rewrites the
        // fixture files. Neither affects what this test asserts, but both emit
        // noise that used to be swallowed.
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "T"]);
        git(&["config", "commit.gpgsign", "false"]);

        std::fs::write(dir.join("src/a.rs"), "1").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "fix the retry backoff"]);
        std::fs::write(dir.join("docs/x.md"), "1").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "unrelated doc edit"]);

        // Against the fixture directly. Setting the process cwd would race
        // every other test in the binary, and on Windows the temp dir is
        // reached by a path `git rev-parse --show-toplevel` does not agree
        // with — so `repo_root()` pointed somewhere with no commits and the
        // log came back empty.
        let scoped = commits_since_in(&dir, 0, &["src/*".to_string()]);
        let all = commits_since_in(&dir, 0, &[]);
        // A claim released before any commit lands has nothing to say, and
        // must not borrow an older commit as its note.
        let future = commits_since_in(&dir, now() + 3600, &["src/*".to_string()]);

        assert!(
            scoped.contains("retry backoff"),
            "the claimed path's commit must be in the note, got {scoped:?}"
        );
        assert!(
            !scoped.contains("unrelated"),
            "globs must scope the log to the claimed paths, got {scoped:?}"
        );
        assert!(
            all.contains("unrelated"),
            "no globs means the whole claim, got {all:?}"
        );
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

    /// The dashboard's two judgments: who has gone quiet, and which questions
    /// are still waiting. Plus escaping, since task text is whatever a peer
    /// typed and the page renders it.
    #[test]
    fn dashboard_flags_quiet_claims_and_open_questions() {
        let t = now();
        let fresh = plan("a", "repo", &["src/**"], "working", 60);
        let quiet = plan("b", "repo", &["lib/**"], "working", 2000);
        let lapsed = plan("c", "repo", &["x/**"], "working", 4000);
        assert!(!stuck(&fresh, t));
        assert!(stuck(&quiet, t));
        assert!(!stuck(&lapsed, t), "an expired claim is gone, not stuck");

        let cfg = Cfg {
            url: "https://relay.example/plan".into(),
            alias: "me".into(),
            instance: String::new(),
        };
        let ask = |id: i64, to: &str| {
            let mut p = plan("b", "repo", &[], "post", 0);
            p.kind = "ask".into();
            p.id = id;
            p.to = to.into();
            p.task = "<script>x</script>".into();
            p
        };
        let mut reply = plan("a", "repo", &[], "post", 0);
        reply.kind = "answer".into();
        reply.reply_to = 1;
        let posts = vec![ask(1, ""), ask(2, ""), ask(3, "me"), reply];
        let open: Vec<i64> = open_asks(&posts, &cfg).iter().map(|p| p.id).collect();
        assert_eq!(open, vec![3, 2], "answered dropped, addressed first");

        // A release from a repo nobody is working in now: its own panel,
        // ranked below the repo with live claims even though it is newer.
        let mut shipped = plan("d", "other", &[], "post", 0);
        shipped.kind = "release".into();
        shipped.task = "shipped the thing".into();
        let mut posts = posts;
        posts.insert(0, shipped);

        let html = dashboard(&[fresh, quiet], &posts, &cfg, t, None, &View::default());
        let at = |r: &str| html.find(&format!("<h2 class=proj>{r}</h2>")).unwrap();
        assert!(at("repo") < at("other"), "busy repos first");
        assert!(html.contains("2 working"));
        assert!(!html.contains("<script>x"), "task text is escaped");
        assert!(html.contains("&lt;script&gt;"));
        assert!(html.contains("for you"));
    }

    /// The ticket is how the dashboard groups work, so a missed ref drops a
    /// row and a false one invents a ticket.
    #[test]
    fn tickets_are_found_where_trackers_put_them() {
        let key = |s: &str| ticket_ref(s).map(|x| x.0);
        assert_eq!(key("Admin Users tab (#88)"), Some("#88".into()));
        assert_eq!(key("#88: users tab"), Some("#88".into()));
        assert_eq!(
            key("see https://github.com/o/r/issues/88."),
            Some("#88".into())
        );
        assert_eq!(key("https://gitlab.com/o/r/-/issues/7"), Some("#7".into()));
        assert_eq!(
            key("https://linear.app/t/issue/ENG-12/slug"),
            Some("ENG-12".into())
        );
        assert_eq!(
            key("ADR-015: one shared product"),
            None,
            "an ADR is not a ticket"
        );
        assert_eq!(key("C# rewrite, item #"), None);
        assert_eq!(key("https://github.com/o/r/pull/9"), None);
        assert_eq!(
            key("https://acme.atlassian.net/browse/ENG-12"),
            Some("ENG-12".into())
        );
        assert_eq!(
            key("retry (.scratch/http/issues/03-retry.md)"),
            Some("http#03".into())
        );
        assert_eq!(
            key("/src/app/.scratch/http/issues/03-retry.md"),
            Some("http#03".into()),
            "absolute path from a hook, same ticket"
        );
        assert_eq!(key(".scratch/http/map.md"), None, "a map is not a ticket");
        assert_eq!(key(".scratch/http/spec.md"), None);

        assert_eq!(carry_ticket("users tab #88", "landed"), "#88 landed");
        assert_eq!(carry_ticket("users tab #88", "#88 landed"), "#88 landed");
        assert_eq!(carry_ticket("no ticket", "landed"), "landed");
        let lin = "https://linear.app/t/issue/ENG-12/x";
        assert_eq!(
            carry_ticket(&format!("fix {lin}"), "done"),
            format!("{lin} done")
        );
    }

    #[test]
    fn a_forge_url_keeps_its_link_whatever_its_path_says() {
        let u = "https://github.com/scratch/app/issues/12";
        assert_eq!(ticket_ref(u), Some(("#12".into(), Some(u.into()))));
    }

    #[test]
    fn only_the_local_trackers_own_layout_is_a_ticket() {
        let key = |s: &str| ticket_ref(s).map(|x| x.0);
        assert_eq!(key("notes/scratch/foo/issues/7-x.md"), None, "scratch is not .scratch");
        assert_eq!(key(".scratch/foo/issues/07-x.txt"), None, "a ticket is a .md file");
        assert_eq!(key("(./.scratch/foo/issues/07-x.md)"), Some("foo#07".into()));
    }

    #[test]
    fn a_browsed_file_is_not_a_ticket() {
        let bitbucket = "https://git.acme.com/projects/P/repos/r/browse/src/main.rs";
        assert_eq!(ticket_ref(bitbucket), None);
    }

    #[test]
    fn a_ticket_url_keys_the_same_with_a_query_or_anchor() {
        let key = |s: &str| ticket_ref(s).map(|x| x.0);
        assert_eq!(
            key("https://acme.atlassian.net/browse/ENG-12?focusedCommentId=5"),
            Some("ENG-12".into())
        );
        assert_eq!(
            key("https://github.com/o/r/issues/88#issuecomment-1"),
            Some("#88".into())
        );
    }

    #[test]
    fn a_quiet_session_start_still_says_to_claim() {
        let quiet = start_context(vec![], "hazel-hare/claude-1");
        assert!(quiet.contains("robofinger claim"), "{quiet}");
        assert!(quiet.contains("ROBOFINGER.md"), "{quiet}");
        assert!(!quiet.contains("[FOR YOU]"), "nothing to answer, so no answering rules");
        assert!(quiet.contains("you direct them"), "the parent is the subagents' line to the team");
        assert!(SUBAGENT_RULE.contains("report to your parent only"));

        let busy = start_context(
            vec!["OPEN QUESTIONS from your teammates".into()],
            "hazel-hare/claude-1",
        );
        assert!(busy.starts_with("OPEN QUESTIONS"), "{busy}");
        assert!(busy.contains("You are `hazel-hare/claude-1`"), "{busy}");
        assert!(busy.contains("robofinger claim") && busy.contains("[FOR YOU]"), "{busy}");
    }

    /// A swarm's subagents start in the same instant. Each must keep the
    /// name it was given, and no two may share one.
    #[test]
    fn subagents_starting_together_get_distinct_names_that_stick() {
        let dir = std::env::temp_dir().join(format!("rf-swarm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let t = 1_000_000;
        let names: Vec<String> = std::thread::scope(|s| {
            let hs: Vec<_> = (0..16)
                .map(|i| {
                    let dir = &dir;
                    s.spawn(move || subagent_instance_in(dir, "claude-1", "Explore", &format!("id{i}"), t))
                })
                .collect();
            hs.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let unique: std::collections::HashSet<&String> = names.iter().collect();
        assert_eq!(unique.len(), 16, "no two share a name: {names:?}");
        for (i, n) in names.iter().enumerate() {
            assert_eq!(&subagent_instance_in(&dir, "claude-1", "Explore", &format!("id{i}"), t), n, "kept its name");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_subagent_is_told_its_path_and_its_parent() {
        let line = you_are("hazel-hare/claude-1/explore-2", Some("hazel-hare/claude-1"));
        assert!(
            line.starts_with("- You are `hazel-hare/claude-1/explore-2`, a subagent of `hazel-hare/claude-1`"),
            "{line}"
        );
        assert!(!you_are("hazel-hare/claude-1", None).contains("subagent"));
    }

    #[test]
    fn an_agent_path_is_the_derived_name_then_the_instance() {
        let key = "fHC-SO9SYAjaB29D8DIRTxTNF2LaicTKk046FtGSIkk";
        assert_eq!(agent_path(key, ""), name_for(key));
        assert_eq!(agent_path(key, "claude-1/explore-2"), format!("{}/claude-1/explore-2", name_for(key)));
    }

    #[test]
    fn dashboard_rolls_ticket_work_into_one_row() {
        let t = now();
        let claim = plan("a", "repo", &["src/**"], "working", 60);
        let mut claim = claim;
        claim.task = "users tab #88".into();
        let mut earlier = plan("b", "repo", &[], "post", 600);
        earlier.kind = "release".into();
        earlier.task = "#88 schema landed".into();
        earlier.commits = vec!["abc1234".into()];
        let mut loose = plan("b", "repo", &[], "post", 700);
        loose.kind = "release".into();
        loose.task = "unrelated cleanup".into();
        let cfg = Cfg {
            url: "https://relay.example/plan".into(),
            alias: "me".into(),
            instance: String::new(),
        };
        let html = dashboard(&[claim], &[earlier, loose], &cfg, t, None, &View::default());
        assert_eq!(
            html.matches("<td>#88</td>").count(),
            1,
            "one row per ticket"
        );
        assert!(
            html.contains("abc1234"),
            "commits roll up from earlier releases"
        );
        let row = &html[html.find("<td>#88</td>").unwrap()..];
        let row = &row[..row.find("</tr>").unwrap()];
        assert!(
            !row.contains("schema landed"),
            "the live claim is the latest word in its row"
        );
        assert!(
            html.contains("unrelated cleanup"),
            "untagged work still listed"
        );
    }
    /// A timeline entry by agent `instance` of key `pk-me`, at `epoch`.
    fn entry(instance: &str, kind: &str, globs: &[&str], epoch: i64) -> Plan {
        let mut p = plan("me", "demo", &[], "post", 0);
        p.instance = instance.into();
        p.kind = kind.into();
        p.globs = globs.iter().map(|s| s.to_string()).collect();
        p.task = format!("{kind} by {instance}");
        p.epoch = epoch;
        p
    }

    /// The dashboard path is the byline the record already prints:
    /// who, then the agent's own path, so a subagent nests under its parent.
    #[test]
    fn a_dashboard_path_names_who_and_which_agent() {
        let r = parse_route("/").unwrap();
        assert_eq!((r.who, r.agent.as_str(), r.at), (None, "", None));

        let r = parse_route("/wheat-falcon").unwrap();
        assert_eq!((r.who.as_deref(), r.agent.as_str()), (Some("wheat-falcon"), ""));

        let r = parse_route("/wheat-falcon/claude-1/explore-2?at=1792893051").unwrap();
        assert_eq!(r.who.as_deref(), Some("wheat-falcon"));
        assert_eq!(r.agent, "claude-1/explore-2");
        assert_eq!(r.at.as_deref(), Some("1792893051"));

        let r = parse_route("/wheat-falcon/claude-1/?at=2h&x=1").unwrap();
        assert_eq!((r.agent.as_str(), r.at.as_deref()), ("claude-1", Some("2h")), "trailing slash, other params");

        let r = parse_route("/wheat-falcon?at=2026-10-24+18%3A51").unwrap();
        assert_eq!(r.at.as_deref(), Some("2026-10-24 18:51"), "a form's `+` is a space");
        let r = parse_route("/wheat-falcon?at=2026-10-24%2018:51").unwrap();
        assert_eq!(r.at.as_deref(), Some("2026-10-24 18:51"), "so is `%20`");

        assert!(parse_route("/favicon.ico").is_none(), "costs a relay round trip for nothing");
        assert!(parse_route("/<script>").is_none());
        assert!(parse_route("/a/../b").is_none());
        assert!(parse_route("no-slash").is_none());
    }

    /// A path selects an agent and everything below it, never a sibling, and
    /// never an agent that merely starts with the same characters.
    #[test]
    fn a_path_selects_an_agent_and_its_subagents() {
        let me = name_for("pk-me");
        let at = |instance: &str| {
            let mut p = plan("me", "demo", &["a"], "working", 0);
            p.instance = instance.into();
            p
        };
        let parent = format!("{me}/claude-1");
        assert!(peer_matches(&at("claude-1"), Some(&parent), &[]));
        assert!(peer_matches(&at("claude-1/explore-2"), Some(&parent), &[]));
        assert!(!peer_matches(&at("claude-10"), Some(&parent), &[]));
        assert!(!peer_matches(&at("claude-2"), Some(&parent), &[]));
        assert!(
            !peer_matches(&at("claude-1/explore-1"), Some(&format!("{me}/claude-1/explore-2")), &[]),
            "a sibling"
        );
        assert!(peer_matches(&at("claude-2"), Some(&me), &[]), "a bare name is everyone on that key");
        assert!(!peer_matches(&at("claude-1"), Some("someone-else/claude-1"), &[]));
    }

    /// The past is replayed from the timeline: a claim holds until its
    /// release, a later claim replaces it, and one that goes quiet for longer
    /// than a claim lives fades — the timeline records changes, not heartbeats.
    #[test]
    fn claims_at_a_moment_are_replayed_from_the_timeline() {
        let held = |posts: &[Plan], t: i64| -> Vec<(String, Vec<String>)> {
            let mut v: Vec<_> = claims_at(posts, t)
                .into_iter()
                .map(|p| (p.instance, p.touching))
                .collect();
            v.sort();
            v
        };
        let t0 = 1_000_000;
        let posts = vec![
            entry("claude-1", "claim", &["src/**"], t0),
            entry("claude-1/explore-1", "claim", &["a.txt"], t0 + 10),
            entry("claude-1/explore-1", "release", &["a.txt"], t0 + 70),
            entry("claude-1", "claim", &["lib/**"], t0 + 100),
        ];
        assert_eq!(held(&posts, t0 - 1), vec![], "before anything");
        assert_eq!(
            held(&posts, t0 + 20),
            vec![
                ("claude-1".into(), vec!["src/**".into()]),
                ("claude-1/explore-1".into(), vec!["a.txt".into()]),
            ]
        );
        assert_eq!(held(&posts, t0 + 80), vec![("claude-1".into(), vec!["src/**".into()])], "released");
        assert_eq!(held(&posts, t0 + 100), vec![("claude-1".into(), vec!["lib/**".into()])], "replaced");

        let lapse = DEFAULT_ETA * STALE_MULT;
        assert_eq!(held(&posts, t0 + 100 + lapse + 1), vec![], "quiet past a claim's life");
        let mut busy = posts.clone();
        busy.push(entry("claude-1", "", &[], t0 + 100 + lapse - 10));
        assert_eq!(
            held(&busy, t0 + 100 + lapse + 1).len(),
            1,
            "any entry from the agent says it is still there"
        );
        let mut done = posts.clone();
        done.push(entry("claude-1", "done", &[], t0 + 200));
        assert_eq!(held(&done, t0 + 201), vec![], "done drops it too");
    }

    /// `?at=` takes what a person types or a link carries.
    #[test]
    fn at_takes_an_epoch_a_duration_or_a_local_time() {
        let now = 1_792_893_051;
        assert_eq!(parse_at("1792800000", now), Ok(1_792_800_000));
        assert_eq!(parse_at("2h", now), Ok(now - 7200));
        let t = 1_792_890_000;
        let typed = stamp(t).replace(' ', "T");
        assert_eq!(parse_at(&typed, now), Ok(t - t % 60), "a stamp, typed back, lands on its minute");
        assert!(parse_at("yesterday", now).is_err());
        assert!(parse_at("2026-13-01T00:00", now).is_err());
    }

    /// Scrubbing steps from event to event, not by a fixed interval: the
    /// moments worth looking at are the ones where something changed.
    #[test]
    fn scrubbing_steps_to_the_events_either_side() {
        let e = [100, 200, 300];
        assert_eq!(neighbours(&e, 200), (Some(100), Some(300)));
        assert_eq!(neighbours(&e, 250), (Some(200), Some(300)), "between events");
        assert_eq!(neighbours(&e, 100), (None, Some(200)));
        assert_eq!(neighbours(&e, 300), (Some(200), None));
        assert_eq!(neighbours(&[], 5), (None, None));
    }

    /// A page fixed at a moment does not refresh itself away from it, its
    /// agent names link to their own pages at the same moment, and it can
    /// step to the events either side.
    #[test]
    fn a_dashboard_at_a_moment_links_agents_and_steps_through_events() {
        let cfg = Cfg {
            instance: "claude-1".into(),
            url: String::new(),
            alias: "me".into(),
        };
        let t = 1_000_500;
        let mut claim = plan("me", "demo", &["src/**"], "working", 0);
        claim.instance = "claude-1/explore-2".into();
        claim.epoch = t - 100;
        let view = View {
            who: Some(name_for("pk-me")),
            agent: "claude-1".into(),
            at: Some(t),
            prev: Some(t - 100),
            next: Some(t + 60),
            oldest: Some(t - 100),
        };
        let html = dashboard(&[claim], &[], &cfg, t, None, &view);
        let me = name_for("pk-me");
        assert!(!html.contains("http-equiv=\"refresh\""), "a fixed moment stays put");
        assert!(
            html.contains(&format!("href=\"/{me}/claude-1/explore-2?at={t}\"")),
            "the subagent links to its own page at this moment"
        );
        assert!(html.contains(&format!("href=\"/{me}/claude-1?at={}\"", t - 100)), "previous event");
        assert!(html.contains(&format!("href=\"/{me}/claude-1?at={}\"", t + 60)), "next event");
        assert!(html.contains(&format!("href=\"/{me}/claude-1\"")), "back to now");
        assert!(
            html.contains(&format!("href=\"/{me}?at={t}\"")),
            "the breadcrumb climbs to the person, staying at this moment"
        );

        let live = dashboard(&[], &[], &cfg, t, None, &View::default());
        assert!(live.contains("http-equiv=\"refresh\""), "the live page still refreshes");
    }
}
