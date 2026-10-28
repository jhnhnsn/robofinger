//! Invitations: how a person joins a project (ADR 0006).
//!
//! A member runs `robofinger invite` and hands over one string. It names the
//! project and its relay, lists the members' addresses, carries an expiry, and
//! is signed with the member's key. `robofinger join <invitation>` checks the
//! signature, joins the folder, follows the members, and posts a "join" entry
//! that carries the invitation. Members adopt the newcomer when that entry
//! shows a current member vouched for them, so nobody has to swap addresses by
//! hand.
//!
//! An invitation is a bearer secret until it expires: anyone holding it can
//! join. That is why it expires, and why the join entry names who invited
//! whom.

use super::*;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};

/// What the signature covers is this prefix plus the body as JSON. The prefix
/// keeps the signature from being mistaken for one over anything else.
const SIGNED: &str = "robofinger invitation v1\n";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Body {
    pub project: String,
    /// The project's relay, its path the namespace.
    pub relay: String,
    /// The inviter's public key.
    pub by: String,
    /// Unix time after which the invitation is no good.
    pub exp: i64,
    /// Every member's address, the inviter's included, each at the project's
    /// relay.
    pub members: Vec<String>,
    /// A link (ADR 0009): whoever joins with this becomes another of the
    /// inviter's own computers. Left out of an ordinary invitation, so its
    /// signed form is what it always was.
    #[serde(default, skip_serializing_if = "is_false")]
    pub link: bool,
    /// For a link: the inviter's other computers, so the new one names them
    /// all and not just the one that made the code.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub computers: Vec<String>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Invitation {
    pub body: Body,
    pub sig: String,
}

/// What a join entry carries, so members can check who vouched.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Proof {
    /// The joiner's own address at the project's relay.
    pub address: String,
    /// The invitation, as handed over.
    pub invitation: String,
}

impl Body {
    fn message(&self) -> String {
        format!(
            "{SIGNED}{}",
            serde_json::to_string(self).unwrap_or_default()
        )
    }
}

pub fn make(k: &Keys, project: &str, relay: &str, members: Vec<String>, exp: i64) -> Invitation {
    sign(
        k,
        Body {
            project: project.into(),
            relay: relay.into(),
            by: k.pubkey(),
            exp,
            members,
            link: false,
            computers: vec![],
        },
    )
}

/// An invitation that makes whoever holds it another of your computers.
pub fn make_link(
    k: &Keys,
    project: &str,
    relay: &str,
    members: Vec<String>,
    exp: i64,
    computers: Vec<String>,
) -> Invitation {
    let mut inv = make(k, project, relay, members, exp);
    inv.body.link = true;
    inv.body.computers = computers;
    sign(k, inv.body)
}

fn sign(k: &Keys, body: Body) -> Invitation {
    let sig = k.sign(&body.message());
    Invitation { body, sig }
}

/// A URL at the project's relay with everything in the fragment, which
/// browsers never send to servers, as robofinger addresses already do.
pub fn encode(inv: &Invitation) -> String {
    let json = serde_json::to_vec(inv).unwrap_or_default();
    format!("{}/join#{}", inv.body.relay, B64.encode(json))
}

pub fn decode(s: &str) -> Result<Invitation, String> {
    let (_, payload) = s
        .trim()
        .rsplit_once('#')
        .ok_or("that is not an invitation: it has no #…")?;
    let json = B64
        .decode(payload)
        .map_err(|_| "that invitation is damaged: its #… part is not base64")?;
    serde_json::from_slice(&json)
        .map_err(|_| "that invitation is damaged: it does not parse".into())
}

/// Whether `s` looks like an invitation rather than a relay URL.
pub fn is_invitation(s: &str) -> bool {
    s.split('#')
        .next()
        .is_some_and(|base| base.ends_with("/join"))
        && s.contains('#')
}

/// An invitation is good when its signature is the inviter's, the inviter is
/// one of the members it lists, and it has not expired at `t`.
pub fn check(inv: &Invitation, t: i64) -> Result<(), String> {
    if !crypto::verify(&inv.body.by, &inv.sig, &inv.body.message()) {
        return Err("that invitation's signature does not check out; ask for a new one".into());
    }
    let listed = inv
        .body
        .members
        .iter()
        .filter_map(|m| Peer::parse(m).ok())
        .any(|p| p.pubkey == inv.body.by);
    if !listed {
        return Err("that invitation does not list its own inviter as a member".into());
    }
    if t > inv.body.exp {
        return Err(format!(
            "that invitation expired {}; ask for a new one",
            stamp(inv.body.exp)
        ));
    }
    Ok(())
}

/// The relay's raw posts, from every key. Only these can show a newcomer
/// nobody follows yet: every other read asks for trusted keys alone.
fn recent_posts(c: &Cfg) -> Vec<Envelope> {
    agent()
        .get(&format!("{}/posts?limit={MAX_POST_LIMIT}", c.url))
        .call()
        .ok()
        .and_then(|mut r| r.body_mut().read_json::<Vec<serde_json::Value>>().ok())
        .map(|v| {
            v.into_iter()
                .filter_map(|x| serde_json::from_value::<Envelope>(x).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Follow anyone who joined this project with an invitation from a current
/// member, found among the relay's recent posts. Returns who was adopted, as
/// (their name, who invited them).
pub fn adopt(c: &Cfg, k: &Keys) -> Vec<(String, String)> {
    let mut peers = crypto::load_peers();
    let me = k.pubkey();
    let mut adopted = Vec::new();
    let mut linked = Vec::new();
    for env in recent_posts(c) {
        if env.pubkey == me || peers.iter().any(|p| p.pubkey == env.pubkey) {
            continue;
        }
        if !crypto::verify(&env.pubkey, &env.sig, &env.signed_message()) {
            continue;
        }
        // Encrypted to the members the invitation listed; anything else does
        // not decrypt, and is none of this machine's business.
        let Some(plan) = decrypt_plan(&env, k) else {
            continue;
        };
        let Some(proof) = plan.joined.as_ref().filter(|_| plan.kind == "join") else {
            continue;
        };
        let Ok(mut newcomer) = Peer::parse(&proof.address) else {
            continue;
        };
        // The address must be the key that signed the entry, or anyone could
        // add someone else.
        if newcomer.pubkey != env.pubkey {
            continue;
        }
        let Ok(inv) = decode(&proof.invitation) else {
            continue;
        };
        let vouched = inv.body.by == me || peers.iter().any(|p| p.pubkey == inv.body.by);
        if !vouched || check(&inv, plan.epoch).is_err() || inv.body.relay != c.url {
            continue;
        }
        // A link from this person's own computer: the newcomer is another
        // of ours, so name it back (ADR 0009).
        if inv.body.link && people::same_person(&inv.body.by, &me) {
            let _ = people::link(std::slice::from_ref(&newcomer.pubkey), &me);
            linked.push(name_for(&newcomer.pubkey));
        }
        newcomer.home = Some(crypto::Home { url: c.url.clone() });
        newcomer.label = free_label(&newcomer, &peers);
        adopted.push((name_for(&newcomer.pubkey), name_for(&inv.body.by)));
        peers.push(newcomer);
    }
    if !adopted.is_empty() {
        let _ = crypto::save_peers(&peers);
    }
    // Teammates believe a link only when both computers name each other. The
    // newcomer's join names this one; say so back now, not whenever this
    // computer next happens to publish.
    if !linked.is_empty() {
        let person = Cfg {
            instance: String::new(),
            ..c.clone()
        };
        let text = format!("linked {} as another of my computers", linked.join(", "));
        let _ = post_with(&person, k, &text, None, "join", vec![], vec![], "", 0, None);
    }
    adopted
}

/// The label someone suggested, unless a different key already has it, in
/// which case the name their key derives to. A suggestion must never shadow
/// someone you already follow.
pub fn free_label(p: &Peer, peers: &[Peer]) -> String {
    let taken = |l: &str| peers.iter().any(|x| x.label == l && x.pubkey != p.pubkey);
    if p.label.is_empty() || p.label == p.pubkey[..8.min(p.pubkey.len())] || taken(&p.label) {
        name_for(&p.pubkey)
    } else {
        p.label.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(tag: &str) -> Keys {
        let d = std::env::temp_dir().join(format!("rf-invite-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        Keys::load_from(&d).unwrap()
    }

    #[test]
    fn an_invitation_round_trips_and_checks_out() {
        let a = keys("a");
        let me = a.identity_blob(
            "john",
            Some(crypto::Home {
                url: "https://relay.example/team".into(),
            }),
        );
        let inv = make(
            &a,
            "robofinger",
            "https://relay.example/team",
            vec![me],
            2_000,
        );
        let s = encode(&inv);
        assert!(s.starts_with("https://relay.example/team/join#"));
        assert!(is_invitation(&s) && !is_invitation("https://relay.example/team"));
        let back = decode(&s).unwrap();
        assert_eq!(back, inv);
        assert!(check(&back, 1_000).is_ok());
        assert!(check(&back, 3_000).unwrap_err().contains("expired"));
    }

    #[test]
    fn a_tampered_invitation_is_refused() {
        let a = keys("t");
        let me = a.identity_blob(
            "john",
            Some(crypto::Home {
                url: "https://relay.example/team".into(),
            }),
        );
        let mut inv = make(
            &a,
            "robofinger",
            "https://relay.example/team",
            vec![me],
            2_000,
        );
        inv.body.relay = "https://evil.example/team".into();
        assert!(check(&inv, 1_000).unwrap_err().contains("signature"));
        assert!(decode("https://relay.example/team/join#not-base64!").is_err());
    }

    /// A link is signed as one: dropping the flag, or adding a computer,
    /// breaks the signature, and an ordinary invitation signs as it always did.
    #[test]
    fn a_link_is_signed_as_a_link() {
        let a = keys("l");
        let me = a.identity_blob(
            "john",
            Some(crypto::Home {
                url: "https://relay.example/team".into(),
            }),
        );
        let inv = make_link(
            &a,
            "robofinger",
            "https://relay.example/team",
            vec![me.clone()],
            2_000,
            vec!["STUDIO".into()],
        );
        let back = decode(&encode(&inv)).unwrap();
        assert!(back.body.link && back.body.computers == ["STUDIO"]);
        assert!(check(&back, 1_000).is_ok());
        let mut plain = back.clone();
        plain.body.link = false;
        assert!(
            check(&plain, 1_000).is_err(),
            "a link cannot pass as an invitation"
        );
        let mut more = back;
        more.body.computers.push("MALLORY".into());
        assert!(check(&more, 1_000).is_err());
        let ordinary = make(
            &a,
            "robofinger",
            "https://relay.example/team",
            vec![me],
            2_000,
        );
        assert!(
            !serde_json::to_string(&ordinary.body)
                .unwrap()
                .contains("link")
        );
    }

    #[test]
    fn an_inviter_must_be_a_member() {
        let (a, b) = (keys("m1"), keys("m2"));
        let other = b.identity_blob(
            "ana",
            Some(crypto::Home {
                url: "https://relay.example/team".into(),
            }),
        );
        let inv = make(
            &a,
            "robofinger",
            "https://relay.example/team",
            vec![other],
            2_000,
        );
        assert!(check(&inv, 1_000).unwrap_err().contains("inviter"));
    }
}
