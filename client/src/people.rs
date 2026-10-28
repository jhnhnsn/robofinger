//! People, their computers, and what to call them (ADR 0009).
//!
//! A computer is one key. A person is the computers that name each other:
//! each lists the others in everything it publishes, signed with its own key,
//! so neither can claim the other alone. Nicknames for both travel the same
//! way, as suggestions. Your own nicknames for anyone win on your screen, and
//! a suggestion that would put a stranger under a name you already use for
//! someone else is ignored. The key-derived name is never replaced, only
//! annotated, so the fingerprint stays on every line.

use super::*;
use std::collections::HashMap;

/// What a computer says about itself and its person, in everything it
/// publishes.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Me {
    /// The person's suggested nickname: "John".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub person: String,
    /// This computer's suggested nickname: "laptop".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub computer: String,
    /// The keys of the person's other computers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub computers: Vec<String>,
}

impl Me {
    pub fn is_empty(&self) -> bool {
        self.person.is_empty() && self.computer.is_empty() && self.computers.is_empty()
    }
}

fn me_path() -> std::path::PathBuf {
    crypto::config_dir().join("me")
}

/// What this computer says about itself. Nothing until a nickname is set or
/// another computer is linked.
pub fn load() -> Me {
    std::fs::read_to_string(me_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(me: &Me) -> Result<(), String> {
    let dir = crypto::config_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(
        me_path(),
        serde_json::to_string_pretty(me).unwrap_or_default(),
    )
    .map_err(|e| format!("write {}: {e}", me_path().display()))?;
    with_book(|b| b.own = me.clone());
    Ok(())
}

/// What goes into a published entry: this computer's self-description, when
/// it has one.
pub fn published() -> Option<Me> {
    Some(load()).filter(|m| !m.is_empty())
}

/// Link `other` to this computer, and every computer it is linked with.
pub fn link(others: &[String], own_key: &str) -> Result<(), String> {
    let mut me = load();
    for o in others {
        if o != own_key && !me.computers.contains(o) {
            me.computers.push(o.clone());
        }
    }
    save(&me)
}

/// Everything needed to name anyone, from the files that say it.
#[derive(Default)]
pub struct Book {
    /// This computer's key.
    pub me: String,
    pub own: Me,
    /// Every other key's latest self-description, and when it said it.
    pub profiles: HashMap<String, (i64, Me)>,
    /// Your nicknames for people, by any one of their computers' keys.
    pub people: HashMap<String, String>,
    /// Your nicknames for computers.
    pub computers: HashMap<String, String>,
    /// The labels you filed peers under (`add --as`).
    pub labels: HashMap<String, String>,
}

impl Book {
    fn profile(&self, k: &str) -> Option<&Me> {
        if k == self.me {
            Some(&self.own)
        } else {
            self.profiles.get(k).map(|(_, m)| m)
        }
    }

    /// Whether two computers each name the other as their person's.
    pub fn linked(&self, a: &str, b: &str) -> bool {
        let names = |x: &str, y: &str| {
            self.profile(x)
                .is_some_and(|m| m.computers.iter().any(|c| c == y))
        };
        a != b && names(a, b) && names(b, a)
    }

    pub fn same_person(&self, a: &str, b: &str) -> bool {
        a == b || self.linked(a, b)
    }

    /// A key and every computer linked with it, sorted.
    pub fn computers_of(&self, k: &str) -> Vec<String> {
        let mut out = vec![k.to_string()];
        if let Some(m) = self.profile(k) {
            out.extend(m.computers.iter().filter(|c| self.linked(k, c)).cloned());
        }
        out.sort();
        out.dedup();
        out
    }

    /// One key that stands for the whole person, whichever computer asks.
    pub fn person_key(&self, k: &str) -> String {
        self.computers_of(k).into_iter().next().unwrap_or_default()
    }

    /// What to call the person behind `k`: your nickname for them, then
    /// theirs for themselves unless it is already someone else's, then the
    /// label you filed them under.
    pub fn person(&self, k: &str) -> Option<String> {
        let mine = self.computers_of(k);
        let chosen = |map: &HashMap<String, String>| {
            mine.iter()
                .find_map(|c| map.get(c).filter(|v| !v.is_empty()).cloned())
        };
        if let Some(n) = chosen(&self.people) {
            return Some(n);
        }
        let suggested = mine
            .iter()
            .filter_map(|c| self.profile(c).map(|m| m.person.trim()))
            .find(|p| !p.is_empty())
            .map(String::from);
        if let Some(s) = suggested.filter(|s| !self.taken(s, k)) {
            return Some(s);
        }
        chosen(&self.labels)
    }

    /// Whether `name` is what you already call someone other than `k`'s person.
    fn taken(&self, name: &str, k: &str) -> bool {
        let other = |key: &str| !self.same_person(key, k);
        let called = |map: &HashMap<String, String>| {
            map.iter()
                .any(|(key, v)| other(key) && v.eq_ignore_ascii_case(name))
        };
        called(&self.people)
            || called(&self.labels)
            || (other(&self.me) && self.own.person.eq_ignore_ascii_case(name))
    }

    /// What to call the computer `k`: your nickname for it, else its own.
    pub fn computer(&self, k: &str) -> Option<String> {
        self.computers
            .get(k)
            .filter(|v| !v.is_empty())
            .cloned()
            .or_else(|| {
                self.profile(k)
                    .map(|m| m.computer.clone())
                    .filter(|c| !c.is_empty())
            })
    }

    /// "John · laptop", "John", "laptop", or None when nobody named either.
    pub fn nickname(&self, k: &str) -> Option<String> {
        match (self.person(k), self.computer(k)) {
            (Some(p), Some(c)) => Some(format!("{p} · {c}")),
            (Some(p), None) => Some(p),
            (None, Some(c)) => Some(c),
            (None, None) => None,
        }
    }
}

fn profiles_path() -> std::path::PathBuf {
    crypto::config_dir().join("profiles")
}

fn computer_names_path() -> std::path::PathBuf {
    crypto::config_dir().join("computer-names")
}

/// `pubkey<TAB>nickname` lines, as the `names` file has always been.
fn read_pairs(path: &std::path::Path) -> HashMap<String, String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .filter(|(k, v)| !k.is_empty() && !v.is_empty())
        .collect()
}

pub fn write_pairs(path: &std::path::Path, pairs: &HashMap<String, String>) -> Result<(), String> {
    let mut rows: Vec<_> = pairs.iter().filter(|(_, v)| !v.is_empty()).collect();
    rows.sort();
    let body: String = rows.iter().map(|(k, v)| format!("{k}\t{v}\n")).collect();
    std::fs::write(path, body).map_err(|e| format!("write {}: {e}", path.display()))
}

static BOOK: std::sync::Mutex<Option<Book>> = std::sync::Mutex::new(None);

/// Which key is this computer's. Set once, before anything is named.
pub fn set_self(key: &str) {
    with_book(|b| b.me = key.to_string());
}

fn with_book<T>(f: impl FnOnce(&mut Book) -> T) -> T {
    let mut guard = BOOK.lock().unwrap_or_else(|e| e.into_inner());
    let book = guard.get_or_insert_with(|| Book {
        own: load(),
        profiles: std::fs::read_to_string(profiles_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default(),
        people: load_names(),
        computers: read_pairs(&computer_names_path()),
        labels: crypto::load_peers()
            .into_iter()
            .filter(|p| !p.label.is_empty())
            .map(|p| (p.pubkey, p.label))
            .collect(),
        ..Book::default()
    });
    f(book)
}

/// Remember what a computer said about itself, if it is newer than what was
/// known. Written to disk only when it changed, which is rarely.
pub fn seen(p: &Plan) {
    let Some(me) = &p.me else { return };
    with_book(|b| {
        if p.pubkey == b.me {
            return;
        }
        let stale = b.profiles.get(&p.pubkey).is_none_or(|(t, _)| *t < p.epoch);
        if !stale {
            return;
        }
        let changed = b.profiles.get(&p.pubkey).is_none_or(|(_, m)| m != me);
        b.profiles.insert(p.pubkey.clone(), (p.epoch, me.clone()));
        if changed {
            let _ = std::fs::create_dir_all(crypto::config_dir());
            let _ = std::fs::write(
                profiles_path(),
                serde_json::to_string(&b.profiles).unwrap_or_default(),
            );
        }
    });
}

pub fn nickname(k: &str) -> Option<String> {
    with_book(|b| b.nickname(k))
}

pub fn person(k: &str) -> Option<String> {
    with_book(|b| b.person(k))
}

pub fn computer(k: &str) -> Option<String> {
    with_book(|b| b.computer(k))
}

pub fn same_person(a: &str, b: &str) -> bool {
    with_book(|bk| bk.same_person(a, b))
}

pub fn person_key(k: &str) -> String {
    with_book(|b| b.person_key(k))
}

pub fn computers_of(k: &str) -> Vec<String> {
    with_book(|b| b.computers_of(k))
}

/// Set your own nickname for a computer, or clear it with an empty one.
pub fn set_computer_name(k: &str, nick: &str) -> Result<(), String> {
    let mut pairs = read_pairs(&computer_names_path());
    if nick.is_empty() {
        pairs.remove(k);
    } else {
        pairs.insert(k.to_string(), nick.to_string());
    }
    let _ = std::fs::create_dir_all(crypto::config_dir());
    write_pairs(&computer_names_path(), &pairs)?;
    with_book(|b| b.computers = pairs);
    Ok(())
}

/// Set your own nickname for a person, by any of their computers' keys.
pub fn set_person_name(k: &str, nick: &str) -> Result<(), String> {
    let mut pairs = load_names();
    // One nickname per person, wherever it was filed.
    for c in computers_of(k) {
        pairs.remove(&c);
    }
    if !nick.is_empty() {
        pairs.insert(k.to_string(), nick.to_string());
    }
    let _ = std::fs::create_dir_all(crypto::config_dir());
    write_pairs(&names_path(), &pairs)?;
    with_book(|b| b.people = pairs);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn me(person: &str, computer: &str, computers: &[&str]) -> Me {
        Me {
            person: person.into(),
            computer: computer.into(),
            computers: computers.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn book() -> Book {
        // You are on "mine". John has a laptop and a studio, linked both ways.
        // Mallory claims John's laptop, but the laptop never claims Mallory.
        let mut b = Book {
            me: "mine".into(),
            own: me("Ana", "desk", &[]),
            ..Book::default()
        };
        b.profiles
            .insert("laptop".into(), (1, me("John", "laptop", &["studio"])));
        b.profiles
            .insert("studio".into(), (1, me("", "studio", &["laptop"])));
        b.profiles
            .insert("mallory".into(), (1, me("Mallory", "", &["laptop"])));
        b
    }

    #[test]
    fn computers_are_one_person_only_when_each_names_the_other() {
        let b = book();
        assert!(b.linked("laptop", "studio") && b.linked("studio", "laptop"));
        assert!(
            !b.linked("mallory", "laptop"),
            "a claim the other side never made"
        );
        assert_eq!(b.computers_of("studio"), ["laptop", "studio"]);
        assert_eq!(b.person_key("studio"), b.person_key("laptop"));
        assert_ne!(b.person_key("mallory"), b.person_key("laptop"));
    }

    #[test]
    fn a_person_is_named_by_any_of_their_computers() {
        let b = book();
        assert_eq!(b.nickname("laptop").as_deref(), Some("John · laptop"));
        assert_eq!(
            b.nickname("studio").as_deref(),
            Some("John · studio"),
            "the studio never said John"
        );
        assert_eq!(b.nickname("mine").as_deref(), Some("Ana · desk"));
        assert_eq!(b.nickname("stranger"), None);
    }

    #[test]
    fn your_nicknames_win() {
        let mut b = book();
        b.people.insert("studio".into(), "Johnny".into());
        b.computers.insert("laptop".into(), "work laptop".into());
        assert_eq!(
            b.nickname("laptop").as_deref(),
            Some("Johnny · work laptop"),
            "filed under the studio, names the person"
        );
    }

    #[test]
    fn a_suggestion_cannot_take_a_name_you_use_for_someone_else() {
        let mut b = book();
        b.profiles
            .insert("mallory".into(), (2, me("John", "", &[])));
        b.people.insert("laptop".into(), "John".into());
        assert_eq!(b.person("mallory"), None, "you call someone else John");
        assert_eq!(b.person("laptop").as_deref(), Some("John"));
        b.profiles.insert("eve".into(), (1, me("ana", "", &[])));
        assert_eq!(b.person("eve"), None, "nor your own name");
        b.labels.insert("eve".into(), "eve-from-work".into());
        assert_eq!(
            b.person("eve").as_deref(),
            Some("eve-from-work"),
            "falls back to your label"
        );
    }
}
