//! The live parts of the `serve` page: the agent tree with a status line per
//! agent, the activity feed, the selected agent's detail, and the project's
//! plan with who is on each item.
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
/// `ELAPSED_JS` must format exactly the same way.
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

/// What one agent is doing, ready to render.
struct Line {
    glyph: &'static str,
    cls: &'static str,
    text: String,
    since: Option<i64>,
    /// Words either side of the ticking duration: "idle 3m", "3m ago".
    pre: &'static str,
    post: &'static str,
}

fn line(n: &Node, t: i64) -> Line {
    use activity::State::*;
    if let Some(s) = n.status {
        let (glyph, cls, post) = match s.state {
            Working => ("✻", "st-work spin", ""),
            Thinking => ("✻", "st-think spin", ""),
            NeedsYou | YourTurn => ("‖", "st-wait", ""),
            Done | Gone => ("✓", "st-done", " ago"),
        };
        return Line {
            glyph,
            cls,
            text: s.text.clone(),
            since: Some(s.since),
            pre: "",
            post,
        };
    }
    if let Some(c) = n.claim {
        let quiet = stuck(c, t);
        return Line {
            glyph: "○",
            cls: if quiet { "st-quiet" } else { "st-claim" },
            text: format!("holds {}", c.touching.join(" ")),
            since: Some(c.epoch),
            pre: if quiet { "quiet " } else { "idle " },
            post: "",
        };
    }
    // A teammate's agent: the relay knows its last entry, not whether it is
    // still running, so say what that entry was.
    match n.last {
        Some(l) => Line {
            glyph: if l.kind == "done" { "✓" } else { "○" },
            cls: if l.kind == "done" { "st-done" } else { "st-claim" },
            text: match l.kind.as_str() {
                "done" => "finished",
                "release" => "released",
                _ => "last seen",
            }
            .into(),
            since: Some(l.epoch),
            pre: "",
            post: " ago",
        },
        None => Line {
            glyph: "",
            cls: "st-claim",
            text: String::new(),
            since: None,
            pre: "",
            post: "",
        },
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
    format!(
        r#"<span class="st {}"><span class=g>{}</span> {}{when}</span>"#,
        l.cls,
        l.glyph,
        esc(&l.text)
    )
}

fn ticket_html(task: &str, tr: &tracker::Tracker) -> String {
    match tr.find(task) {
        Some((key, Some(u))) => format!(r#" <a class=tk href="{}">{}</a>"#, esc(&u), esc(&key)),
        Some((key, None)) => format!(" <span class=tk>{}</span>", esc(&key)),
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

/// Whether an agent gets a row in the tree. One that finished more than a
/// minute ago has gone, unless it somehow still holds a claim.
pub fn shown(n: &Node, t: i64) -> bool {
    n.claim.is_some() || !n.status.is_some_and(|s| finished_long_ago(s, t))
}

/// Machine identity, then agent, then subagent, each level collapsible.
pub fn tree(b: &Board, ns: &[Node], c: &Cfg, t: i64, view: &View) -> String {
    let visible: Vec<&Node> = ns.iter().filter(|n| shown(n, t)).collect();
    let ns = &visible[..];
    if ns.is_empty() {
        return r#"<p class="none">Nobody on the record right now</p>"#.into();
    }
    let moment = view.moment();
    let selected = view.path();
    let mut keys: Vec<&str> = ns.iter().map(|n| n.pubkey.as_str()).collect();
    if keys.is_empty() {
        return r#"<p class="none">Nobody on the record right now</p>"#.into();
    }
    keys.sort_by_key(|k| (*k != b.me, name_for(k)));
    keys.dedup();
    keys.iter()
        .map(|pk| {
            let mine: Vec<&Node> = ns.iter().copied().filter(|n| n.pubkey == *pk).collect();
            let name = name_for(pk);
            let label = if *pk == b.me {
                let live = if view.at.is_none() { ", live" } else { "" };
                format!("{} · this machine{live}", c.alias)
            } else {
                local_label(pk).unwrap_or_default()
            };
            let sel = if selected == format!("/{name}") { " sel" } else { "" };
            let agents = branch(&mine, "", b, t, &moment, &selected);
            format!(
                r#"<details open data-id="{n}"><summary><span class="row id{sel}"><a class=n href="/{n}{moment}">{n}</a> <span class=dim>{}</span> <span class=sum>{}</span></span></summary><div class=kids>{agents}</div></details>"#,
                esc(&label),
                summary(&count(&mine)),
                n = esc(&name),
            )
        })
        .collect()
}

/// The agents directly under `parent` ("" for top level), and theirs.
fn branch(ns: &[&Node], parent: &str, b: &Board, t: i64, moment: &str, selected: &str) -> String {
    let depth = if parent.is_empty() {
        0
    } else {
        parent.matches('/').count() + 1
    };
    let mut here: Vec<&&Node> = ns
        .iter()
        .filter(|n| {
            n.instance.matches('/').count() == depth
                && (parent.is_empty() || n.instance.starts_with(&format!("{parent}/")))
                && !n.instance.is_empty()
        })
        .collect();
    // An identity with no instances at all is one headless agent.
    if parent.is_empty() && here.is_empty() {
        here = ns.iter().filter(|n| n.instance.is_empty()).collect();
    }
    here.sort_by_key(|n| tracker::natural(&n.instance));
    here.iter()
        .map(|n| {
            let path = n.path();
            let l = line(n, t);
            let fin = if l.cls == "st-done" { " fin" } else { "" };
            let sel = if selected == format!("/{path}") { " sel" } else { "" };
            let name = n.instance.rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or("(headless)");
            let ticket = n.claim.map(|c| ticket_html(&c.task, b.tracker)).unwrap_or_default();
            let row = format!(
                r#"<span class="row{sel}{fin}" data-agent="{p}"><a class=n href="/{p}{moment}">{}</a> {}{ticket}</span>"#,
                esc(name),
                line_html(&l, t),
                p = esc(&path),
            );
            let kids = if n.instance.is_empty() {
                String::new()
            } else {
                branch(ns, &n.instance, b, t, moment, selected)
            };
            if kids.is_empty() {
                format!("<div class=leaf>{row}</div>")
            } else {
                format!(
                    r#"<details open data-id="{}"><summary>{row}</summary><div class=kids>{kids}</div></details>"#,
                    esc(&path)
                )
            }
        })
        .collect()
}

/// What happened, newest first: agents appearing and finishing on this
/// machine, and everything on the timeline.
pub fn feed(b: &Board, t: i64, view: &View) -> String {
    let moment = view.moment();
    let mut items: Vec<(i64, String, String)> = Vec::new();
    for e in b.events.iter().filter(|e| in_scope(view, b.me, &e.agent)) {
        let text = match e.kind.as_str() {
            "appear" => "started".to_string(),
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
            "ask" => format!("asks: {}", p.task),
            "answer" => format!("answered: {}", p.task),
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
                r#"<div class=ev><span class="tm dim">{}</span> <a href="/{p}{moment}">{p}</a> {}</div>"#,
                when(*e, t),
                esc(&truncate(text, 140)),
                p = esc(path),
            )
        })
        .collect::<String>()
        + &format!("<p class=dim style=\"font-size:12px;margin:6px 0 0\">as of {}</p>", stamp(t))
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

/// The entity the URL names: what it is doing, what it holds, and the trail
/// of what it did.
pub fn detail(b: &Board, ns: &[Node], c: &Cfg, t: i64, view: &View) -> String {
    let Some(who) = &view.who else {
        return String::new();
    };
    let moment = view.moment();
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
            "<section><h2>Machine identity</h2><div class=big>{} <span class=dim>{}</span></div><p>{}</p><p class=dim>{} agents on the record now.</p></section>",
            esc(who),
            esc(&label),
            summary(&count(&mine)),
            mine.len()
        );
    }
    let path = format!("{who}/{}", view.agent);
    let Some(n) = ns.iter().find(|n| n.path() == path) else {
        return format!(
            "<section><h2>Agent</h2><div class=big>{}</div><p class=none>Nothing on the record for it at this moment.</p></section>",
            esc(&path)
        );
    };
    let link = |p: &str| format!(r#"<a href="/{p}{moment}">{p}</a>"#, p = esc(p));
    let mut facts: Vec<(&str, String)> = Vec::new();
    if let Some(cl) = n.claim {
        facts.push(("Task", esc(&cl.task)));
        let ticket = ticket_html(&cl.task, b.tracker);
        if !ticket.is_empty() {
            facts.push(("Ticket", ticket));
        }
        facts.push((
            "Claims",
            format!("<code>{}</code>", esc(&cl.touching.join(" "))),
        ));
    } else {
        facts.push(("Claims", "nothing".into()));
    }
    facts.push((
        "Seen via",
        if n.status.is_some() || n.pubkey == b.me {
            "this machine's hooks, live".into()
        } else {
            "the timeline: teammates' machines don't report live yet".into()
        },
    ));
    if let Some((parent, _)) = n.instance.rsplit_once('/') {
        facts.push(("Parent", link(&agent_path(&n.pubkey, parent))));
    }
    let kids: Vec<String> = ns
        .iter()
        .filter(|k| {
            k.pubkey == n.pubkey
                && k.instance
                    .rsplit_once('/')
                    .is_some_and(|(p, _)| p == n.instance)
        })
        .map(|k| link(&k.path()))
        .collect();
    if !kids.is_empty() {
        facts.push(("Subagents", kids.join(" ")));
    }
    let facts: String = facts
        .iter()
        .map(|(k, v)| format!("<dt>{k}</dt><dd>{v}</dd>"))
        .collect();
    let trail: Vec<String> = if n.pubkey == b.me {
        b.events
            .iter()
            .rev()
            .filter(|e| e.agent == n.instance)
            .take(14)
            .map(|e| {
                let text = match e.kind.as_str() {
                    "tool" => activity::doing(&e.tool, &e.target),
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
                    r#"<div class=ev><span class="tm dim">{}</span> {}</div>"#,
                    clock(e.t),
                    esc(&text)
                )
            })
            .collect()
    } else {
        Vec::new()
    };
    let trail = if trail.is_empty() {
        String::new()
    } else {
        format!("<h2>What it's doing</h2>{}", trail.join(""))
    };
    format!(
        "<section><h2>{}</h2><div class=big>{}</div><div class=bigline>{}</div><dl class=facts>{facts}</dl>{trail}</section>",
        if n.instance.contains('/') {
            "Subagent"
        } else {
            "Agent"
        },
        esc(&path),
        line_html(&line(n, t), t),
    )
}

/// The project's plan, in the order it lands, with the agents on each item.
pub fn plan(
    groups: &[tracker::Group],
    b: &Board,
    ns: &[Node],
    t: i64,
    view: &View,
    here: &str,
) -> String {
    let moment = view.moment();
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
            r#"<div class=who><a href="/{p}{moment}">{p}</a> {}</div>"#,
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
                        ("closed", "done")
                    } else if c.waiting > 0 {
                        waiting += 1;
                        ("waiting on you", "wait")
                    } else if !on.is_empty() {
                        active += 1;
                        ("in progress", "work")
                    } else {
                        ("open", "")
                    };
                    format!(
                        r#"<div class=issue><span class="chip {cls}">{chip}</span> <a class=num href="{}">{}</a> {}{}</div>"#,
                        esc(&i.url),
                        esc(&i.key),
                        esc(&i.title),
                        on.iter().map(|n| who_html(n)).collect::<String>()
                    )
                })
                .collect();
            let total = g.issues.len().max(1);
            let mut bits = vec![format!("{closed} of {} closed", g.issues.len())];
            if active > 0 {
                bits.push(format!("{active} in progress"));
            }
            if waiting > 0 {
                bits.push(format!("{waiting} waiting on you"));
            }
            let open = if active + waiting > 0 { " open" } else { "" };
            format!(
                r#"<details class=ms data-id="plan:{n}"{open}><summary><span class=msname>{n}</span> <span class="dim about">{}</span><div class=bar><div style="width:{}%"></div></div><div class="dim msum">{}</div></summary>{rows}</details>"#,
                esc(&g.about),
                100 * closed / total,
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
        format!("<h2 style=\"margin-top:10px\">Untracked</h2>{untracked}")
    };
    format!(
        "<section><div class=ph><h2 class=proj>{}</h2><span class=dim>plan</span></div>{body}{untracked}</section>",
        esc(here)
    )
}

/// Keeps a live page live: re-fetches it every two seconds and swaps the
/// board in, keeping what is folded folded, marking agents that just
/// appeared, and ticking every duration once a second in between.
pub const LIVE_JS: &str = r#"<script>
(function () {
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
      ["board", "summary"].forEach(function (id) {
        var a = document.getElementById(id), b = next.getElementById(id);
        if (a && b) a.replaceWith(b);
      });
      document.querySelectorAll("details[data-id]").forEach(function (d) {
        if (d.dataset.id in open) d.open = open[d.dataset.id];
      });
      document.querySelectorAll("[data-agent]").forEach(function (e) {
        if (!seen[e.dataset.agent]) e.classList.add("arriving");
      });
      tick();
    } catch (e) {}
  }
  setInterval(tick, 1000);
  setInterval(refresh, 2000);
})();
</script>"#;

pub const CSS: &str = r#"
.cols{display:flex;flex-wrap:wrap;gap:12px;align-items:flex-start}
.col-l{flex:1 1 360px;min-width:0}.col-m{flex:1.4 1 440px;min-width:0}.col-r{flex:1 1 320px;min-width:0}
details>summary{list-style:none;cursor:pointer}details>summary::-webkit-details-marker{display:none}
details>summary::before{content:"▸";display:inline-block;width:14px;color:var(--dim);transition:transform .15s}
details[open]>summary::before{transform:rotate(90deg)}
.kids{padding-left:16px}.leaf{padding-left:14px}
.row{display:inline-flex;flex-wrap:wrap;gap:4px 8px;align-items:baseline;padding:3px 6px;border-radius:3px}
.row.sel{background:color-mix(in srgb,var(--hi) 16%,transparent);outline:1px solid color-mix(in srgb,var(--hi) 45%,transparent)}
.row.id>a.n{font-weight:600}.row .sum{font-size:12px;color:var(--dim)}
a.n{font-family:ui-monospace,SFMono-Regular,Menlo,monospace;font-size:13px}
.st{font-family:ui-monospace,SFMono-Regular,Menlo,monospace;font-size:12px}
.st .g{display:inline-block;width:1em;text-align:center}
.spin .g{animation:spin 1.4s linear infinite}@keyframes spin{to{transform:rotate(360deg)}}
.st-work{color:var(--hi)}.st-think{color:color-mix(in srgb,var(--hi) 70%,var(--fg))}.st-wait{color:var(--wait)}
.st-done{color:var(--ok)}.st-claim{color:var(--dim)}.st-quiet{color:var(--bad)}
.fin{opacity:.6}.arriving{animation:arrive 1.8s ease-out}
@keyframes arrive{from{opacity:0;transform:translateX(-8px);background:color-mix(in srgb,var(--hi) 30%,transparent)}to{opacity:1;transform:none}}
.tk{font-size:12px}
.ev{font-size:13px;padding:3px 0;border-top:1px solid var(--line)}.ev .tm{font-size:12px;font-family:ui-monospace,monospace}
.ev a{font-family:ui-monospace,SFMono-Regular,Menlo,monospace;font-size:12px}
.big{font-family:ui-monospace,SFMono-Regular,Menlo,monospace;font-size:14px;word-break:break-all}.bigline{margin:8px 0}
.facts{display:grid;grid-template-columns:auto 1fr;gap:4px 12px;font-size:13px;margin:10px 0}.facts dt{color:var(--dim)}.facts dd{margin:0;word-break:break-word}
.ms{border-top:1px solid var(--line);padding:6px 0}.ms>summary{padding:2px 0}.msname{color:var(--hi);font-weight:600}.about{font-size:12px}
.bar{height:4px;background:var(--line);border-radius:2px;margin:5px 0 3px 14px;overflow:hidden}.bar>div{height:100%;background:var(--ok)}
.msum{font-size:12px;margin-left:14px}
.issue{padding:5px 0 5px 14px;border-top:1px solid var(--line);font-size:13px}.num{font-weight:600}
.chip{border:1px solid var(--dim);color:var(--dim);border-radius:3px;padding:0 5px;font-size:11px;white-space:nowrap}
.chip.work{border-color:var(--hi);color:var(--hi)}.chip.wait{border-color:var(--wait);color:var(--wait)}.chip.done{border-color:var(--ok);color:var(--ok)}
.who{padding:2px 0 0 8px}.who a{font-family:ui-monospace,SFMono-Regular,Menlo,monospace;font-size:12px}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_finished_agent_leaves_the_tree_after_its_minute_but_stays_finished() {
        let t = 10_000;
        let done = |since| activity::Status { state: activity::State::Done, text: "Done".into(), since };
        let (recent, old) = (done(t - 30), done(t - 600));
        let node = |s| Node { pubkey: "pk".into(), instance: "claude-1/general-1".into(), claim: None, last: None, status: Some(s) };
        assert!(shown(&node(&recent), t), "seen finishing");
        assert!(!shown(&node(&old), t), "gone from the tree");
        assert_eq!(line(&node(&old), t).cls, "st-done", "its own page still says it finished");
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
        let by_label = View { who: Some("macbook".into()), ..View::default() };
        assert!(!in_scope(&by_label, me, "claude-1"), "a local label is not an address");
    }
}
