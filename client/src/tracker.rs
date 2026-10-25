//! Where a work item lives: links to it, and the project's plan as the tracker
//! has it.
//!
//! Links come from a template, `ROBOFINGER_TICKET_URL`, the way commit links
//! come from `ROBOFINGER_COMMIT_URL`: `{id}` takes the number from `#123` and
//! `{key}` takes a key like `ENG-123`. With no template, GitHub, GitLab and
//! Bitbucket are derived from `origin`. Setting a `{key}` template is also
//! what makes a bare `ENG-123` count as a ticket: it is how ADRs and RFCs are
//! numbered too, so only a team that says its tracker uses keys gets that.
//!
//! The plan — milestones or phases, and the issues in each — is read with
//! `gh` when `origin` is on GitHub, using the person's own credentials. Other
//! trackers still get links; their plan is whatever the record cites.

use super::{config_file, origin_base, ticket_ref};

pub struct Tracker {
    template: Option<String>,
}

impl Tracker {
    pub fn load() -> Self {
        let configured = std::env::var("ROBOFINGER_TICKET_URL")
            .ok()
            .or_else(|| config_file().get("ROBOFINGER_TICKET_URL").cloned())
            .filter(|s| !s.is_empty());
        Tracker {
            template: configured.or_else(|| origin_base().and_then(|b| default_template(&b))),
        }
    }

    /// No tracker: tickets are still found, but link only where the record
    /// carried a URL.
    #[cfg(test)]
    pub fn none() -> Self {
        Tracker { template: None }
    }

    #[cfg(test)]
    pub fn with(template: &str) -> Self {
        Tracker {
            template: Some(template.into()),
        }
    }

    /// The ticket a task names, as (key, link).
    pub fn find(&self, task: &str) -> Option<(String, Option<String>)> {
        let (key, link) = ticket_ref(task).or_else(|| self.bare_key(task))?;
        let link = link.or_else(|| self.link(&key));
        Some((key, link))
    }

    pub fn link(&self, key: &str) -> Option<String> {
        let t = self.template.as_ref()?;
        match key.strip_prefix('#') {
            Some(n) if t.contains("{id}") => Some(t.replace("{id}", n)),
            None if is_key(key) && t.contains("{key}") => Some(t.replace("{key}", key)),
            _ => None,
        }
    }

    fn bare_key(&self, task: &str) -> Option<(String, Option<String>)> {
        if !self.template.as_ref()?.contains("{key}") {
            return None;
        }
        task.split_whitespace()
            .map(|w| w.trim_matches(|c: char| "()[]{},.;:\"'".contains(c)))
            .find(|w| is_key(w))
            .map(|w| (w.to_string(), None))
    }
}

/// `ENG-123`: an uppercase project key, a dash, a number.
pub fn is_key(id: &str) -> bool {
    id.split_once('-').is_some_and(|(p, n)| {
        p.starts_with(|c: char| c.is_ascii_uppercase())
            && p.bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
            && !n.is_empty()
            && n.bytes().all(|b| b.is_ascii_digit())
    })
}

fn default_template(base: &str) -> Option<String> {
    let host = base
        .split('/')
        .nth(2)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let seg = match host.as_str() {
        h if h.ends_with("github.com") => "/issues/",
        h if h.ends_with("gitlab.com") => "/-/issues/",
        h if h.ends_with("bitbucket.org") => "/issues/",
        _ => return None,
    };
    Some(format!("{base}{seg}{{id}}"))
}

/// One work item from the tracker.
#[derive(Clone, Debug, PartialEq)]
pub struct Issue {
    /// `#25`, the form claims cite.
    pub key: String,
    pub title: String,
    pub closed: bool,
    pub url: String,
}

/// A milestone or phase and the issues in it.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub name: String,
    pub about: String,
    pub issues: Vec<Issue>,
}

/// The project's plan from GitHub, in the order it is meant to land: by
/// milestone, earliest due first; failing milestones, by `phase-*` label.
/// None when `origin` is not GitHub or `gh` cannot answer.
pub fn plan() -> Option<Vec<Group>> {
    let base = origin_base()?;
    if !base
        .split('/')
        .nth(2)?
        .to_ascii_lowercase()
        .ends_with("github.com")
    {
        return None;
    }
    let out = run_with_timeout(
        std::process::Command::new("gh")
            .current_dir(super::repo_root())
            .env("GH_PROMPT_DISABLED", "1")
            .args([
                "issue",
                "list",
                "--state",
                "all",
                "--limit",
                "500",
                "--json",
                "number,title,state,url,labels,milestone",
            ]),
        std::time::Duration::from_secs(8),
    )?;
    groups(&serde_json::from_slice(&out).ok()?)
}

/// `gh issue list --json` output to groups. Separate so it is testable.
pub fn groups(issues: &serde_json::Value) -> Option<Vec<Group>> {
    let issues = issues.as_array()?;
    let s = |v: &serde_json::Value, k: &str| {
        v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
    };
    let item = |v: &serde_json::Value| Issue {
        key: format!("#{}", v.get("number").and_then(|n| n.as_u64()).unwrap_or(0)),
        title: s(v, "title"),
        closed: s(v, "state").eq_ignore_ascii_case("closed"),
        url: s(v, "url"),
    };
    // (name, about, due, issues)
    let mut out: Vec<(String, String, String, Vec<Issue>)> = Vec::new();
    let mut add = |name: String, about: String, due: String, i: Issue| match out
        .iter_mut()
        .find(|g| g.0 == name)
    {
        Some(g) => g.3.push(i),
        None => out.push((name, about, due, vec![i])),
    };
    let by_milestone = issues
        .iter()
        .any(|v| v.get("milestone").is_some_and(|m| m.is_object()));
    let mut loose = Vec::new();
    for v in issues {
        let group = if by_milestone {
            v.get("milestone")
                .filter(|m| m.is_object())
                .map(|m| (s(m, "title"), s(m, "description"), s(m, "dueOn")))
        } else {
            v.get("labels")
                .and_then(|l| l.as_array())
                .and_then(|ls| ls.iter().find(|l| s(l, "name").starts_with("phase-")))
                .map(|l| (s(l, "name"), s(l, "description"), String::new()))
        };
        match group {
            Some((name, about, due)) => add(name, about, due, item(v)),
            None => loose.push(item(v)),
        }
    }
    if out.is_empty() && loose.is_empty() {
        return None;
    }
    // Due date first (none last), then the name read the way people number
    // things, so phase-10 follows phase-9.
    out.sort_by(|a, b| {
        (a.2.is_empty(), &a.2, natural(&a.0)).cmp(&(b.2.is_empty(), &b.2, natural(&b.0)))
    });
    let mut groups: Vec<Group> = out
        .into_iter()
        .map(|(name, about, _, mut issues)| {
            issues.sort_by_key(|i| (i.closed, i.key[1..].parse::<u64>().unwrap_or(0)));
            Group {
                name,
                about,
                issues,
            }
        })
        .collect();
    // What the plan does not place: the open part only, newest first, so a
    // big backlog is a short list rather than the whole page.
    let open: Vec<Issue> = loose.into_iter().filter(|i| !i.closed).take(30).collect();
    if !open.is_empty() {
        let name = if groups.is_empty() {
            "Open issues"
        } else {
            "Not in the plan"
        };
        groups.push(Group {
            name: name.into(),
            about: String::new(),
            issues: open,
        });
    }
    Some(groups)
}

/// A sort key that compares runs of digits as numbers.
pub fn natural(s: &str) -> Vec<(u64, String)> {
    let mut out = Vec::new();
    let mut text = String::new();
    let mut num = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() {
            num.push(c);
        } else {
            if !num.is_empty() {
                out.push((num.parse().unwrap_or(0), std::mem::take(&mut text)));
                num.clear();
            }
            text.push(c);
        }
    }
    out.push((num.parse().unwrap_or(u64::MAX), text));
    out
}

/// Run a command, giving up after `limit`. A tracker that hangs must cost the
/// dashboard its plan, never the page.
fn run_with_timeout(
    cmd: &mut std::process::Command,
    limit: std::time::Duration,
) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    // Read while it runs: a large answer fills the pipe, and a child blocked
    // on a full pipe never exits.
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let deadline = std::time::Instant::now() + limit;
    let ok = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(25))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break false;
            }
        }
    };
    let out = reader.join().ok()?;
    ok.then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_number_links_through_an_id_template_and_a_key_through_a_key_template() {
        let gh = Tracker::with("https://github.com/o/r/issues/{id}");
        assert_eq!(
            gh.find("users tab #88"),
            Some((
                "#88".into(),
                Some("https://github.com/o/r/issues/88".into())
            ))
        );
        assert_eq!(
            gh.find("per ENG-12"),
            None,
            "a key is not a ticket unless the tracker uses keys"
        );

        let jira = Tracker::with("https://acme.atlassian.net/browse/{key}");
        assert_eq!(
            jira.find("fix login (ENG-12)"),
            Some((
                "ENG-12".into(),
                Some("https://acme.atlassian.net/browse/ENG-12".into())
            ))
        );
        assert_eq!(
            jira.find("#88"),
            Some(("#88".into(), None)),
            "no {{id}}, so no link to guess"
        );

        let pasted = "https://linear.app/acme/issue/ENG-7/slug";
        assert_eq!(
            Tracker::none().find(pasted).unwrap().1.as_deref(),
            Some(pasted),
            "a pasted URL is its own link"
        );
    }

    #[test]
    fn forges_get_a_template_from_origin() {
        assert_eq!(
            default_template("https://github.com/o/r").as_deref(),
            Some("https://github.com/o/r/issues/{id}")
        );
        assert_eq!(
            default_template("https://gitlab.com/o/r").as_deref(),
            Some("https://gitlab.com/o/r/-/issues/{id}")
        );
        assert_eq!(default_template("https://git.example.com/o/r"), None);
    }

    #[test]
    fn phases_come_in_the_order_they_are_numbered() {
        let issue = |n: u64, state: &str, label: Option<&str>| {
            serde_json::json!({
                "number": n, "title": format!("t{n}"), "state": state,
                "url": format!("https://github.com/o/r/issues/{n}"),
                "labels": label.map(|l| vec![serde_json::json!({"name": l, "description": "about"})]).unwrap_or_default(),
                "milestone": null
            })
        };
        let json = serde_json::json!([
            issue(30, "OPEN", Some("phase-10-later")),
            issue(20, "OPEN", Some("phase-2-next")),
            issue(3, "CLOSED", Some("phase-1-first")),
            issue(1, "OPEN", Some("phase-1-first")),
            issue(9, "OPEN", None),
            issue(8, "CLOSED", None),
        ]);
        let g = groups(&json).unwrap();
        let names: Vec<&str> = g.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "phase-1-first",
                "phase-2-next",
                "phase-10-later",
                "Not in the plan"
            ]
        );
        assert_eq!(
            g[0].issues
                .iter()
                .map(|i| i.key.as_str())
                .collect::<Vec<_>>(),
            ["#1", "#3"],
            "open first"
        );
        assert_eq!(
            g[3].issues.len(),
            1,
            "only the open part of what the plan does not place"
        );
    }

    #[test]
    fn milestones_win_over_labels_and_sort_by_due_date() {
        let json = serde_json::json!([
            {"number": 1, "title": "a", "state": "OPEN", "url": "u1", "labels": [{"name": "phase-1"}],
             "milestone": {"title": "Beta", "description": "", "dueOn": "2026-12-01T00:00:00Z"}},
            {"number": 2, "title": "b", "state": "OPEN", "url": "u2", "labels": [],
             "milestone": {"title": "Alpha", "description": "", "dueOn": "2026-11-01T00:00:00Z"}},
            {"number": 3, "title": "c", "state": "OPEN", "url": "u3", "labels": [],
             "milestone": {"title": "Someday", "description": "", "dueOn": null}},
        ]);
        let names: Vec<String> = groups(&json).unwrap().into_iter().map(|g| g.name).collect();
        assert_eq!(names, ["Alpha", "Beta", "Someday"]);
    }

    #[test]
    fn a_hung_command_is_given_up_on() {
        let t = std::time::Instant::now();
        let out = run_with_timeout(
            std::process::Command::new("sleep").arg("5"),
            std::time::Duration::from_millis(200),
        );
        assert!(out.is_none());
        assert!(t.elapsed() < std::time::Duration::from_secs(2));
    }
}
