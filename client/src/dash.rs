//! The `serve` page: the roadmap with who is on each item, the agent tree
//! with a status line per agent, the activity feed, the selected agent, and
//! a Time Machine rail down the right edge for stepping back through it.
//!
//! Scoped left to right, broad to narrow: the project, the agents, then the
//! one entity the URL names. This machine's agents come from the activity
//! spool and show what they are doing this second; everyone else's come from
//! the relay, which knows what they hold and when they last wrote.

use super::*;

/// Everything one page renders from. `live` and `posts` are scoped to the
/// page's agent; `all_live` and `all_posts` are not, because the tree and the
/// plan show the whole team around the agent being looked at.
pub struct Board<'a> {
    pub live: &'a [Plan],
    pub posts: &'a [Plan],
    pub all_live: &'a [Plan],
    pub all_posts: &'a [Plan],
    /// This machine's agents and what each is doing, at the page's moment.
    pub local: &'a [(String, activity::Status)],
    /// This machine's spool up to the page's moment.
    pub events: &'a [activity::Event],
    /// This machine's key, whose agents `local` describes.
    pub me: &'a str,
    pub plan: Option<&'a [tracker::Group]>,
    pub tracker: &'a tracker::Tracker,
    /// The codebase this project is chiefly worked in, by the name entries
    /// carry: what the roadmap and the untracked list are about.
    pub here: &'a str,
    /// The project's working agreement, when its repo has one.
    pub agreement: Option<&'a agreement::Agreement>,
}

/// One agent on the page.
pub struct Node<'a> {
    pub pubkey: String,
    pub instance: String,
    pub claim: Option<&'a Plan>,
    pub last: Option<&'a Plan>,
    pub status: Option<&'a activity::Status>,
}

impl Node<'_> {
    pub fn path(&self) -> String {
        agent_path(&self.pubkey, &self.instance)
    }
}

/// The agents worth showing at `t`: holding a claim, active on this machine,
/// or heard from recently. Ancestors of a visible subagent are kept so it has
/// somewhere to hang.
pub fn nodes<'a>(b: &'a Board, t: i64) -> Vec<Node<'a>> {
    let mut out: Vec<Node> = Vec::new();
    fn at<'a, 'n>(out: &'n mut Vec<Node<'a>>, pubkey: &str, instance: &str) -> &'n mut Node<'a> {
        let i = match out
            .iter()
            .position(|n| n.pubkey == pubkey && n.instance == instance)
        {
            Some(i) => i,
            None => {
                out.push(Node {
                    pubkey: pubkey.into(),
                    instance: instance.into(),
                    claim: None,
                    last: None,
                    status: None,
                });
                out.len() - 1
            }
        };
        &mut out[i]
    }
    for p in b
        .all_live
        .iter()
        .filter(|p| p.live(t) && !p.touching.is_empty())
    {
        at(&mut out, &p.pubkey, &p.instance).claim = Some(p);
    }
    for p in b.all_posts {
        let n = at(&mut out, &p.pubkey, &p.instance);
        if n.last.is_none_or(|l| p.epoch > l.epoch) {
            n.last = Some(p);
        }
    }
    for (inst, s) in b.local {
        at(&mut out, b.me, inst).status = Some(s);
    }
    out.retain(|n| {
        n.claim.is_some()
            || n.status.is_some()
            || n.last.is_some_and(|l| t - l.epoch < ACTIVITY_WINDOW)
    });
    let missing: Vec<(String, String)> = out
        .iter()
        .flat_map(|n| {
            let segs: Vec<&str> = n.instance.split('/').collect();
            (1..segs.len()).map(move |i| (n.pubkey.clone(), segs[..i].join("/")))
        })
        .collect();
    for (pk, inst) in missing {
        at(&mut out, &pk, &inst);
    }
    out
}

/// A duration as a status line shows it: seconds while they matter.
/// `LIVE_JS` must format exactly the same way.
pub fn elapsed(secs: i64) -> String {
    let s = secs.max(0);
    match s {
        s if s < 60 => format!("{s}s"),
        s if s < 600 => format!("{}m {}s", s / 60, s % 60),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h {}m", s / 3600, s % 3600 / 60),
        s => format!("{}d", s / 86_400),
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Icon {
    Spin,
    Wait,
    Done,
    Hold,
}

/// What one agent is doing, ready to render.
struct Line {
    icon: Icon,
    /// The tone: work, wait, done, hold or quiet.
    tone: &'static str,
    text: String,
    /// The command behind `text`, shown quieter beside it, when `text` is
    /// the agent's own account of why it is running.
    cmd: String,
    since: Option<i64>,
    /// Words either side of the ticking duration: "idle 3m", "3m ago".
    pre: &'static str,
    post: &'static str,
}

fn line(n: &Node, t: i64) -> Line {
    use activity::State::*;
    if let Some(s) = n.status {
        let (icon, tone, post) = match s.state {
            Working | Thinking => (Icon::Spin, "work", ""),
            NeedsYou | YourTurn => (Icon::Wait, "wait", ""),
            Done | Gone => (Icon::Done, "done", " ago"),
        };
        // Why first, in the agent's words; what, beside it.
        let (text, cmd) = if s.why.is_empty() {
            (s.text.clone(), String::new())
        } else {
            (s.why.clone(), s.text.clone())
        };
        return Line {
            icon,
            tone,
            text,
            cmd,
            since: Some(s.since),
            pre: "",
            post,
        };
    }
    if let Some(c) = n.claim {
        let quiet = stuck(c, t);
        return Line {
            icon: Icon::Hold,
            tone: if quiet { "quiet" } else { "hold" },
            text: format!("holds {}", c.touching.join(" ")),
            cmd: String::new(),
            since: Some(c.epoch),
            pre: if quiet { "quiet " } else { "idle " },
            post: "",
        };
    }
    // A teammate's agent: the relay knows its last entry, not whether it is
    // still running, so say what that entry was.
    match n.last {
        Some(l) => {
            let done = matches!(l.kind.as_str(), "done" | "release");
            Line {
                icon: if done { Icon::Done } else { Icon::Hold },
                tone: if done { "done" } else { "hold" },
                text: match l.kind.as_str() {
                    "done" => "finished",
                    "release" => "released",
                    _ => "last seen",
                }
                .into(),
                cmd: String::new(),
                since: Some(l.epoch),
                pre: "",
                post: " ago",
            }
        }
        None => Line {
            icon: Icon::Hold,
            tone: "hold",
            text: String::new(),
            cmd: String::new(),
            since: None,
            pre: "",
            post: "",
        },
    }
}

const CHECK: &str = r#"<svg class=check viewBox="0 0 12 12" aria-hidden="true"><path d="M2.5 6.5l2.2 2.2 4.8-5" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/></svg>"#;
const MAC: &str = r#"<svg width="18" height="18" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true"><rect x="3" y="4" width="14" height="9" rx="1.5"/><path d="M1.5 16h17"/></svg>"#;
const TERM: &str = r#"<svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" aria-hidden="true"><rect x="1.5" y="2.5" width="13" height="11" rx="2"/><path d="M4.5 6.5l2 1.5-2 1.5M8.5 10h3"/></svg>"#;
const BRANCH: &str = r#"<svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" aria-hidden="true"><circle cx="4" cy="4" r="1.8"/><circle cx="12" cy="12" r="1.8"/><path d="M4 6v2a4 4 0 0 0 4 4h2"/></svg>"#;
const UP: &str = r#"<svg width="14" height="14" viewBox="0 0 14 14" aria-hidden="true"><path d="M3 9l4-4 4 4" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"/></svg>"#;
const DOWN: &str = r#"<svg width="14" height="14" viewBox="0 0 14 14" aria-hidden="true"><path d="M3 5l4 4 4-4" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"/></svg>"#;
const FOLDER: &str = r#"<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" aria-hidden="true"><path d="M2 4.5h4l1.5 1.5H14v6.5a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1z"/></svg>"#;
const CLOCK: &str = r#"<svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6" aria-hidden="true"><path d="M2.5 8a5.5 5.5 0 1 0 1.6-3.9M2.5 2.5v2.5H5"/><path d="M8 5v3.2l2 1.3"/></svg>"#;

fn icon_html(i: Icon) -> &'static str {
    match i {
        Icon::Spin => r#"<span class=spin aria-hidden="true"></span>"#,
        Icon::Wait => r#"<span class=dotw aria-hidden="true"></span>"#,
        Icon::Done => CHECK,
        Icon::Hold => r#"<span class=hold aria-hidden="true"></span>"#,
    }
}

fn line_html(l: &Line, t: i64) -> String {
    let when = match l.since {
        Some(s) => format!(
            r#" · {}<span data-since="{s}">{}</span>{}"#,
            l.pre,
            elapsed(t - s),
            l.post
        ),
        None => String::new(),
    };
    let cmd = if l.cmd.is_empty() {
        String::new()
    } else {
        format!(" <span class=cmd>{}</span>", esc(&l.cmd))
    };
    format!(
        r#"<span class="st t-{}">{} <span>{}{cmd}{when}</span></span>"#,
        l.tone,
        icon_html(l.icon),
        esc(&l.text)
    )
}

fn ticket_html(task: &str, tr: &tracker::Tracker) -> String {
    match tr.find(task) {
        Some((key, Some(u))) => format!(r#"<a class=key href="{}">{}</a>"#, esc(&u), esc(&key)),
        Some((key, None)) => format!("<span class=key>{}</span>", esc(&key)),
        None => String::new(),
    }
}

/// Is this agent inside what the page is scoped to?
pub fn in_scope(view: &View, pubkey: &str, instance: &str) -> bool {
    match &view.who {
        None => true,
        Some(w) if *w != name_for(pubkey) => false,
        Some(_) => {
            view.agent.is_empty()
                || instance == view.agent
                || instance.starts_with(&format!("{}/", view.agent))
        }
    }
}

#[derive(Default)]
pub struct Counts {
    pub working: usize,
    pub waiting: usize,
    pub holding: usize,
}

fn count(ns: &[&Node]) -> Counts {
    use activity::State::*;
    let mut c = Counts::default();
    for n in ns {
        match n.status.map(|s| &s.state) {
            Some(Working | Thinking) => c.working += 1,
            Some(NeedsYou | YourTurn) => c.waiting += 1,
            Some(_) => {}
            None if n.claim.is_some() => c.holding += 1,
            None => {}
        }
    }
    c
}

pub fn summary(c: &Counts) -> String {
    let mut bits = Vec::new();
    if c.working > 0 {
        bits.push(format!("{} working", c.working));
    }
    if c.waiting > 0 {
        bits.push(format!("{} waiting on you", c.waiting));
    }
    if c.holding > 0 {
        bits.push(format!("{} holding claims", c.holding));
    }
    if bits.is_empty() {
        bits.push("quiet".into());
    }
    bits.join(" · ")
}

pub fn counts(ns: &[Node]) -> Counts {
    count(&ns.iter().collect::<Vec<_>>())
}

/// Whether a top-level agent gets a row. One that finished more than a minute
/// ago has gone, unless it somehow still holds a claim. Subagents always get
/// one while they are on record: a finished one moves into its parent's Done
/// group rather than vanishing.
pub fn shown(n: &Node, t: i64) -> bool {
    n.claim.is_some() || !n.status.is_some_and(|s| finished_long_ago(s, t))
}

/// Finished, as far as anyone can tell: its tool said so, or — for a
/// teammate's agent the relay alone knows — its last word was a release.
fn is_done(n: &Node) -> bool {
    use activity::State::*;
    match n.status {
        Some(s) => matches!(s.state, Done | Gone),
        None => {
            n.claim.is_none()
                && n.last
                    .is_some_and(|l| matches!(l.kind.as_str(), "done" | "release"))
        }
    }
}

/// What a person reads an agent as: what its parent started it for, when its
/// tool recorded that, else the last segment of its path.
fn title(n: &Node) -> (String, bool) {
    match n.status.map(|s| s.label.as_str()).filter(|l| !l.is_empty()) {
        Some(l) => (l.to_string(), true),
        None => (
            n.instance
                .rsplit('/')
                .next()
                .filter(|s| !s.is_empty())
                .unwrap_or("(headless)")
                .to_string(),
            false,
        ),
    }
}

/// Machine identity, then agent, then its subagents in two groups: in
/// progress, open, and done, folded away.
pub fn tree(b: &Board, ns: &[Node], c: &Cfg, t: i64, view: &View) -> String {
    let moment = view.moment();
    let base = view.base();
    let selected = view.rel();
    let tops = |pk: &str| -> Vec<&Node> {
        let mine: Vec<&Node> = ns
            .iter()
            .filter(|n| n.pubkey == pk && !n.instance.contains('/'))
            .collect();
        let named: Vec<&Node> = mine
            .iter()
            .copied()
            .filter(|n| !n.instance.is_empty())
            .collect();
        let mut out = if named.is_empty() { mine } else { named };
        out.retain(|n| shown(n, t));
        out.sort_by_key(|n| tracker::natural(&n.instance));
        out
    };
    let mut keys: Vec<&str> = ns.iter().map(|n| n.pubkey.as_str()).collect();
    keys.sort_by_key(|k| (*k != b.me, name_for(k)));
    keys.dedup();
    keys.retain(|k| !tops(k).is_empty());
    if keys.is_empty() {
        return r#"<p class="none">Nobody on the record right now</p>"#.into();
    }
    keys.iter()
        .map(|pk| {
            let mine: Vec<&Node> = ns.iter().filter(|n| n.pubkey == *pk).collect();
            let name = name_for(pk);
            let label = if *pk == b.me {
                let live = if view.at.is_none() { " · live" } else { "" };
                format!("{} · this machine{live}", c.alias)
            } else {
                local_label(pk).unwrap_or_default()
            };
            let sel = if selected == format!("/{name}") { " sel" } else { "" };
            let agents: String = tops(pk).iter().map(|n| block(n, &mine, b, t, &base, &moment, &selected)).collect();
            format!(
                r#"<div class=mach><a class="mrow{sel}" href="{base}/{n}{moment}"><span class="av mac">{MAC}</span><span class=grow><span class=mname>{n}</span><span class=sub>{}</span></span><span class=sub>{}</span></a><div class=agents>{agents}</div></div>"#,
                esc(&label),
                summary(&count(&mine)),
                n = esc(&name),
            )
        })
        .collect()
}

/// One agent's row, and its subagents under it.
#[allow(clippy::too_many_arguments)]
fn block(
    n: &Node,
    all: &[&Node],
    b: &Board,
    t: i64,
    base: &str,
    moment: &str,
    selected: &str,
) -> String {
    let row = row(n, b, t, base, moment, selected);
    if n.instance.is_empty() {
        return format!("<div class=agent>{row}</div>");
    }
    let depth = n.instance.matches('/').count() + 1;
    let mut kids: Vec<&Node> = all
        .iter()
        .copied()
        .filter(|k| {
            k.instance.starts_with(&format!("{}/", n.instance))
                && k.instance.matches('/').count() == depth
        })
        .collect();
    if kids.is_empty() {
        return format!("<div class=agent>{row}</div>");
    }
    kids.sort_by_key(|k| tracker::natural(&k.instance));
    let (done, running): (Vec<&Node>, Vec<&Node>) = kids.into_iter().partition(|k| is_done(k));
    let running_html = if running.is_empty() {
        r#"<p class=quiet>Nothing running</p>"#.to_string()
    } else {
        running
            .iter()
            .map(|k| block(k, all, b, t, base, moment, selected))
            .collect()
    };
    let done_html = if done.is_empty() {
        String::new()
    } else {
        format!(
            r#"<details class=done data-id="{}:done"><summary class=grp>Done <span class=count>{}</span></summary>{}</details>"#,
            esc(&n.path()),
            done.len(),
            done.iter()
                .map(|k| block(k, all, b, t, base, moment, selected))
                .collect::<String>()
        )
    };
    format!(
        r#"<div class=agent>{row}<div class=subs><div class=grp>In progress <span class=count>{}</span></div>{running_html}{done_html}</div></div>"#,
        running.len()
    )
}

fn row(n: &Node, b: &Board, t: i64, base: &str, moment: &str, selected: &str) -> String {
    let path = n.path();
    let l = line(n, t);
    let sub = n.instance.contains('/');
    let (name, labelled) = title(n);
    let id = if labelled {
        format!(
            " <span class=aid>{}</span>",
            esc(n.instance.rsplit('/').next().unwrap_or(""))
        )
    } else {
        String::new()
    };
    // What it is on: its to-do item, else what it claimed for. The line a
    // person reads to know why the status below it is happening.
    let intent = n
        .status
        .map(|s| s.intent.clone())
        .filter(|i| !i.is_empty())
        .or_else(|| n.claim.map(|c| c.task.clone()).filter(|t| !t.is_empty()))
        .map(|i| format!("<span class=intent>{}</span>", esc(&truncate(&i, 110))))
        .unwrap_or_default();
    // A claim in someone else's area, per the working agreement: shown, never
    // refused. A claimed glob is placed by what precedes its first wildcard.
    let area = n
        .claim
        .zip(b.agreement)
        .and_then(|(c, a)| {
            c.touching.iter().find_map(|g| {
                let lit = g
                    .split(['*', '?', '['])
                    .next()
                    .unwrap_or(g)
                    .trim_end_matches('/');
                a.area_of(lit).filter(|ar| ar.owner != name_for(&n.pubkey))
            })
        })
        .map(|ar| format!(" <span class=inarea>in {}'s area</span>", esc(&ar.owner)))
        .unwrap_or_default();
    let ticket = n
        .claim
        .and_then(|c| b.tracker.find(&c.task))
        .map(|(k, _)| format!("<span class=tk>{}</span>", esc(&k)))
        .unwrap_or_default();
    format!(
        r#"<a class="arow{}{}{}" data-agent="{p}" href="{base}/{p}{moment}"><span class="av{}">{}</span><span class=grow><span class="aname{}">{}</span>{id}{intent}<span class=astat>{}</span></span>{area}{ticket}</a>"#,
        if selected == format!("/{path}") {
            " sel"
        } else {
            ""
        },
        if l.tone == "done" { " fin" } else { "" },
        if sub { " subrow" } else { "" },
        if sub { " small" } else { "" },
        if sub { BRANCH } else { TERM },
        if labelled { "" } else { " mono" },
        esc(&name),
        line_html(&l, t),
        p = esc(&path),
    )
}

/// What happened, newest first: agents appearing and finishing on this
/// machine, and everything on the timeline.
pub fn feed(b: &Board, t: i64, view: &View) -> String {
    let moment = view.moment();
    let base = view.base();
    let label = |inst: &str| {
        b.local
            .iter()
            .find(|(i, _)| i == inst)
            .map(|(_, s)| s.label.clone())
            .filter(|l| !l.is_empty())
    };
    let mut items: Vec<(i64, String, String)> = Vec::new();
    for e in b.events.iter().filter(|e| in_scope(view, b.me, &e.agent)) {
        let text = match e.kind.as_str() {
            "appear" => match label(&e.agent) {
                Some(l) => format!("started · {l}"),
                None => "started".into(),
            },
            "done" => "finished".into(),
            "gone" => "session ended".into(),
            "waiting" if !e.target.is_empty() => format!("needs you: {}", e.target),
            "waiting" => "needs you".into(),
            _ => continue,
        };
        items.push((e.t, agent_path(b.me, &e.agent), text));
    }
    for p in b.posts {
        let text = match p.kind.as_str() {
            "claim" => format!("claimed {} · {}", p.globs.join(" "), p.task),
            "release" => format!("released · {}", p.task),
            "done" if p.task.is_empty() => "finished".into(),
            "done" => format!("finished · {}", p.task),
            "ask" if p.to.is_empty() => format!("asks: {}", p.task),
            "ask" => format!("asks {}: {}", p.to, p.task),
            "answer" if p.to.is_empty() => format!("answered: {}", p.task),
            "answer" => format!("answered {}: {}", p.to, p.task),
            "directive" => format!("directed {}: {}", p.to, p.task),
            _ if !p.to.is_empty() => format!("to {}: {}", p.to, p.task),
            _ => p.task.clone(),
        };
        items.push((p.epoch, agent_path(&p.pubkey, &p.instance), text));
    }
    if items.is_empty() {
        return r#"<p class="none">Nothing yet</p>"#.into();
    }
    items.sort_by_key(|i| std::cmp::Reverse(i.0));
    items
        .iter()
        .take(16)
        .map(|(e, path, text)| {
            format!(
                r#"<div class=ev><span class=tm>{}</span><span class=grow><a class=evwho href="{base}/{p}{moment}">{p}</a> {}</span></div>"#,
                when(*e, t),
                esc(&truncate(text, 140)),
                p = esc(path),
            )
        })
        .collect()
}

/// `HH:MM` of a moment, local time.
fn clock(e: i64) -> String {
    stamp(e).rsplit(' ').next().unwrap_or_default().to_string()
}

/// `HH:MM` on the page's own day, `MM-DD HH:MM` before it.
fn when(e: i64, t: i64) -> String {
    let (day, now) = (stamp(e), stamp(t));
    if day.get(..10) == now.get(..10) {
        clock(e)
    } else {
        day.get(5..).unwrap_or(&day).to_string()
    }
}

const DAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Day of the week of a moment, local time, Sunday first.
fn weekday(e: i64) -> &'static str {
    let local = e + utc_offset(e);
    DAYS[(local.div_euclid(86_400) + 4).rem_euclid(7) as usize]
}

/// "Saturday 24 Oct, 19:13".
pub fn long_when(e: i64) -> String {
    let s = stamp(e);
    let month = s
        .get(5..7)
        .and_then(|m| m.parse::<usize>().ok())
        .unwrap_or(1);
    let day = s.get(8..10).unwrap_or("").trim_start_matches('0');
    format!(
        "{} {day} {}, {}",
        weekday(e),
        MONTHS[month.clamp(1, 12) - 1],
        clock(e)
    )
}

/// "Sat 19:13".
fn short_when(e: i64) -> String {
    format!("{} {}", &weekday(e)[..3], clock(e))
}

/// The entity the URL names: what it is doing, what it holds, and the trail
/// of what it did.
/// The page as data (#23): the agents in view and where each stands, the
/// entries in view, and the moments either side, so a script can step
/// through it the way the rail does. Same scope, same moment as the page.
pub fn json(b: &Board, t: i64, view: &View) -> serde_json::Value {
    let ns = nodes(b, t);
    let agents: Vec<serde_json::Value> = ns
        .iter()
        .filter(|n| in_scope(view, &n.pubkey, &n.instance) && shown(n, t))
        .map(|n| {
            let l = line(n, t);
            serde_json::json!({
                "path": n.path(),
                "state": l.tone,
                "doing": l.text,
                "since": l.since,
                "claim": n.claim.map(|c| serde_json::json!({
                    "task": c.task,
                    "globs": c.touching,
                    "state": if stuck(c, t) { "quiet" } else { "live" },
                })),
                "last_entry": n.last.map(|p| p.epoch),
            })
        })
        .collect();
    let mut posts: Vec<&Plan> = b.posts.iter().collect();
    posts.sort_by_key(|p| std::cmp::Reverse(p.epoch));
    let entries: Vec<serde_json::Value> = posts
        .iter()
        .map(|p| {
            serde_json::json!({
                "id": p.id,
                "epoch": p.epoch,
                "path": agent_path(&p.pubkey, &p.instance),
                "kind": if p.kind.is_empty() { "note" } else { p.kind.as_str() },
                "text": p.task,
                "to": p.to,
                "re": p.reply_to,
                "globs": p.globs,
                "commits": p.commits,
            })
        })
        .collect();
    serde_json::json!({
        "project": view.project,
        "path": view.rel(),
        "at": view.at,
        "now": t,
        "prev": view.prev,
        "next": view.next,
        "oldest": view.oldest,
        "agents": agents,
        "entries": entries,
    })
}

/// Send to whoever the page is about (#29): a message to anyone, or a
/// directive to one of your own agents (ADR 0008). Live pages only, since
/// the past cannot be written to, and only with the token `serve` issued.
pub fn send_form(b: &Board, view: &View) -> String {
    let Some(who) = &view.who else {
        return String::new();
    };
    if view.token.is_empty() || view.at.is_some() {
        return String::new();
    }
    let to = view.rel().trim_start_matches('/').to_string();
    let me = name_for(b.me);
    let mine = *who == me;
    let parent = view.agent.split('/').next().unwrap_or("");
    let kinds = if mine && !view.agent.is_empty() {
        r#"<div class="pill seg kinds"><label><input type=radio name=kind value=message checked>Message</label><label><input type=radio name=kind value=directive>Directive</label></div>"#
    } else {
        r#"<input type=hidden name=kind value=message>"#
    };
    let note = if view.agent.contains('/') {
        format!(
            "Goes to {who}/{parent}, which decides what to pass on: subagents do not hear from the team."
        )
    } else if !mine {
        format!("A message: their agent weighs it. Only {who}'s person can direct it.")
    } else if view.agent.is_empty() {
        "A message to every one of your agents. To direct one, open it.".into()
    } else {
        "A directive is acted on as if you had typed it into the session; a message is weighed."
            .into()
    };
    let sent = if view.sent {
        format!("<p class=sent>Sent. It is {DELIVERY}.</p>")
    } else {
        String::new()
    };
    format!(
        r#"<section class=card id=send><p class=eyebrow>Send to {to}</p>{sent}<form method=post action="{action}"><input type=hidden name=token value="{token}">{kinds}<textarea name=text maxlength={max} rows=3 required placeholder="What should they know?"></textarea><div class=sendrow><span class=sub>{note}</span><button class=btn type=submit>Send</button></div></form></section>"#,
        to = esc(&to),
        action = esc(&view.path()),
        token = esc(&view.token),
        max = MAX_ENTRY,
        note = esc(&note),
    )
}

pub fn detail(b: &Board, ns: &[Node], c: &Cfg, t: i64, view: &View) -> String {
    let Some(who) = &view.who else {
        return String::new();
    };
    let moment = view.moment();
    let kv = |rows: &[(&str, String)]| -> String {
        rows.iter()
            .map(|(k, v)| {
                format!(
                    "<div class=kvrow><span class=kvk>{k}</span><span class=kvv>{v}</span></div>"
                )
            })
            .collect()
    };
    if view.agent.is_empty() {
        let mine: Vec<&Node> = ns.iter().filter(|n| name_for(&n.pubkey) == *who).collect();
        let label = mine
            .first()
            .map(|n| {
                if n.pubkey == b.me {
                    c.alias.clone()
                } else {
                    local_label(&n.pubkey).unwrap_or_default()
                }
            })
            .unwrap_or_default();
        return format!(
            "<section class=card><p class=eyebrow>Machine identity</p><div class=big>{}</div><div class=sub>{}</div><div class=kv>{}</div></section>",
            esc(who),
            esc(&label),
            kv(&[
                ("Now", summary(&count(&mine))),
                ("Agents on record", mine.len().to_string())
            ])
        );
    }
    let path = format!("{who}/{}", view.agent);
    let Some(n) = ns.iter().find(|n| n.path() == path) else {
        return format!(
            "<section class=card><p class=eyebrow>Agent</p><div class=big mono>{}</div><p class=none>Nothing on the record for it at this moment.</p></section>",
            esc(&path)
        );
    };
    let base = view.base();
    let link = |p: &str| format!(r#"<a href="{base}/{p}{moment}">{p}</a>"#, p = esc(p));
    let mut facts: Vec<(&str, String)> = Vec::new();
    if let Some(i) = n
        .status
        .map(|s| s.intent.as_str())
        .filter(|i| !i.is_empty())
    {
        facts.push(("Now on", esc(i)));
    }
    if let Some(cl) = n.claim {
        facts.push(("Task", esc(&cl.task)));
        let ticket = ticket_html(&cl.task, b.tracker);
        if !ticket.is_empty() {
            facts.push(("Work item", ticket));
        }
        facts.push((
            "Claims",
            format!("<code>{}</code>", esc(&cl.touching.join(" "))),
        ));
    } else {
        facts.push(("Claims", "Nothing".into()));
    }
    facts.push((
        "Seen via",
        if n.status.is_some() || n.pubkey == b.me {
            "This machine's hooks, live".into()
        } else {
            "The timeline".into()
        },
    ));
    if let Some((parent, _)) = n.instance.rsplit_once('/') {
        facts.push(("Reports to", link(&agent_path(&n.pubkey, parent))));
    }
    let kids: Vec<&Node> = ns
        .iter()
        .filter(|k| {
            k.pubkey == n.pubkey
                && k.instance
                    .rsplit_once('/')
                    .is_some_and(|(p, _)| p == n.instance)
        })
        .collect();
    if !kids.is_empty() {
        let done = kids.iter().filter(|k| is_done(k)).count();
        facts.push((
            "Subagents",
            format!("{} in progress · {done} done", kids.len() - done),
        ));
    }
    let trail: String = if n.pubkey == b.me {
        b.events
            .iter()
            .rev()
            .filter(|e| e.agent == n.instance)
            .take(12)
            .map(|e| {
                let text = match e.kind.as_str() {
                    "tool" if !e.why.is_empty() => {
                        format!("{} — {}", e.why, activity::doing(&e.tool, &e.target))
                    }
                    "tool" => activity::doing(&e.tool, &e.target),
                    "intent" if !e.target.is_empty() => format!("On to: {}", e.target),
                    "intent" => "Finished its to-do list".into(),
                    "idle" => "Thinking".into(),
                    "prompt" => "New prompt from its person".into(),
                    "waiting" if !e.target.is_empty() => format!("Needs you: {}", e.target),
                    "waiting" => "Needs you".into(),
                    "stop" => "Finished its turn".into(),
                    "appear" => "Started".into(),
                    "done" => "Finished".into(),
                    "gone" => "Session ended".into(),
                    other => other.into(),
                };
                format!(
                    r#"<div class=ev><span class=tm>{}</span><span class=grow>{}</span></div>"#,
                    clock(e.t),
                    esc(&text)
                )
            })
            .collect()
    } else {
        String::new()
    };
    let trail = if trail.is_empty() {
        String::new()
    } else {
        format!("<p class=eyebrow style=\"margin-top:18px\">Recent</p>{trail}")
    };
    let l = line(n, t);
    let (name, _) = title(n);
    format!(
        r#"<section class=card><p class=eyebrow>{}</p><div class=big>{}</div><div class="sub mono">{}</div><div class="pill tone-{}">{}</div><div class=kv>{}</div>{trail}</section>"#,
        if n.instance.contains('/') {
            "Subagent"
        } else {
            "Agent"
        },
        esc(&name),
        esc(&path),
        l.tone,
        line_html(&l, t),
        kv(&facts),
    )
}

/// The working agreement as a panel: areas, where anyone may work, where to
/// ask first, and who has accepted the version in the repo.
pub fn agreement_panel(b: &Board) -> String {
    let Some(a) = b.agreement else {
        return String::new();
    };
    let accepted: Vec<String> = {
        let mut names: Vec<String> = b
            .all_posts
            .iter()
            .filter(|p| matches!(p.kind.as_str(), "accept" | "join") && p.task.contains(&a.version))
            .map(|p| name_for(&p.pubkey))
            .collect();
        names.sort();
        names.dedup();
        names
    };
    let areas: String = a
        .areas
        .iter()
        .map(|ar| {
            let who = if ar.person.is_empty() {
                esc(&ar.owner)
            } else {
                format!("{} <span class=sub>({})</span>", esc(&ar.owner), esc(&ar.person))
            };
            format!(
                "<div class=item><span class=grow><span class=pname>{who}</span><div class=pabout>{}</div><div><code>{}</code></div></span></div>",
                esc(&ar.about),
                esc(&ar.globs.join("  "))
            )
        })
        .collect();
    let mut extra = String::new();
    let anywhere: Vec<String> = a
        .anywhere_notes
        .iter()
        .chain(a.anywhere.iter())
        .cloned()
        .collect();
    if !anywhere.is_empty() {
        extra.push_str(&format!(
            "<div class=kvrow><span class=kvk>Anyone</span><span class=kvv>{}</span></div>",
            esc(&anywhere.join(", "))
        ));
    }
    if !a.ask_first.is_empty() {
        let items: Vec<String> = a
            .ask_first
            .iter()
            .map(|(g, why)| {
                if why.is_empty() {
                    esc(g)
                } else {
                    format!("{} <span class=sub>({})</span>", esc(g), esc(why))
                }
            })
            .collect();
        extra.push_str(&format!(
            "<div class=kvrow><span class=kvk>Ask first</span><span class=kvv>{}</span></div>",
            items.join("<br>")
        ));
    }
    extra.push_str(&format!(
        "<div class=kvrow><span class=kvk>Accepted</span><span class=kvv>{}</span></div>",
        if accepted.is_empty() {
            "nobody yet, at this version".to_string()
        } else {
            esc(&accepted.join(", "))
        }
    ));
    format!(
        r#"<section class=card><p class=eyebrow>Working agreement · {}</p>{areas}<div class=kv>{extra}</div></section>"#,
        esc(&a.version)
    )
}

/// A progress ring: how much of a phase is closed.
fn ring(closed: usize, total: usize, active: bool) -> String {
    let c = 2.0 * std::f64::consts::PI * 15.0;
    let done = c * closed as f64 / total.max(1) as f64;
    format!(
        r#"<svg class=ring width="26" height="26" viewBox="0 0 36 36" aria-hidden="true"><circle cx="18" cy="18" r="15" class=rbg /><circle cx="18" cy="18" r="15" class="rfg{}" stroke-dasharray="{done:.1} {c:.1}" transform="rotate(-90 18 18)"/></svg>"#,
        if active { " on" } else { "" }
    )
}

/// The project's roadmap, in the order it lands, with the agents on each item.
pub fn plan(
    groups: &[tracker::Group],
    b: &Board,
    ns: &[Node],
    t: i64,
    view: &View,
    here: &str,
) -> String {
    let moment = view.moment();
    let base = view.base();
    let working_on = |key: &str| -> Vec<&Node> {
        ns.iter()
            .filter(|n| {
                n.claim
                    .is_some_and(|c| b.tracker.find(&c.task).is_some_and(|(k, _)| k == key))
            })
            .collect()
    };
    let who_html = |n: &Node| {
        format!(
            r#"<div class=who><a href="{base}/{p}{moment}">{p}</a> {}</div>"#,
            line_html(&line(n, t), t),
            p = esc(&n.path())
        )
    };
    let body: String = groups
        .iter()
        .map(|g| {
            let closed = g.issues.iter().filter(|i| i.closed).count();
            let mut active = 0;
            let mut waiting = 0;
            let rows: String = g
                .issues
                .iter()
                .map(|i| {
                    let on = working_on(&i.key);
                    let c = count(&on);
                    let (chip, cls) = if i.closed {
                        ("Done", "b-done")
                    } else if c.waiting > 0 {
                        waiting += 1;
                        ("Waiting on you", "b-wait")
                    } else if !on.is_empty() {
                        active += 1;
                        ("In progress", "b-work")
                    } else {
                        ("Open", "b-open")
                    };
                    format!(
                        r#"<div class=item><a class=key href="{}">{}</a><span class=grow>{}{}</span><span class="badge {cls}">{chip}</span></div>"#,
                        esc(&i.url),
                        esc(&i.key),
                        esc(&i.title),
                        on.iter().map(|n| who_html(n)).collect::<String>()
                    )
                })
                .collect();
            let mut bits = vec![format!("{closed} of {}", g.issues.len())];
            if active > 0 {
                bits.push(format!("{active} in progress"));
            }
            if waiting > 0 {
                bits.push(format!("{waiting} waiting on you"));
            }
            let about = if g.about.is_empty() { String::new() } else { format!("{} · ", g.about) };
            let open = if active + waiting > 0 { " open" } else { "" };
            format!(
                r#"<details class=phase data-id="plan:{n}"{open}><summary>{}<span class=grow><span class=pname>{n}</span><span class=pabout>{}{}</span></span></summary><div class=items>{rows}</div></details>"#,
                ring(closed, g.issues.len(), active + waiting > 0),
                esc(&about),
                bits.join(" · "),
                n = esc(&g.name),
            )
        })
        .collect();
    let untracked: String = ns
        .iter()
        .filter(|n| {
            n.claim
                .is_some_and(|c| c.project == here && b.tracker.find(&c.task).is_none())
        })
        .map(&who_html)
        .collect();
    let untracked = if untracked.is_empty() {
        String::new()
    } else {
        format!("<p class=eyebrow style=\"margin-top:14px\">Untracked</p>{untracked}")
    };
    format!(
        r#"<section class="card roadmap"><p class=eyebrow>Roadmap · {}</p>{body}{untracked}</section>"#,
        esc(here)
    )
}

/// The page's window onto the past, down its right edge: a tick per event,
/// longer where a day starts, and Now at the foot. Every tick is a link, so
/// it works with no script; with one, the rail is a slider.
pub fn rail(view: &View) -> String {
    let p = esc(&view.path());
    let n = view.epochs.len();
    let cur = match view.at {
        None => n,
        Some(at) => view.epochs.iter().rposition(|&e| e <= at).unwrap_or(0),
    };
    let mut ticks = String::new();
    let mut last_day = String::new();
    for (i, &e) in view.epochs.iter().enumerate() {
        let day = stamp(e).get(..10).unwrap_or("").to_string();
        let label = if day != last_day {
            format!(
                "{} {}",
                &weekday(e)[..3],
                day.get(8..10).unwrap_or("").trim_start_matches('0')
            )
        } else {
            String::new()
        };
        last_day = day;
        ticks.push_str(&format!(
            r#"<a class="rtk{}{}" href="{p}{}" title="{}" data-when="{}"><span class=tkl>{label}</span><span class=tkline></span></a>"#,
            if label.is_empty() { "" } else { " day" },
            if i == cur { " cur" } else { "" },
            esc(&view.query(Some(e))),
            long_when(e),
            short_when(e),
        ));
    }
    ticks.push_str(&format!(
        r#"<a class="rtk day now{}" href="{p}{}" title="Now" data-when="Now"><span class=tkl>Now</span><span class=tkline></span></a>"#,
        if cur == n { " cur" } else { "" },
        esc(&view.query(None))
    ));
    let arrow = |href: Option<String>, label: &str, svg: &str| match href {
        Some(h) => format!(r#"<a class=rbtn href="{h}" aria-label="{label}">{svg}</a>"#),
        None => format!(r#"<span class="rbtn off" aria-hidden="true">{svg}</span>"#),
    };
    let up = arrow(
        view.prev
            .map(|e| format!("{p}{}", esc(&view.query(Some(e))))),
        "Earlier event",
        UP,
    );
    let down = match (view.at, view.next) {
        (Some(_), Some(e)) => arrow(
            Some(format!("{p}{}", esc(&view.query(Some(e))))),
            "Later event",
            DOWN,
        ),
        (Some(_), None) => arrow(Some(format!("{p}{}", esc(&view.query(None)))), "Now", DOWN),
        (None, _) => arrow(None, "", DOWN),
    };
    let (flag, flag_long) = match view.at {
        None => ("Now".to_string(), "Now".to_string()),
        Some(_) => view
            .epochs
            .get(cur)
            .map(|&e| (short_when(e), long_when(e)))
            .unwrap_or_default(),
    };
    format!(
        r#"<aside class=rail id=rail aria-label="Time Machine">{up}<div class=track><div class=ticks>{ticks}</div><span class=flag style="top:{:.2}%">{}</span><label class=vh for=scrub>Moment to view</label><input id=scrub class=scrub type=range min=0 max={n} step=1 value={cur} aria-valuetext="{}"></div>{down}</aside>"#,
        if n == 0 {
            100.0
        } else {
            100.0 * cur as f64 / n as f64
        },
        esc(&flag),
        esc(&flag_long),
    )
}

/// On a page fixed at a moment: what moment, the way back, and the stacked
/// boards behind this one, Time Machine's receding windows.
pub fn history(view: &View) -> (String, String) {
    match view.at {
        None => (String::new(), String::new()),
        Some(t) => (
            format!(
                r#"<div class=banner><div class=banner-in>{CLOCK}<span>Viewing <b>{}</b></span><a class=btn href="{}">Back to Now</a></div></div>"#,
                long_when(t),
                esc(&format!("{}{}", view.path(), view.query(None)))
            ),
            "<div class=\"ghost g3\"></div><div class=\"ghost g2\"></div><div class=\"ghost g1\"></div>".into(),
        ),
    }
}

/// The toolbar's mode: live, or the moment being looked at.
pub fn mode(view: &View) -> String {
    match view.at {
        None => r#"<span class="pill live"><span class=dot></span>Live</span>"#.into(),
        Some(t) => format!(
            r#"<span class="pill hist">{CLOCK}History · {}</span>"#,
            short_when(t)
        ),
    }
}

/// "This project · All": which projects the page shows. All is everything
/// this machine can read; this project is the repo `serve` runs in.
pub fn switch(view: &View, here: &str) -> String {
    let flip = View {
        all: !view.all,
        ..view.clone()
    };
    let href = format!("{}{}", view.path(), flip.query(view.at));
    let (this, all) = if view.all {
        (
            format!(r#"<a href="{}">{}</a>"#, esc(&href), esc(here)),
            "<b>All</b>".to_string(),
        )
    } else {
        (
            format!("<b>{}</b>", esc(here)),
            format!(r#"<a href="{}">All</a>"#, esc(&href)),
        )
    };
    format!(
        r#"<span class="pill seg" role="group" aria-label="Projects shown">{this}<span class=segsep></span>{all}</span>"#
    )
}

/// The front page when there is more than one project: one card each.
pub fn project_list(ps: &[Project]) -> String {
    let cards: String = if ps.is_empty() {
        r#"<section class=card><p class=none>No projects yet. In a folder you want shared, run <code>robofinger join &lt;name&gt;</code>.</p></section>"#.into()
    } else {
        ps.iter()
            .map(|p| {
                let folders: String = p
                    .folders
                    .iter()
                    .map(|f| format!("<div class=\"sub mono\">{}</div>", esc(f)))
                    .collect();
                format!(
                    r#"<a class="card proj" href="/{s}"><span class="av mac">{FOLDER}</span><span class=grow><span class=big>{s}</span><span class="sub mono">{}</span>{folders}</span></a>"#,
                    esc(&p.url),
                    s = esc(&p.slug),
                )
            })
            .collect()
    };
    format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>robofinger · projects</title><style>{CSS}
.projects{{max-width:760px;margin:24px auto;padding:0 16px}}.proj{{display:flex;gap:14px;align-items:flex-start;color:inherit}}.proj:hover{{text-decoration:none;border-color:var(--accent)}}</style></head><body>
<header class=toolbar><span class=brand>robofinger</span><span class=spacer></span><span class=sub>{} project{}</span></header>
<main class=projects><p class=eyebrow>Projects you have joined</p>{cards}</main></body></html>"#,
        ps.len(),
        if ps.len() == 1 { "" } else { "s" },
    )
}

pub fn folder() -> &'static str {
    FOLDER
}

/// Turns the rail into a slider: dragging moves the marker and its time, and
/// letting go goes there. Loaded on every page; without it the ticks are
/// links and the arrows step.
pub const RAIL_JS: &str = r##"<script>
(function () {
  function wire() {
    var rail = document.getElementById("rail");
    if (!rail) return;
    var s = rail.querySelector(".scrub"), ticks = rail.querySelectorAll(".rtk"), flag = rail.querySelector(".flag");
    if (!s || s.dataset.wired) return;
    s.dataset.wired = "1";
    function show(i) {
      var n = ticks.length - 1;
      flag.style.top = (n ? 100 * i / n : 100) + "%";
      flag.textContent = ticks[i].dataset.when;
      ticks.forEach(function (t, j) { t.classList.toggle("cur", j === i); });
    }
    s.addEventListener("input", function () { show(Number(s.value)); });
    s.addEventListener("change", function () { location.href = ticks[Number(s.value)].href; });
  }
  wire();
  window.rfWireRail = wire;
})();
</script>"##;

/// Keeps a live page live: re-fetches it every two seconds and swaps the
/// board in, keeping what is folded folded, marking agents that just
/// appeared, and ticking every duration once a second in between. The rail
/// is left alone while someone has hold of it.
pub const LIVE_JS: &str = r##"<script>
(function () {
  // A sent form lands on ?sent=1; reloading that must not read as sending again.
  if (/[?&]sent=1/.test(location.search)) {
    history.replaceState(null, "", location.pathname + location.search.replace(/[?&]sent=1/, "").replace(/^&/, "?"));
  }
  function elapsed(s) {
    s = Math.max(0, Math.floor(s));
    if (s < 60) return s + "s";
    if (s < 600) return Math.floor(s / 60) + "m " + (s % 60) + "s";
    if (s < 3600) return Math.floor(s / 60) + "m";
    if (s < 86400) return Math.floor(s / 3600) + "h " + Math.floor(s % 3600 / 60) + "m";
    return Math.floor(s / 86400) + "d";
  }
  function tick() {
    var now = Date.now() / 1000;
    document.querySelectorAll("[data-since]").forEach(function (e) {
      e.textContent = elapsed(now - Number(e.dataset.since));
    });
  }
  async function refresh() {
    try {
      var r = await fetch(location.href, { cache: "no-store" });
      if (!r.ok) return;
      var next = new DOMParser().parseFromString(await r.text(), "text/html");
      var open = {}, seen = {};
      document.querySelectorAll("details[data-id]").forEach(function (d) { open[d.dataset.id] = d.open; });
      document.querySelectorAll("[data-agent]").forEach(function (e) { seen[e.dataset.agent] = 1; });
      var keep = document.getElementById("send");
      var ids = ["board", "summary"];
      var rail = document.getElementById("rail");
      if (rail && !rail.contains(document.activeElement)) ids.push("rail");
      ids.forEach(function (id) {
        var a = document.getElementById(id), b = next.getElementById(id);
        if (a && b) a.replaceWith(b);
      });
      // What is being typed survives the refresh.
      var fresh = document.getElementById("send");
      if (keep && fresh) fresh.replaceWith(keep);
      document.querySelectorAll("details[data-id]").forEach(function (d) {
        if (d.dataset.id in open) d.open = open[d.dataset.id];
      });
      document.querySelectorAll("[data-agent]").forEach(function (e) {
        if (!seen[e.dataset.agent]) e.classList.add("arriving");
      });
      if (window.rfWireRail) window.rfWireRail();
      tick();
    } catch (e) {}
  }
  setInterval(tick, 1000);
  setInterval(refresh, 2000);
})();
</script>"##;

/// The page's look: Apple's, light or dark by the system's setting.
pub const CSS: &str = r##"
:root{--bg:#f5f5f7;--panel:#fff;--panel2:#fbfbfd;--line:rgba(0,0,0,.08);--fg:#1d1d1f;--fg2:#6e6e73;--accent:#0071e3;--accentbg:rgba(0,113,227,.10);--wait:#b25000;--waitbg:rgba(255,159,10,.16);--done:#1d7a35;--donebg:rgba(52,199,89,.14);--bad:#d70015;--badbg:rgba(255,59,48,.12);--shadow:0 1px 2px rgba(0,0,0,.04),0 8px 28px rgba(0,0,0,.05);--glass:rgba(255,255,255,.72);--rail:#d2d2d7;--sel:rgba(0,113,227,.09);--pastbg:#e9ecf2}
@media (prefers-color-scheme:dark){:root{--bg:#000;--panel:#1c1c1e;--panel2:#232325;--line:rgba(255,255,255,.10);--fg:#f5f5f7;--fg2:#a1a1a6;--accent:#2997ff;--accentbg:rgba(41,151,255,.16);--wait:#ffb340;--waitbg:rgba(255,159,10,.18);--done:#30d158;--donebg:rgba(48,209,88,.16);--bad:#ff6961;--badbg:rgba(255,69,58,.18);--shadow:0 0 0 1px rgba(255,255,255,.04);--glass:rgba(28,28,30,.72);--rail:#3a3a3c;--sel:rgba(41,151,255,.16);--pastbg:#0b0d14}}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--fg);font:14px/1.45 -apple-system,BlinkMacSystemFont,"SF Pro Text","Helvetica Neue",system-ui,sans-serif;-webkit-font-smoothing:antialiased}
body.past{background:var(--pastbg)}
a{color:var(--accent);text-decoration:none}a:hover{text-decoration:underline}
code,.mono{font-family:"SF Mono",ui-monospace,Menlo,monospace;font-size:12px}
.toolbar{position:sticky;top:0;z-index:5;display:flex;align-items:center;gap:10px;flex-wrap:wrap;padding:12px 24px;background:var(--glass);backdrop-filter:saturate(180%) blur(20px);-webkit-backdrop-filter:saturate(180%) blur(20px);border-bottom:1px solid var(--line)}
.brand{font-weight:600;font-size:17px;letter-spacing:-.01em;color:var(--fg)}.brand:hover{text-decoration:none}
.spacer{flex:1}
.pill{display:inline-flex;align-items:center;gap:6px;padding:4px 12px;border-radius:999px;background:var(--panel);border:1px solid var(--line);font-size:13px}
.live{color:var(--done)}.live .dot{width:7px;height:7px;border-radius:50%;background:currentColor;animation:pulse 2s ease-in-out infinite}
@keyframes pulse{50%{opacity:.35}}
.hist{color:var(--accent)}
.seg{gap:0;padding:2px}.seg b,.seg a{padding:2px 10px;border-radius:999px;font-weight:500}.seg b{background:var(--accentbg);color:var(--accent)}.seg a{color:var(--fg2)}.seg a:hover{text-decoration:none;color:var(--fg)}.segsep{width:2px}
.sub{color:var(--fg2);font-size:13px}
.crumbs{padding:10px 24px 0;font-size:12px;color:var(--fg2)}.crumbs nav{display:inline}
.workspace{display:flex;align-items:stretch}
.stage{flex:1;min-width:0;padding:16px 8px 40px 24px}
.banner{display:flex;justify-content:center;margin:0 0 14px}
.banner-in{display:inline-flex;align-items:center;gap:10px;flex-wrap:wrap;padding:6px 6px 6px 14px;border-radius:999px;background:var(--glass);backdrop-filter:blur(20px);-webkit-backdrop-filter:blur(20px);border:1px solid var(--line);box-shadow:var(--shadow);font-size:13px;color:var(--fg)}
.banner-in svg{color:var(--accent)}
.btn{background:var(--accent);color:#fff;border-radius:999px;padding:6px 14px;font-size:13px;font-weight:500;min-height:30px;display:inline-flex;align-items:center}
.btn:hover{text-decoration:none;filter:brightness(1.08)}
#send textarea{display:block;width:100%;margin:10px 0 8px;padding:8px 10px;font:inherit;color:var(--fg);background:var(--panel2);border:1px solid var(--line);border-radius:10px;resize:vertical}
#send textarea:focus{outline:3px solid var(--accentbg);border-color:var(--accent)}
#send .btn{border:0;cursor:pointer;font:inherit;font-weight:500;flex:none}
.sendrow{display:flex;align-items:center;justify-content:space-between;gap:12px}
.kinds label{padding:2px 10px;border-radius:999px;color:var(--fg2);cursor:pointer;font-weight:500}.kinds input{position:absolute;opacity:0;pointer-events:none}
.kinds label:has(input:checked){background:var(--accentbg);color:var(--accent)}.kinds label:has(input:focus-visible){outline:2px solid var(--accent)}
.sent{color:var(--done);font-size:13px;margin:0 0 4px}
.stack{position:relative}
.ghost{position:absolute;inset:0;border-radius:18px;background:var(--panel);border:1px solid var(--line);pointer-events:none}
.g1{transform:translateY(-14px) scale(.975);opacity:.55}.g2{transform:translateY(-26px) scale(.95);opacity:.3}.g3{transform:translateY(-36px) scale(.925);opacity:.14}
.board{position:relative;display:flex;flex-wrap:wrap;gap:16px;align-items:flex-start}
.side{flex:1 1 300px;max-width:380px;min-width:0}.center{flex:1.5 1 440px;min-width:0}.inspector{flex:1 1 300px;max-width:400px;min-width:0}
section,.card{background:var(--panel);border-radius:16px;box-shadow:var(--shadow);border:1px solid var(--line);padding:16px 18px;margin-bottom:16px;overflow-x:auto}
.eyebrow,section h2{font-size:11px;font-weight:600;letter-spacing:.06em;text-transform:uppercase;color:var(--fg2);margin:0 0 10px}
.cardhead{display:flex;justify-content:space-between;align-items:baseline;gap:8px;flex-wrap:wrap}
.n{color:var(--accent)}
.grow{flex:1;min-width:0;display:flex;flex-direction:column}
details>summary{list-style:none;cursor:pointer}details>summary::-webkit-details-marker{display:none}
.mach{margin-top:6px}
.mrow,.arow{display:flex;align-items:center;gap:10px;padding:8px 10px;border-radius:10px;color:inherit;min-height:44px}
.mrow:hover,.arow:hover{background:var(--panel2);text-decoration:none}
.mrow.sel,.arow.sel{background:var(--sel)}
.mname{font-weight:600}
.agents{margin-left:26px;padding-left:12px;border-left:1px solid var(--line)}
.subs{margin-left:26px;padding-left:12px;border-left:1px solid var(--line);margin-bottom:4px}
.av{flex:none;width:30px;height:30px;border-radius:9px;display:inline-flex;align-items:center;justify-content:center;background:var(--panel2);border:1px solid var(--line);color:var(--fg2)}
.av.small{width:26px;height:26px;border-radius:8px}
.av.mac{width:34px;height:34px;border-radius:10px;background:var(--accentbg);color:var(--accent);border:0}
.aname{font-weight:500}.aname.mono{font-size:13px}
.aid{font-family:"SF Mono",ui-monospace,Menlo,monospace;font-size:11px;color:var(--fg2)}
.astat{font-size:12px}
.subrow .aname{font-size:13px}
.fin{opacity:.6}
.arriving{animation:arrive .6s cubic-bezier(.2,.8,.2,1)}
@keyframes arrive{from{opacity:0;transform:translateY(-6px) scale(.98)}to{opacity:1;transform:none}}
.grp{display:flex;align-items:center;gap:6px;font-size:11px;font-weight:600;letter-spacing:.04em;text-transform:uppercase;color:var(--fg2);padding:8px 10px 4px;min-height:30px}
details.done>summary.grp::before{content:"";width:6px;height:6px;border-right:1.6px solid currentColor;border-bottom:1.6px solid currentColor;transform:rotate(-45deg);margin-right:4px;transition:transform .2s}
details.done[open]>summary.grp::before{transform:rotate(45deg)}
.count{font-weight:500;letter-spacing:0;background:var(--line);border-radius:999px;padding:0 7px;text-transform:none}
.quiet{margin:2px 0 6px 10px;font-size:12px;color:var(--fg2)}
.tk{font-size:12px;color:var(--accent);font-weight:500;flex:none}
.intent{font-size:13px;color:var(--fg);margin:1px 0 2px}
.cmd{font-family:"SF Mono",ui-monospace,Menlo,monospace;font-size:11px;color:var(--fg2);opacity:.85}
.inarea{font-size:11px;font-weight:500;padding:1px 8px;border-radius:999px;background:var(--waitbg);color:var(--wait);white-space:nowrap;flex:none}
.st{display:inline-flex;align-items:center;gap:6px;color:var(--fg2)}
.t-work{color:var(--accent)}.t-wait{color:var(--wait)}.t-done{color:var(--done)}.t-quiet{color:var(--bad)}
.spin{width:11px;height:11px;border-radius:50%;border:2px solid var(--accentbg);border-top-color:var(--accent);animation:spin .9s linear infinite;flex:none}
@keyframes spin{to{transform:rotate(360deg)}}
.dotw{width:8px;height:8px;border-radius:50%;background:var(--wait);flex:none}
.hold{width:8px;height:8px;border-radius:50%;border:1.5px solid currentColor;flex:none}
.check{width:12px;height:12px;flex:none}
.ev{display:flex;gap:10px;padding:7px 0;border-top:1px solid var(--line);font-size:13px;align-items:baseline}
.ev:first-of-type{border-top:0}
.tm{color:var(--fg2);font-size:12px;flex:none;min-width:44px;font-variant-numeric:tabular-nums}
.evwho{font-weight:500;font-family:"SF Mono",ui-monospace,Menlo,monospace;font-size:12px}
.ev .grow{display:block}
.big{font-size:22px;font-weight:600;letter-spacing:-.02em;word-break:break-word;margin:2px 0 4px}
.inspector .pill{margin:12px 0 0;padding:6px 12px}
.tone-work{background:var(--accentbg);border-color:transparent}.tone-wait{background:var(--waitbg);border-color:transparent}.tone-done{background:var(--donebg);border-color:transparent}.tone-quiet{background:var(--badbg);border-color:transparent}
.kv{margin:14px 0 0;border-radius:12px;background:var(--panel2);border:1px solid var(--line);overflow:hidden}
.kvrow{display:flex;justify-content:space-between;gap:12px;padding:10px 14px;border-top:1px solid var(--line);font-size:13px}
.kvrow:first-child{border-top:0}.kvk{color:var(--fg2);flex:none}.kvv{text-align:right;word-break:break-word}
.phase{border-top:1px solid var(--line)}.phase:first-of-type{border-top:0}
.phase>summary{display:flex;align-items:center;gap:10px;padding:8px 4px;border-radius:10px;min-height:44px}
.phase>summary:hover{background:var(--panel2)}
.ring{flex:none}.rbg{fill:none;stroke:var(--line);stroke-width:4}.rfg{fill:none;stroke:var(--done);stroke-width:4;stroke-linecap:round}.rfg.on{stroke:var(--accent)}
.pname{font-weight:500}.pabout{font-size:12px;color:var(--fg2)}
.items{padding:0 0 8px 36px}
.item{display:flex;gap:8px;align-items:baseline;padding:6px 0;font-size:13px;border-top:1px solid var(--line)}
.item .grow{display:block}
.key{font-weight:500;white-space:nowrap}
.badge{font-size:11px;font-weight:500;padding:1px 8px;border-radius:999px;white-space:nowrap}
.b-work{background:var(--accentbg);color:var(--accent)}.b-wait{background:var(--waitbg);color:var(--wait)}.b-done{background:var(--donebg);color:var(--done)}.b-open{background:var(--line);color:var(--fg2)}
.who{font-size:12px;padding-top:3px}.who a{font-family:"SF Mono",ui-monospace,Menlo,monospace}
.none{color:var(--fg2);font-style:italic;margin:0;font-size:13px}.ok{color:var(--done);font-weight:600;margin:0}
table{border-collapse:collapse;width:100%;font-size:13px}th,td{text-align:left;padding:6px 8px;border-bottom:1px solid var(--line);vertical-align:top}th{color:var(--fg2);font-weight:500;font-size:12px}
.tag{border-radius:999px;padding:1px 8px;font-size:11px;font-weight:500;background:var(--line);color:var(--fg2);white-space:nowrap}.tag.bad{background:var(--badbg);color:var(--bad)}
.item>.sub,.item .sub{font-size:12px}
.ph{display:flex;justify-content:space-between;align-items:baseline;gap:8px;margin-bottom:8px}h2.proj{font-size:15px;text-transform:none;letter-spacing:-.01em;color:var(--fg);margin:0}
.dim{color:var(--fg2)}
.rail{flex:none;width:96px;position:sticky;top:57px;height:calc(100vh - 57px);display:flex;flex-direction:column;padding:12px 10px 16px 0}
.rbtn{align-self:flex-end;width:32px;height:32px;border-radius:8px;display:inline-flex;align-items:center;justify-content:center;color:var(--fg2)}
.rbtn:hover{background:var(--line);color:var(--fg)}.rbtn.off{opacity:.3}
.track{position:relative;flex:1;margin:6px 0}
.ticks{position:absolute;inset:0;display:flex;flex-direction:column;justify-content:space-between;align-items:flex-end}
.rtk{display:flex;align-items:center;justify-content:flex-end;gap:6px;height:3px;width:100%;color:var(--fg2)}
.rtk:hover{text-decoration:none}
.tkl{font-size:10px;font-weight:500;white-space:nowrap}
.tkline{height:2px;border-radius:1px;background:var(--rail);width:14px;transition:width .15s}
.rtk.day .tkline{width:24px;background:var(--fg2);opacity:.6}
.rtk.cur .tkline{width:34px;height:3px;background:var(--accent);opacity:1}
.track:hover .tkline{width:20px}.track:hover .rtk.day .tkline{width:28px}
.scrub{position:absolute;inset:-6px 0;width:100%;height:calc(100% + 12px);margin:0;opacity:0;cursor:grab;writing-mode:vertical-lr;direction:ltr;z-index:2}
.scrub:active{cursor:grabbing}.scrub:focus-visible{opacity:.2}
.flag{position:absolute;right:42px;transform:translateY(-50%);background:var(--accent);color:#fff;font-size:11px;font-weight:500;padding:3px 9px;border-radius:999px;white-space:nowrap;pointer-events:none;box-shadow:0 2px 8px rgba(0,113,227,.3);z-index:1}
.vh{position:absolute;width:1px;height:1px;overflow:hidden;clip:rect(0 0 0 0)}
a:focus-visible,input:focus-visible,summary:focus-visible{outline:2px solid var(--accent);outline-offset:2px}
@media (max-width:760px){.rail{width:72px}.stage{padding:14px 4px 32px 16px}.side,.inspector{max-width:none}.toolbar{padding:10px 16px}}
"##;

#[cfg(test)]
mod tests {
    use super::*;

    fn status(state: activity::State, since: i64, label: &str) -> activity::Status {
        activity::Status {
            state,
            text: "Done".into(),
            since,
            label: label.into(),
            why: String::new(),
            intent: String::new(),
        }
    }

    #[test]
    fn a_finished_agent_leaves_the_tree_after_its_minute_but_stays_finished() {
        let t = 10_000;
        let (recent, old) = (
            status(activity::State::Done, t - 30, ""),
            status(activity::State::Done, t - 600, ""),
        );
        let node = |s| Node {
            pubkey: "pk".into(),
            instance: "claude-1".into(),
            claim: None,
            last: None,
            status: Some(s),
        };
        assert!(shown(&node(&recent), t), "seen finishing");
        assert!(!shown(&node(&old), t), "gone from the tree");
        assert_eq!(
            line(&node(&old), t).tone,
            "done",
            "its own page still says it finished"
        );
    }

    fn empty_board<'a>(
        me: &'a str,
        tr: &'a tracker::Tracker,
        local: &'a [(String, activity::Status)],
    ) -> Board<'a> {
        Board {
            live: &[],
            posts: &[],
            all_live: &[],
            all_posts: &[],
            local,
            events: &[],
            me,
            plan: None,
            tracker: tr,
            here: "",
            agreement: None,
        }
    }

    /// A directive is offered only for your own agents, and only where it
    /// can be sent: a live page served with a token.
    #[test]
    fn the_send_form_offers_a_directive_only_to_your_own_agents() {
        let me = "pk-me";
        let tr = tracker::Tracker::none();
        let b = empty_board(me, &tr, &[]);
        let page = |who: &str, agent: &str| View {
            who: Some(who.into()),
            agent: agent.into(),
            project: "demo".into(),
            token: "tok".into(),
            ..View::default()
        };
        let mine = send_form(&b, &page(&name_for(me), "claude-1"));
        assert!(
            mine.contains("value=directive") && mine.contains("value=\"tok\""),
            "{mine}"
        );
        assert!(
            mine.contains(&format!("action=\"/demo/{}/claude-1\"", name_for(me))),
            "{mine}"
        );

        let theirs = send_form(&b, &page("quiet-heron", "claude-1"));
        assert!(!theirs.contains("value=directive"), "{theirs}");
        assert!(
            theirs.contains("Only quiet-heron&#39;s person")
                || theirs.contains("Only quiet-heron's person"),
            "{theirs}"
        );

        let sub = send_form(&b, &page("quiet-heron", "claude-1/explore-2"));
        assert!(sub.contains("Goes to quiet-heron/claude-1"), "{sub}");

        let past = View {
            at: Some(5),
            ..page(&name_for(me), "claude-1")
        };
        assert!(send_form(&b, &past).is_empty(), "the past is read-only");
        let tokenless = View {
            token: String::new(),
            ..page(&name_for(me), "claude-1")
        };
        assert!(send_form(&b, &tokenless).is_empty());
        assert!(
            send_form(
                &b,
                &View {
                    token: "tok".into(),
                    ..View::default()
                }
            )
            .is_empty(),
            "nobody to send to"
        );
    }

    /// The JSON form names the same agents and entries the page shows.
    #[test]
    fn the_page_as_data_scopes_like_the_page() {
        let me = "pk-me";
        let tr = tracker::Tracker::none();
        let local = vec![
            (
                "claude-1".to_string(),
                status(activity::State::Working, 90, ""),
            ),
            (
                "claude-2".to_string(),
                status(activity::State::Working, 90, ""),
            ),
        ];
        let b = empty_board(me, &tr, &local);
        let view = View {
            who: Some(name_for(me)),
            agent: "claude-1".into(),
            project: "demo".into(),
            prev: Some(40),
            ..View::default()
        };
        let v = json(&b, 100, &view);
        let paths: Vec<&str> = v["agents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["path"].as_str().unwrap())
            .collect();
        assert_eq!(paths, [format!("{}/claude-1", name_for(me))]);
        assert_eq!(v["agents"][0]["state"], "work");
        assert_eq!(v["path"], format!("/{}/claude-1", name_for(me)));
        assert_eq!(v["prev"], 40);
        assert!(v["entries"].as_array().unwrap().is_empty());
    }

    #[test]
    fn subagents_sort_into_in_progress_and_done() {
        let me = "pk-me";
        let working = status(activity::State::Working, 90, "Decode space form of ?at=");
        let done = status(activity::State::Done, 10, "Fix stale REFERENCE line");
        let tr = tracker::Tracker::none();
        let local = vec![
            (
                "claude-1".to_string(),
                status(activity::State::Working, 50, ""),
            ),
            ("claude-1/general-purpose-1".to_string(), working.clone()),
            ("claude-1/general-purpose-2".to_string(), done.clone()),
        ];
        let b = Board {
            live: &[],
            posts: &[],
            all_live: &[],
            all_posts: &[],
            local: &local,
            events: &[],
            me,
            plan: None,
            tracker: &tr,
            here: "",
            agreement: None,
        };
        let ns = nodes(&b, 100);
        let cfg = Cfg {
            url: String::new(),
            alias: "mac".into(),
            instance: String::new(),
            codebase: None,
        };
        let html = tree(&b, &ns, &cfg, 100, &View::default());
        let progress = html
            .find("In progress <span class=count>1</span>")
            .expect("in progress group");
        let done_at = html
            .find("Done <span class=count>1</span>")
            .expect("done group");
        assert!(progress < done_at);
        assert!(
            !html[done_at - 80..done_at].contains(" open"),
            "done starts folded"
        );
        assert!(
            html.contains("Decode space form of ?at="),
            "labelled by what it was started for"
        );
        assert!(
            html.contains("<span class=aid>general-purpose-1</span>"),
            "its id stays beside the label"
        );
    }

    #[test]
    fn the_rail_marks_the_moment_and_ends_at_now() {
        let view = View {
            who: Some("w".into()),
            at: Some(200),
            epochs: vec![100, 200, 300],
            prev: Some(100),
            next: Some(300),
            ..View::default()
        };
        let html = rail(&view);
        assert_eq!(
            html.matches("class=\"rtk").count(),
            4,
            "one per event, then Now"
        );
        assert!(html.contains(r#"href="/w?at=200" "#) || html.contains(r#"cur" href="/w?at=200""#));
        assert!(
            html.contains(r#"value=1"#),
            "the slider sits on the moment shown"
        );
        assert!(html.contains(r#"href="/w?at=100" aria-label="Earlier event""#));
        let live = rail(&View {
            epochs: vec![100],
            ..View::default()
        });
        assert!(
            live.contains("value=1") && live.contains(r#"class="rtk day now cur""#),
            "a live page sits on Now"
        );
    }

    #[test]
    fn showing_every_project_survives_every_link() {
        let view = View {
            who: Some("w".into()),
            at: Some(200),
            all: true,
            epochs: vec![100, 200],
            prev: Some(100),
            ..View::default()
        };
        assert_eq!(view.moment(), "?at=200&all=1");
        let html = rail(&view);
        assert!(
            html.contains("href=\"/w?at=100&amp;all=1\""),
            "ticks and arrows keep it"
        );
        assert!(html.contains("href=\"/w?all=1\""), "and so does Now");
        let sw = switch(&view, "robofinger");
        assert!(
            sw.contains("<b>All</b>") && sw.contains("href=\"/w?at=200\""),
            "the switch flips it back, keeping the moment"
        );
        let here = switch(&View::default(), "robofinger");
        assert!(here.contains("<b>robofinger</b>") && here.contains("href=\"/?all=1\""));
    }

    #[test]
    fn elapsed_shows_seconds_while_they_matter() {
        assert_eq!(elapsed(42), "42s");
        assert_eq!(elapsed(134), "2m 14s");
        assert_eq!(elapsed(1200), "20m");
        assert_eq!(elapsed(7500), "2h 5m");
        assert_eq!(elapsed(-3), "0s");
    }

    #[test]
    fn scope_takes_the_agent_and_its_subagents_not_its_siblings() {
        let me = "pk-me";
        let view = View {
            who: Some(name_for(me)),
            agent: "claude-1".into(),
            ..View::default()
        };
        assert!(in_scope(&view, me, "claude-1"));
        assert!(in_scope(&view, me, "claude-1/explore-2"));
        assert!(!in_scope(&view, me, "claude-10"));
        assert!(!in_scope(&view, "pk-other", "claude-1"));
        assert!(in_scope(&View::default(), "pk-other", "x"));
        let by_label = View {
            who: Some("macbook".into()),
            ..View::default()
        };
        assert!(
            !in_scope(&by_label, me, "claude-1"),
            "a local label is not an address"
        );
    }
}
