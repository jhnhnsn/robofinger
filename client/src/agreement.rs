//! The working agreement: `AGREEMENT.md` at the repo root, saying whose areas
//! are whose, where anyone may work, and where to ask first (ADR 0006).
//!
//! Guidance, never enforcement. robofinger reads it to tell agents which areas
//! are their person's at session start, to name an area's owner before an
//! edit lands in it, and to show it on the dashboard. Nothing is refused.
//!
//! ```markdown
//! ## Areas
//! - lilac-viper (John): robofinger in general — client/**, relay/**, docs/**
//! - quiet-heron (Ana): the local archive — client/src/archive.rs
//!
//! ## Play anywhere
//! Tests, typos, docs/agents/**
//!
//! ## Ask first
//! - relay/cloudflare-d1/schema.sql (deployed; changes need a migration)
//! ```
//!
//! Written for people first, so the parser is forgiving: unknown sections
//! and lines it cannot read are skipped, not errors.

pub const FILE: &str = "AGREEMENT.md";

#[derive(Clone, Debug, PartialEq)]
pub struct Area {
    /// The owner's robofinger name, the one their key derives to.
    pub owner: String,
    /// What people call them, from the parentheses; may be empty.
    pub person: String,
    pub about: String,
    pub globs: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Agreement {
    pub areas: Vec<Area>,
    /// Paths anyone may work in.
    pub anywhere: Vec<String>,
    /// The "Play anywhere" items that are not paths: "tests", "typos".
    pub anywhere_notes: Vec<String>,
    /// (glob, why) pairs: ask before changing these, whoever owns them.
    pub ask_first: Vec<(String, String)>,
    /// A short hash of the file, which acceptance is recorded against.
    pub version: String,
}

/// The agreement at `root`, if the repo has one.
pub fn load(root: &std::path::Path) -> Option<Agreement> {
    std::fs::read_to_string(root.join(FILE))
        .ok()
        .map(|t| parse(&t))
}

pub fn parse(text: &str) -> Agreement {
    let mut a = Agreement {
        version: version(text),
        ..Default::default()
    };
    let mut section = String::new();
    for raw in text.lines() {
        let line = raw.trim();
        if let Some(h) = line.strip_prefix('#') {
            section = h.trim_start_matches('#').trim().to_ascii_lowercase();
            continue;
        }
        let item = line.trim_start_matches(['-', '*']).trim();
        if item.is_empty() || item.starts_with("<!--") {
            continue;
        }
        if section.starts_with("area") {
            if let Some(area) = parse_area(item) {
                a.areas.push(area);
            }
        } else if section.starts_with("play anywhere") || section.starts_with("anywhere") {
            for part in item.split(',').map(str::trim).filter(|p| !p.is_empty()) {
                if looks_like_path(part) {
                    a.anywhere.push(part.trim_matches('`').to_string());
                } else {
                    a.anywhere_notes.push(part.to_string());
                }
            }
        } else if section.starts_with("ask first") {
            let (glob, why) = match item.split_once('(') {
                Some((g, w)) => (g.trim(), w.trim_end_matches(')').trim()),
                None => (item, ""),
            };
            for g in glob
                .split(',')
                .map(|g| g.trim().trim_matches('`'))
                .filter(|g| !g.is_empty())
            {
                a.ask_first.push((g.to_string(), why.to_string()));
            }
        }
    }
    a
}

/// `owner (person): about — globs`. The person and the about are optional;
/// the dash may be an em dash or `--`.
fn parse_area(item: &str) -> Option<Area> {
    let (head, globs) = item
        .split_once('—')
        .or_else(|| item.split_once("--"))
        .or_else(|| item.split_once(" - "))?;
    let globs: Vec<String> = globs
        .split([',', ' '])
        .map(|g| g.trim().trim_matches('`'))
        .filter(|g| !g.is_empty())
        .map(String::from)
        .collect();
    let (who, about) = head.split_once(':').unwrap_or((head, ""));
    let (owner, person) = match who.split_once('(') {
        Some((o, p)) => (o.trim(), p.trim_end().trim_end_matches(')').trim()),
        None => (who.trim(), ""),
    };
    if owner.is_empty() || globs.is_empty() {
        return None;
    }
    Some(Area {
        owner: owner.to_string(),
        person: person.to_string(),
        about: about.trim().to_string(),
        globs,
    })
}

fn looks_like_path(s: &str) -> bool {
    s.contains('/') || s.contains('*') || s.starts_with('`')
}

/// Eight hex characters of FNV-1a: enough to tell versions apart, short enough
/// to read out.
fn version(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{:08x}", h >> 32)
}

fn matches(glob: &str, path: &str) -> bool {
    glob::Pattern::new(glob).map(|p| p.matches(path)).unwrap_or(false)
        || path == glob
        // `client/**` should cover `client` itself, and a claim's literal
        // prefix (what is left of `client/src/*` before the star).
        || glob.strip_suffix("/**").is_some_and(|d| path == d || path.starts_with(&format!("{d}/")))
}

impl Agreement {
    /// The area a path is in: the most specific one, so a carve-out like
    /// `client/src/archive.rs` wins over `client/**`.
    pub fn area_of(&self, path: &str) -> Option<&Area> {
        self.areas
            .iter()
            .filter_map(|a| {
                a.globs
                    .iter()
                    .filter(|g| matches(g, path))
                    .map(|g| g.trim_end_matches(['*', '/']).len())
                    .max()
                    .map(|len| (len, a))
            })
            .max_by_key(|(len, _)| *len)
            .map(|(_, a)| a)
    }

    pub fn ask_first_for(&self, path: &str) -> Option<&(String, String)> {
        self.ask_first.iter().find(|(g, _)| matches(g, path))
    }

    pub fn open_to_anyone(&self, path: &str) -> bool {
        self.anywhere.iter().any(|g| matches(g, path))
    }

    /// What an agent is told at session start: its person's areas first,
    /// then everyone else's, where anyone may work, and where to ask.
    pub fn briefing(&self, me: &str) -> String {
        let fmt = |a: &Area| {
            let who = if a.person.is_empty() {
                a.owner.clone()
            } else {
                format!("{} ({})", a.owner, a.person)
            };
            let about = if a.about.is_empty() {
                String::new()
            } else {
                format!(": {}", a.about)
            };
            format!("{who}{about} — {}", a.globs.join(", "))
        };
        let mut out = vec![format!(
            "The working agreement (AGREEMENT.md, version {}). Guidance for agents and their people, not rules:",
            self.version
        )];
        let (mine, theirs): (Vec<&Area>, Vec<&Area>) =
            self.areas.iter().partition(|a| a.owner == me);
        if !mine.is_empty() {
            out.push(format!(
                "  Yours: {}",
                mine.iter().map(|a| fmt(a)).collect::<Vec<_>>().join("; ")
            ));
        }
        if !theirs.is_empty() {
            out.push(format!(
                "  Others': {}. Ask before working in someone else's area.",
                theirs.iter().map(|a| fmt(a)).collect::<Vec<_>>().join("; ")
            ));
        }
        let anywhere: Vec<String> = self
            .anywhere_notes
            .iter()
            .chain(self.anywhere.iter())
            .cloned()
            .collect();
        if !anywhere.is_empty() {
            out.push(format!("  Anyone may work on: {}", anywhere.join(", ")));
        }
        if !self.ask_first.is_empty() {
            let items: Vec<String> = self
                .ask_first
                .iter()
                .map(|(g, why)| {
                    if why.is_empty() {
                        g.clone()
                    } else {
                        format!("{g} ({why})")
                    }
                })
                .collect();
            out.push(format!("  Ask a person first: {}", items.join("; ")));
        }
        out.join("\n")
    }

    /// A word before an edit lands somewhere it should not without asking, or
    /// None when the path is the agent's own, open to anyone, or unmapped.
    pub fn note_for(&self, path: &str, me: &str) -> Option<String> {
        if let Some((g, why)) = self.ask_first_for(path) {
            let why = if why.is_empty() {
                String::new()
            } else {
                format!(" ({why})")
            };
            return Some(format!(
                "{path} is under \"ask first\" in AGREEMENT.md: {g}{why}. Check with your person before changing it."
            ));
        }
        if self.open_to_anyone(path) {
            return None;
        }
        let a = self.area_of(path)?;
        if a.owner == me {
            return None;
        }
        let who = if a.person.is_empty() {
            a.owner.clone()
        } else {
            format!("{} ({})", a.owner, a.person)
        };
        let about = if a.about.is_empty() {
            String::new()
        } else {
            format!(": {}", a.about)
        };
        Some(format!(
            "{path} is in {who}'s area{about}, per AGREEMENT.md. Guidance, not a rule: ask them \
             (`robofinger ask --to {} \"…\"`) or your person before changing it, or work elsewhere.",
            a.owner
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "# Working agreement\n\n## Areas\n\
        - lilac-viper (John): robofinger in general — client/**, relay/**, docs/**\n\
        - quiet-heron (Ana): the local archive — client/src/archive.rs\n\n\
        ## Play anywhere\nTests, typos, docs/agents/**\n\n\
        ## Ask first\n- relay/cloudflare-d1/schema.sql (deployed; changes need a migration)\n";

    #[test]
    fn the_sample_parses_into_areas_and_guidance() {
        let a = parse(SAMPLE);
        assert_eq!(a.areas.len(), 2);
        assert_eq!(
            a.areas[1],
            Area {
                owner: "quiet-heron".into(),
                person: "Ana".into(),
                about: "the local archive".into(),
                globs: vec!["client/src/archive.rs".into()],
            }
        );
        assert_eq!(a.anywhere, ["docs/agents/**"]);
        assert_eq!(a.anywhere_notes, ["Tests", "typos"]);
        assert_eq!(a.ask_first[0].0, "relay/cloudflare-d1/schema.sql");
        assert_eq!(a.version.len(), 8);
        assert_ne!(
            a.version,
            parse(&SAMPLE.replace("Ana", "Anna")).version,
            "an edit is a new version"
        );
    }

    #[test]
    fn the_most_specific_area_wins() {
        let a = parse(SAMPLE);
        assert_eq!(
            a.area_of("client/src/archive.rs").unwrap().owner,
            "quiet-heron"
        );
        assert_eq!(
            a.area_of("client/src/main.rs").unwrap().owner,
            "lilac-viper"
        );
        assert_eq!(
            a.area_of("client").unwrap().owner,
            "lilac-viper",
            "a claim's literal prefix"
        );
        assert!(a.area_of("README.md").is_none());
    }

    #[test]
    fn notes_come_only_where_they_help() {
        let a = parse(SAMPLE);
        let note = a.note_for("client/src/archive.rs", "lilac-viper").unwrap();
        assert!(
            note.contains("quiet-heron (Ana)'s area") && note.contains("ask --to quiet-heron"),
            "{note}"
        );
        assert!(
            a.note_for("client/src/main.rs", "lilac-viper").is_none(),
            "your own area"
        );
        assert!(
            a.note_for("docs/agents/x.md", "quiet-heron").is_none(),
            "open to anyone"
        );
        assert!(
            a.note_for("relay/cloudflare-d1/schema.sql", "lilac-viper")
                .unwrap()
                .contains("ask first"),
            "even in your own area"
        );
        assert!(a.note_for("README.md", "quiet-heron").is_none(), "unmapped");
    }

    #[test]
    fn the_briefing_puts_yours_first() {
        let b = parse(SAMPLE).briefing("quiet-heron");
        let yours = b.find("Yours: quiet-heron").unwrap();
        let others = b.find("Others': lilac-viper").unwrap();
        assert!(yours < others && b.contains("Ask a person first"), "{b}");
    }
}
