//! Keys, identity blobs, signing and encryption.
//!
//! Two keypairs per agent, because they do different jobs:
//!   * Ed25519 — signs envelopes. The public key IS the agent's identity.
//!   * age X25519 — encrypts plan bodies to peers (same crate/format as envstow).
//!
//! Both public halves travel together in one `rf1...` blob so adding a peer is
//! a single paste.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use std::io::{Read, Write};
use std::path::PathBuf;

pub struct Keys {
    pub signing: SigningKey,
    pub age_secret: age::x25519::Identity,
    /// True when this load generated a new identity rather than reading one.
    /// Callers surface it — silently minting keys leaves people unaware that
    /// something irreplaceable now exists on disk.
    pub freshly_generated: bool,
}

pub fn config_dir() -> PathBuf {
    std::env::var("ROBOFINGER_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_default();
            PathBuf::from(home).join(".config/robofinger")
        })
}

/// Write a private key with 0600 set *at creation*.
///
/// Writing first and chmod-ing after leaves a window where the key is on disk
/// world-readable, and does nothing at all if the file already existed with
/// loose permissions — `fs::write` truncates but keeps the old mode.
fn write_private(path: &PathBuf, contents: &str) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(contents.as_bytes())?;
        // `mode()` only applies when the file is newly created, so an existing
        // file keeps whatever it had — tighten it explicitly.
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        f.sync_all()?;
    }
    #[cfg(not(unix))]
    std::fs::write(path, contents)?;
    Ok(())
}

/// Refuse to load a key that others can read.
///
/// Keys arrive by means we do not control — restored from a backup, copied
/// with `cp`, synced between machines — and any of those can land 0644. A
/// silent load would leave the identity readable by every process on the box.
#[cfg(unix)]
fn check_private_mode(path: &PathBuf) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path)
        .map_err(|e| format!("stat {}: {e}", path.display()))?
        .permissions()
        .mode()
        & 0o777;
    if mode & 0o077 != 0 {
        return Err(format!(
            "{} is mode {:o} — group/other can read your private key.\n  fix it with: chmod 600 {}",
            path.display(),
            mode,
            path.display()
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_private_mode(_path: &PathBuf) -> Result<(), String> {
    Ok(())
}

impl Keys {
    /// Load keys, generating them on first use.
    pub fn load_or_create() -> Result<Keys, String> {
        Self::load_from(&config_dir())
    }

    /// Load or create keys in an explicit directory.
    pub fn load_from(dir: &PathBuf) -> Result<Keys, String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }

        let mut freshly_generated = false;
        let sk_path = dir.join("signing.key");
        let signing = match std::fs::read_to_string(&sk_path) {
            Ok(s) => {
                check_private_mode(&sk_path)?;
                let raw = B64.decode(s.trim()).map_err(|_| {
                    format!(
                        "{} is not valid base64 — restore it from a backup, or delete it to generate a new identity (you will have to re-exchange with every peer)",
                        sk_path.display()
                    )
                })?;
                let arr: [u8; 32] = raw.as_slice().try_into().map_err(|_| {
                    format!(
                        "{} is {} bytes, expected 32 — restore it from a backup, or delete it to generate a new identity (you will have to re-exchange with every peer)",
                        sk_path.display(),
                        raw.len()
                    )
                })?;
                SigningKey::from_bytes(&arr)
            }
            Err(_) => {
                freshly_generated = true;
                let mut seed = [0u8; 32];
                getrandom::fill(&mut seed).map_err(|e| format!("rng: {e}"))?;
                let k = SigningKey::from_bytes(&seed);
                write_private(&sk_path, &B64.encode(k.to_bytes()))
                    .map_err(|e| format!("write signing.key: {e}"))?;
                k
            }
        };

        let age_path = dir.join("age.key");
        let age_secret = match std::fs::read_to_string(&age_path) {
            Ok(s) => {
                check_private_mode(&age_path)?;
                s.trim().parse::<age::x25519::Identity>().map_err(|_| {
                    format!(
                        "{} is not a valid age key — restore it from a backup, or delete it to generate a new one (peers will need to re-add you)",
                        age_path.display()
                    )
                })?
            }
            Err(_) => {
                freshly_generated = true;
                let id = age::x25519::Identity::generate();
                let secret = age::secrecy::ExposeSecret::expose_secret(&id.to_string()).to_string();
                write_private(&age_path, &secret).map_err(|e| format!("write age.key: {e}"))?;
                id
            }
        };

        Ok(Keys {
            signing,
            age_secret,
            freshly_generated,
        })
    }

    /// Base64url Ed25519 public key — this agent's identity on the relay.
    pub fn pubkey(&self) -> String {
        B64.encode(self.signing.verifying_key().to_bytes())
    }

    /// Shareable identity blob.
    ///
    /// Emits `rf2` (carrying your relay + namespace) when they're known, so a
    /// peer on a different relay can still reach you. Falls back to `rf1` when
    /// unconfigured — an identity is still shareable before `init`.
    pub fn identity_blob(&self, label: &str, home: Option<Home>) -> String {
        Peer {
            label: label.to_string(),
            pubkey: self.pubkey(),
            age_pub: self.age_secret.to_public().to_string(),
            home,
            groups: Vec::new(),
        }
        .to_blob()
    }

    pub fn sign(&self, msg: &str) -> String {
        B64.encode(self.signing.sign(msg.as_bytes()).to_bytes())
    }
}

/// Where a peer publishes: a relay base URL whose path is the namespace.
/// `None` means "wherever I am" — the local ROBOFINGER_URL.
#[derive(Clone, Debug, PartialEq)]
pub struct Home {
    pub url: String,
}

/// Percent-encode the few characters that would break a query string. Labels
/// are cosmetic, so this is deliberately minimal rather than a full URL encoder.
fn encode_label(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            ' ' => "%20".to_string(),
            '#' => "%23".to_string(),
            '&' => "%26".to_string(),
            '?' => "%3F".to_string(),
            c => c.to_string(),
        })
        .collect()
}

fn decode_label(s: &str) -> String {
    s.replace("%20", " ")
        .replace("%23", "#")
        .replace("%26", "&")
        .replace("%3F", "?")
}

#[derive(Clone, Debug, PartialEq)]
pub struct Peer {
    pub label: String,
    pub pubkey: String,
    pub age_pub: String,
    /// Set when the peer publishes to a different relay or namespace than you.
    pub home: Option<Home>,
    /// Local tags, never part of the shared address — who you consider this
    /// peer to be is your business, not theirs. Empty means they receive
    /// everything you publish.
    pub groups: Vec<String>,
}

impl Peer {
    /// An address is a URL:
    ///
    ///   https://sam@relay.example.com/plan/u/<pubkey>#<agekey>
    ///          │    └───── base = namespace ──┘    │        │
    ///     suggested label                      identity  encryption key
    ///
    /// The label is a *suggestion* the sender makes about what to call them.
    /// It is not part of their identity — the public key is — so the receiver
    /// can override it and must not let a suggestion shadow a peer they
    /// already follow. `?label=` is still accepted for older addresses.
    ///
    /// The base path IS the namespace, so a relay can live at
    /// `example.com/plan` without colliding with the rest of the site.
    ///
    /// The age key sits in the fragment because browsers never send fragments
    /// to servers — paste this in a browser and the relay still cannot learn
    /// your encryption key. (A convention, not a guarantee: only well-behaved
    /// relays are bound by it. Confidentiality still rests on encryption.)
    pub fn parse(addr: &str) -> Result<Peer, String> {
        let addr = addr.trim();
        if !addr.starts_with("http://") && !addr.starts_with("https://") {
            return Err("address must be an http(s) URL".into());
        }

        // Fragment first — everything after the first '#' is the age key.
        let (rest, age_pub) = addr
            .split_once('#')
            .ok_or_else(|| "address is missing its #<agekey> fragment".to_string())?;
        if !age_pub.starts_with("age1") {
            return Err(format!("bad age key in fragment: {age_pub}"));
        }

        // Query is optional and carries only the display label.
        let (path_part, query) = match rest.split_once('?') {
            Some((p, q)) => (p, Some(q)),
            None => (rest, None),
        };
        let query_label = query.and_then(|q| {
            q.split('&')
                .find_map(|kv| kv.strip_prefix("label=").map(str::to_string))
        });

        // `scheme://label@host/...` — strip the userinfo before anything tries
        // to treat it as part of the host.
        let (path_part, user_label) = {
            let (scheme, after) = path_part
                .split_once("://")
                .ok_or_else(|| "address must be an http(s) URL".to_string())?;
            match after.split_once('@') {
                Some((user, host)) if !user.contains('/') => {
                    (format!("{scheme}://{host}"), Some(user.to_string()))
                }
                _ => (path_part.to_string(), None),
            }
        };
        let label = user_label.or(query_label).unwrap_or_default();
        let path_part = path_part.as_str();

        let (base, pubkey) = path_part
            .rsplit_once("/u/")
            .ok_or_else(|| "address must contain /u/<pubkey>".to_string())?;
        if B64.decode(pubkey).map(|v| v.len()) != Ok(32) {
            return Err(format!("bad ed25519 key: {pubkey}"));
        }
        if base.is_empty() {
            return Err("address is missing its relay URL".into());
        }

        Ok(Peer {
            label: if label.is_empty() {
                pubkey[..8].to_string()
            } else {
                decode_label(&label)
            },
            pubkey: pubkey.to_string(),
            age_pub: age_pub.to_string(),
            home: Some(Home {
                url: base.trim_end_matches('/').to_string(),
            }),
            groups: Vec::new(),
        })
    }

    pub fn to_blob(&self) -> String {
        let base = self
            .home
            .as_ref()
            .map(|h| h.url.as_str())
            .unwrap_or_default();
        // Suggest the label as userinfo: it reads as an address rather than a
        // query, and puts the human-meaningful part first.
        let with_label = match (self.label.as_str(), base.split_once("://")) {
            ("", _) | (_, None) => base.to_string(),
            (label, Some((scheme, host))) => {
                format!("{scheme}://{}@{host}", encode_label(label))
            }
        };
        format!("{with_label}/u/{}#{}", self.pubkey, self.age_pub)
    }

    /// Relay base URL to fetch this peer from, falling back to your own.
    pub fn endpoint<'a>(&'a self, my_url: &'a str) -> &'a str {
        match &self.home {
            Some(h) => &h.url,
            None => my_url,
        }
    }
}

pub fn peers_path() -> PathBuf {
    config_dir().join("peers")
}

/// One peer per line: the address, then optional whitespace-separated groups.
///
///   https://alice@relay.example.com/u/…#age1…   work,friends
pub fn load_peers() -> Vec<Peer> {
    std::fs::read_to_string(peers_path())
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let mut parts = l.split_whitespace();
            let addr = parts.next()?;
            let groups: Vec<String> = parts
                .next()
                .map(|g| {
                    g.split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            let mut p = Peer::parse(addr).ok()?;
            p.groups = groups;
            Some(p)
        })
        .collect()
}

pub fn save_peers(peers: &[Peer]) -> Result<(), String> {
    let body: String = peers
        .iter()
        .map(|p| {
            if p.groups.is_empty() {
                format!("{}\n", p.to_blob())
            } else {
                format!("{}  {}\n", p.to_blob(), p.groups.join(","))
            }
        })
        .collect::<Vec<_>>()
        .concat();
    std::fs::write(peers_path(), body).map_err(|e| format!("write peers: {e}"))
}

/// Verify `sig` over `msg` for a base64url Ed25519 public key.
pub fn verify(pubkey: &str, sig: &str, msg: &str) -> bool {
    let (Ok(pk), Ok(s)) = (B64.decode(pubkey), B64.decode(sig)) else {
        return false;
    };
    let (Ok(pk), Ok(s)): (Result<[u8; 32], _>, Result<[u8; 64], _>) =
        (pk.as_slice().try_into(), s.as_slice().try_into())
    else {
        return false;
    };
    VerifyingKey::from_bytes(&pk)
        .map(|k| k.verify(msg.as_bytes(), &Signature::from_bytes(&s)).is_ok())
        .unwrap_or(false)
}

/// Encrypt to every recipient. Always includes self, or you cannot read your
/// own plans back.
pub fn encrypt(plaintext: &[u8], recipients: &[age::x25519::Recipient]) -> Result<String, String> {
    if recipients.is_empty() {
        return Err("no recipients".into());
    }
    let boxed: Vec<Box<dyn age::Recipient + Send>> = recipients
        .iter()
        .map(|r| Box::new(r.clone()) as Box<dyn age::Recipient + Send>)
        .collect();
    let enc =
        age::Encryptor::with_recipients(boxed.iter().map(|b| b.as_ref() as &dyn age::Recipient))
            .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    let mut w = enc.wrap_output(&mut out).map_err(|e| e.to_string())?;
    w.write_all(plaintext).map_err(|e| e.to_string())?;
    w.finish().map_err(|e| e.to_string())?;
    Ok(B64.encode(out))
}

pub fn decrypt(b64: &str, identity: &age::x25519::Identity) -> Result<Vec<u8>, String> {
    let ct = B64.decode(b64).map_err(|e| e.to_string())?;
    let dec = age::Decryptor::new(&ct[..]).map_err(|e| e.to_string())?;
    let mut r = dec
        .decrypt(std::iter::once(identity as &dyn age::Identity))
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    r.read_to_end(&mut out).map_err(|e| e.to_string())?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Load keys from an isolated directory. Takes an explicit path instead of
    /// setting ROBOFINGER_HOME so tests stay independent under the default
    /// parallel test runner.
    fn keys_in(tag: &str) -> Keys {
        let d = std::env::temp_dir().join(format!("rf-test-{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        Keys::load_from(&d).unwrap()
    }

    #[test]
    fn sign_and_verify_roundtrip() {
        let k = keys_in("sig");
        let sig = k.sign("hello");
        assert!(verify(&k.pubkey(), &sig, "hello"));
        assert!(
            !verify(&k.pubkey(), &sig, "hello!"),
            "tampered msg must fail"
        );
    }

    #[test]
    fn wrong_key_cannot_forge() {
        let a = keys_in("forge");
        let sig = a.sign("msg");
        let b = keys_in("forge2");
        assert!(
            !verify(&b.pubkey(), &sig, "msg"),
            "other key must not verify"
        );
    }

    #[test]
    fn encrypt_only_readable_by_recipients() {
        let a = keys_in("enc-a");
        let b = keys_in("enc-b");
        let c = keys_in("enc-c");

        // encrypt to a and b only
        let ct = encrypt(
            b"secret plan",
            &[a.age_secret.to_public(), b.age_secret.to_public()],
        )
        .unwrap();
        assert_eq!(decrypt(&ct, &a.age_secret).unwrap(), b"secret plan");
        assert_eq!(decrypt(&ct, &b.age_secret).unwrap(), b"secret plan");
        assert!(
            decrypt(&ct, &c.age_secret).is_err(),
            "non-recipient must fail"
        );
    }

    #[test]
    fn label_travels_as_userinfo() {
        let k = keys_in("userinfo");
        let home = Home {
            url: "https://relay.example.com/plan".into(),
        };
        let addr = k.identity_blob("sam", Some(home));
        assert!(
            addr.starts_with("https://sam@relay.example.com/plan/u/"),
            "expected userinfo form, got {addr}"
        );
        let p = Peer::parse(&addr).unwrap();
        assert_eq!(p.label, "sam");
        assert_eq!(p.home.unwrap().url, "https://relay.example.com/plan");
    }

    #[test]
    fn query_label_still_parses_for_older_addresses() {
        let k = keys_in("oldlabel");
        let old = format!(
            "https://relay.example.com/plan/u/{}?label=laptop#{}",
            k.pubkey(),
            k.age_secret.to_public()
        );
        let p = Peer::parse(&old).unwrap();
        assert_eq!(p.label, "laptop");
        assert_eq!(p.home.unwrap().url, "https://relay.example.com/plan");
    }

    #[test]
    fn an_at_in_the_path_is_not_userinfo() {
        let k = keys_in("atpath");
        // Only userinfo sits before the first '/', so a later '@' must not be
        // mistaken for a label.
        let addr = format!(
            "https://relay.example.com/plan/@team/u/{}#{}",
            k.pubkey(),
            k.age_secret.to_public()
        );
        let p = Peer::parse(&addr).unwrap();
        assert_eq!(p.home.unwrap().url, "https://relay.example.com/plan/@team");
    }

    #[test]
    fn address_is_a_url_with_key_in_the_fragment() {
        let k = keys_in("urlform");
        let home = Home {
            url: "https://relay.example.com/plan".into(),
        };
        let addr = k.identity_blob("laptop", Some(home));
        assert!(addr.starts_with("https://laptop@relay.example.com/plan/u/"));
        assert!(addr.contains("laptop@"), "label rides as userinfo: {addr}");
        // The age key must be in the fragment: browsers never send it to servers.
        let (_, frag) = addr.split_once('#').expect("needs a fragment");
        assert_eq!(frag, k.age_secret.to_public().to_string());
        let p = Peer::parse(&addr).unwrap();
        assert_eq!(p.label, "laptop");
        assert_eq!(p.pubkey, k.pubkey());
        assert_eq!(p.home.unwrap().url, "https://relay.example.com/plan");
    }

    #[test]
    fn path_is_the_namespace() {
        let k = keys_in("pathns");
        // A relay under a path coexists with the rest of the site, and deeper
        // paths are separate rooms.
        for base in [
            "https://jhnhnsn.com/plan",
            "https://jhnhnsn.com/plan/team-a",
            "http://localhost:8787",
        ] {
            let addr = k.identity_blob("x", Some(Home { url: base.into() }));
            let p = Peer::parse(&addr).unwrap();
            assert_eq!(p.home.unwrap().url, base, "round-trip failed for {base}");
        }
    }

    #[test]
    fn labels_with_awkward_characters_survive() {
        let k = keys_in("labels");
        let home = Home {
            url: "https://r.example.com".into(),
        };
        // Spaces and separators would otherwise break the query string.
        let p = Peer::parse(&k.identity_blob("my laptop", Some(home))).unwrap();
        assert_eq!(p.label, "my laptop");
    }

    #[test]
    fn malformed_addresses_are_rejected() {
        let k = keys_in("badaddr");
        let key = k.pubkey();
        let age = k.age_secret.to_public().to_string();
        let cases = [
            ("not-a-url", "must be http(s)"),
            ("https://r.example.com/u/{key}", "missing fragment"),
            ("https://r.example.com/plans#{age}", "no /u/ segment"),
            ("https://r.example.com/u/short#{age}", "bad ed25519 key"),
            ("https://r.example.com/u/{key}#notanagekey", "bad age key"),
        ];
        for (tpl, why) in cases {
            let addr = tpl.replace("{key}", &key).replace("{age}", &age);
            assert!(Peer::parse(&addr).is_err(), "should reject ({why}): {addr}");
        }
    }

    #[test]
    fn identity_blob_roundtrips() {
        let k = keys_in("blob");
        let home = Home {
            url: "https://relay.example.com".into(),
        };
        let addr = k.identity_blob("laptop", Some(home));
        let p = Peer::parse(&addr).unwrap();
        assert_eq!(p.label, "laptop");
        assert_eq!(p.pubkey, k.pubkey());
        assert_eq!(p.age_pub, k.age_secret.to_public().to_string());
        // Round-trips through to_blob() unchanged, so a re-shared address is
        // byte-identical to the original.
        assert_eq!(p.to_blob(), addr);
    }

    #[test]
    fn an_address_without_a_relay_is_rejected() {
        let k = keys_in("norelay");
        // identity_blob with no home cannot produce a usable address — there is
        // nowhere to fetch from. Better to fail loudly than emit a broken URL.
        let addr = k.identity_blob("nowhere", None);
        assert!(Peer::parse(&addr).is_err(), "got {addr}");
    }

    #[test]
    #[cfg(unix)]
    fn keys_are_created_private() {
        use std::os::unix::fs::PermissionsExt;
        let d = std::env::temp_dir().join("rf-test-mode-create");
        let _ = std::fs::remove_dir_all(&d);
        Keys::load_from(&d).unwrap();
        for f in ["signing.key", "age.key"] {
            let mode = std::fs::metadata(d.join(f)).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "{f} should be 0600, got {mode:o}");
        }
    }

    #[test]
    #[cfg(unix)]
    fn a_world_readable_key_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let d = std::env::temp_dir().join("rf-test-mode-loose");
        let _ = std::fs::remove_dir_all(&d);
        Keys::load_from(&d).unwrap();

        // A key can arrive 0644 from a backup, `cp`, or a sync tool. Loading it
        // silently would leave the identity readable by any local process.
        let sk = d.join("signing.key");
        std::fs::set_permissions(&sk, std::fs::Permissions::from_mode(0o644)).unwrap();
        // Not unwrap_err() — Keys deliberately has no Debug impl, since it
        // holds private key material.
        let err = match Keys::load_from(&d) {
            Err(e) => e,
            Ok(_) => panic!("a 0644 key should be refused"),
        };
        assert!(err.contains("644"), "should name the mode: {err}");
        assert!(err.contains("chmod 600"), "should say how to fix it: {err}");

        // And it loads again once tightened.
        std::fs::set_permissions(&sk, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(Keys::load_from(&d).is_ok());
    }

    #[test]
    #[cfg(unix)]
    fn rewriting_a_key_tightens_a_loose_file() {
        use std::os::unix::fs::PermissionsExt;
        let d = std::env::temp_dir().join("rf-test-mode-rewrite");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("signing.key");
        std::fs::write(&p, "placeholder").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o666)).unwrap();

        write_private(&p, "replacement").unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "write_private must tighten an existing file");
    }

    #[test]
    fn generation_is_reported_only_the_first_time() {
        let d = std::env::temp_dir().join("rf-test-fresh-flag");
        let _ = std::fs::remove_dir_all(&d);
        assert!(
            Keys::load_from(&d).unwrap().freshly_generated,
            "first load mints an identity and must say so"
        );
        assert!(
            !Keys::load_from(&d).unwrap().freshly_generated,
            "a subsequent load reads the same keys and must stay quiet"
        );
    }

    #[test]
    fn keys_persist_across_loads() {
        let d = std::env::temp_dir().join("rf-test-persist");
        let _ = std::fs::remove_dir_all(&d);
        let a = Keys::load_from(&d).unwrap();
        let b = Keys::load_from(&d).unwrap();
        assert_eq!(a.pubkey(), b.pubkey(), "must not regenerate");
    }
}
