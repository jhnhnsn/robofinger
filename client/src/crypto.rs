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

    /// Shareable identity blob: `rf1.<label>.<ed25519pub>.<agepub>`
    pub fn identity_blob(&self, label: &str) -> String {
        format!(
            "rf1.{}.{}.{}",
            label,
            self.pubkey(),
            self.age_secret.to_public()
        )
    }

    pub fn sign(&self, msg: &str) -> String {
        B64.encode(self.signing.sign(msg.as_bytes()).to_bytes())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Peer {
    pub label: String,
    pub pubkey: String,
    pub age_pub: String,
}

impl Peer {
    pub fn parse(blob: &str) -> Result<Peer, String> {
        let p: Vec<&str> = blob.trim().split('.').collect();
        if p.len() != 4 || p[0] != "rf1" {
            return Err("expected rf1.<label>.<signkey>.<agekey>".into());
        }
        if B64.decode(p[2]).map(|v| v.len()) != Ok(32) {
            return Err("bad ed25519 key".into());
        }
        if !p[3].starts_with("age1") {
            return Err("bad age key".into());
        }
        Ok(Peer {
            label: p[1].into(),
            pubkey: p[2].into(),
            age_pub: p[3].into(),
        })
    }

    pub fn to_blob(&self) -> String {
        format!("rf1.{}.{}.{}", self.label, self.pubkey, self.age_pub)
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
    fn identity_blob_roundtrips() {
        let k = keys_in("blob");
        let blob = k.identity_blob("laptop");
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
