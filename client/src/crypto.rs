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
}

pub fn config_dir() -> PathBuf {
    std::env::var("ROBOFINGER_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_default();
            PathBuf::from(home).join(".config/robofinger")
        })
}

fn write_private(path: &PathBuf, contents: &str) -> std::io::Result<()> {
    std::fs::write(path, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
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

        let sk_path = dir.join("signing.key");
        let signing = match std::fs::read_to_string(&sk_path) {
            Ok(s) => {
                let raw = B64
                    .decode(s.trim())
                    .map_err(|_| "signing.key is corrupt".to_string())?;
                let arr: [u8; 32] = raw
                    .as_slice()
                    .try_into()
                    .map_err(|_| "signing.key wrong length".to_string())?;
                SigningKey::from_bytes(&arr)
            }
            Err(_) => {
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
            Ok(s) => s
                .trim()
                .parse::<age::x25519::Identity>()
                .map_err(|_| "age.key is corrupt".to_string())?,
            Err(_) => {
                let id = age::x25519::Identity::generate();
                let secret = age::secrecy::ExposeSecret::expose_secret(&id.to_string()).to_string();
                write_private(&age_path, &secret).map_err(|e| format!("write age.key: {e}"))?;
                id
            }
        };

        Ok(Keys {
            signing,
            age_secret,
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
        }
        .to_blob()
    }

    pub fn sign(&self, msg: &str) -> String {
        B64.encode(self.signing.sign(msg.as_bytes()).to_bytes())
    }
}

/// Where a peer publishes. `None` means "wherever I am" — the local
/// ROBOFINGER_URL and namespace.
#[derive(Clone, Debug, PartialEq)]
pub struct Home {
    pub url: String,
    pub ns: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Peer {
    pub label: String,
    pub pubkey: String,
    pub age_pub: String,
    /// Set when the peer publishes to a different relay or namespace than you.
    pub home: Option<Home>,
}

impl Peer {
    /// Accepts both blob versions:
    ///   rf1.<label>.<signkey>.<agekey>                     — same relay as you
    ///   rf2.<label>.<signkey>.<agekey>.<b64(url|ns)>       — carries its home
    ///
    /// The home field is base64url-encoded because a URL contains `/` and `:`,
    /// which would otherwise collide with the `.` separator and make the blob
    /// ambiguous to split.
    pub fn parse(blob: &str) -> Result<Peer, String> {
        let p: Vec<&str> = blob.trim().split('.').collect();
        let (version, expected) = match p.first() {
            Some(&"rf1") => ("rf1", 4),
            Some(&"rf2") => ("rf2", 5),
            _ => return Err("expected an rf1... or rf2... identity blob".into()),
        };
        if p.len() != expected {
            return Err(format!(
                "malformed {version} blob: expected {expected} dot-separated parts, got {}",
                p.len()
            ));
        }
        if B64.decode(p[2]).map(|v| v.len()) != Ok(32) {
            return Err("bad ed25519 key".into());
        }
        if !p[3].starts_with("age1") {
            return Err("bad age key".into());
        }

        let home = if version == "rf2" {
            let raw = B64
                .decode(p[4])
                .map_err(|_| "bad home field".to_string())
                .and_then(|b| String::from_utf8(b).map_err(|_| "home is not utf-8".to_string()))?;
            let (url, ns) = raw
                .split_once('|')
                .ok_or_else(|| "home must be <url>|<namespace>".to_string())?;
            if url.is_empty() || ns.is_empty() {
                return Err("home url and namespace must both be set".into());
            }
            Some(Home {
                url: url.trim_end_matches('/').to_string(),
                ns: ns.to_string(),
            })
        } else {
            None
        };

        Ok(Peer {
            label: p[1].into(),
            pubkey: p[2].into(),
            age_pub: p[3].into(),
            home,
        })
    }

    pub fn to_blob(&self) -> String {
        match &self.home {
            None => format!("rf1.{}.{}.{}", self.label, self.pubkey, self.age_pub),
            Some(h) => format!(
                "rf2.{}.{}.{}.{}",
                self.label,
                self.pubkey,
                self.age_pub,
                B64.encode(format!("{}|{}", h.url, h.ns))
            ),
        }
    }

    /// Relay and namespace to fetch this peer from, falling back to your own.
    pub fn endpoint<'a>(&'a self, my_url: &'a str, my_ns: &'a str) -> (&'a str, &'a str) {
        match &self.home {
            Some(h) => (&h.url, &h.ns),
            None => (my_url, my_ns),
        }
    }
}

pub fn peers_path() -> PathBuf {
    config_dir().join("peers")
}

pub fn load_peers() -> Vec<Peer> {
    std::fs::read_to_string(peers_path())
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .filter_map(|l| Peer::parse(l).ok())
        .collect()
}

pub fn save_peers(peers: &[Peer]) -> Result<(), String> {
    let body: String = peers
        .iter()
        .map(|p| p.to_blob() + "\n")
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
    fn rf2_blob_carries_home_relay() {
        let k = keys_in("rf2");
        let home = Home {
            url: "https://other.example.com".into(),
            ns: "theirns".into(),
        };
        let blob = k.identity_blob("faraway", Some(home.clone()));
        assert!(blob.starts_with("rf2."), "expected rf2, got {blob}");
        let p = Peer::parse(&blob).unwrap();
        assert_eq!(p.home, Some(home));
        assert_eq!(p.label, "faraway");
        // A trailing slash on the URL must not survive, or endpoint URLs double up.
        let h2 = Home {
            url: "https://x.example/".into(),
            ns: "n".into(),
        };
        let p2 = Peer::parse(&k.identity_blob("t", Some(h2))).unwrap();
        assert_eq!(p2.home.unwrap().url, "https://x.example");
    }

    #[test]
    fn rf1_blobs_still_parse_and_mean_local() {
        let k = keys_in("rf1compat");
        let blob = k.identity_blob("local", None);
        assert!(blob.starts_with("rf1."));
        let p = Peer::parse(&blob).unwrap();
        assert_eq!(p.home, None, "no home means use my own relay");
        assert_eq!(p.endpoint("https://mine", "myns"), ("https://mine", "myns"));
    }

    #[test]
    fn endpoint_prefers_the_peers_own_relay() {
        let k = keys_in("endpoint");
        let home = Home {
            url: "https://theirs".into(),
            ns: "theirns".into(),
        };
        let p = Peer::parse(&k.identity_blob("x", Some(home))).unwrap();
        assert_eq!(
            p.endpoint("https://mine", "myns"),
            ("https://theirs", "theirns")
        );
    }

    #[test]
    fn malformed_blobs_are_rejected() {
        assert!(Peer::parse("garbage").is_err());
        assert!(Peer::parse("rf1.a.b.c").is_err(), "bad keys");
        assert!(Peer::parse("rf2.a.b.c").is_err(), "rf2 needs 5 parts");
        assert!(Peer::parse("rf9.a.b.c.d").is_err(), "unknown version");
        let k = keys_in("malformed");
        // rf2 with an unparseable home field
        let bad = format!("rf2.x.{}.{}.!!!", k.pubkey(), k.age_secret.to_public());
        assert!(Peer::parse(&bad).is_err(), "bad base64 home");
    }

    #[test]
    fn identity_blob_roundtrips() {
        let k = keys_in("blob");
        let blob = k.identity_blob("laptop", None);
        let p = Peer::parse(&blob).unwrap();
        assert_eq!(p.label, "laptop");
        assert_eq!(p.pubkey, k.pubkey());
        assert_eq!(p.age_pub, k.age_secret.to_public().to_string());
        assert!(Peer::parse("garbage").is_err());
        assert!(Peer::parse("rf1.a.b.c").is_err(), "bad keys rejected");
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
