//! The two choices every participant makes (plan section 0):
//!
//! 1. What I send: [`SendConsent`], field by field, applied in the sender's own app before
//!    anything leaves it. A field not chosen is never transmitted.
//! 2. What I see: [`Subscription`], the viewer's chosen fields, which also travels to senders so
//!    they send less.
//!
//! A sender sends the intersection: fields it consents to that some viewer asked for.

use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::card::{Card, Consent, FIELDS, FieldDef, Sharing, field_def};

/// A set of known field ids. Unknown ids are left out on reading.
fn read_ids<'de, D: Deserializer<'de>>(d: D) -> Result<BTreeSet<&'static str>, D::Error> {
    let ids = Vec::<String>::deserialize(d)?;
    Ok(ids
        .iter()
        .filter_map(|id| field_def(id))
        .map(|f| f.id)
        .collect())
}

/// Written in [`FIELDS`] order.
fn write_ids<S: Serializer>(ids: &BTreeSet<&'static str>, s: S) -> Result<S::Ok, S::Error> {
    let ordered: Vec<&str> = FIELDS
        .iter()
        .filter(|f| ids.contains(f.id))
        .map(|f| f.id)
        .collect();
    ordered.serialize(s)
}

/// The fields a sender agrees to send. Nothing is sent until turned on. Ship and character
/// status stay as consent groups ([`SendConsent::group`]), refined to single fields. The card's
/// `state` (UP, DOWNED, ...) goes with any character field.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendConsent {
    #[serde(serialize_with = "write_ids", deserialize_with = "read_ids")]
    fields: BTreeSet<&'static str>,
}

impl SendConsent {
    pub fn none() -> Self {
        Self::default()
    }

    /// Every field of the groups `sharing` turns on.
    pub fn groups(sharing: Sharing) -> Self {
        Self {
            fields: FIELDS
                .iter()
                .filter(|f| sharing.allows(f.consent))
                .map(|f| f.id)
                .collect(),
        }
    }

    /// Turn a whole group on or off.
    pub fn group(&mut self, consent: Consent, on: bool) {
        for f in FIELDS.iter().filter(|f| f.consent == consent) {
            self.set(f.id, on);
        }
    }

    /// Turn one field on or off. False for an unknown field.
    pub fn set(&mut self, id: &str, on: bool) -> bool {
        let Some(def) = field_def(id) else {
            return false;
        };
        if on {
            self.fields.insert(def.id);
        } else {
            self.fields.remove(def.id);
        }
        true
    }

    pub fn allows(&self, id: &str) -> bool {
        self.fields.contains(id)
    }

    /// The groups this consent sends anything of, as the card states them.
    pub fn sharing(&self) -> Sharing {
        let any = |c| FIELDS.iter().any(|f| f.consent == c && self.allows(f.id));
        Sharing {
            ship: any(Consent::Ship),
            character: any(Consent::Character),
            detail: any(Consent::Detail),
        }
    }
}

/// The fields a viewer asks for. A field not asked for is not sent to them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subscription {
    #[serde(serialize_with = "write_ids", deserialize_with = "read_ids")]
    pub fields: BTreeSet<&'static str>,
}

impl Subscription {
    pub fn of<'a>(ids: impl IntoIterator<Item = &'a str>) -> Self {
        Self {
            fields: ids
                .into_iter()
                .filter_map(field_def)
                .map(|f| f.id)
                .collect(),
        }
    }

    pub fn wants(&self, id: &str) -> bool {
        self.fields.contains(id)
    }

    /// Every field either subscription asks for, for a sender with several viewers.
    pub fn union(&self, other: &Self) -> Self {
        Self {
            fields: self.fields.union(&other.fields).copied().collect(),
        }
    }
}

/// The fields a sender may put on a card, in display order: consented to and wanted.
pub fn sendable<'a>(
    consent: &'a SendConsent,
    wanted: impl Fn(&str) -> bool + 'a,
) -> impl Iterator<Item = &'static FieldDef> + 'a {
    FIELDS
        .iter()
        .filter(move |f| consent.allows(f.id) && wanted(f.id))
}

/// The last guard before a card leaves the sender: drops every field outside the consent and the
/// subscription, and the state without character consent, and states the sharing it carries.
pub fn filter_card(card: &mut Card, consent: &SendConsent, subscription: &Subscription) {
    card.fields
        .retain(|id| consent.allows(id) && subscription.wants(id));
    card.sharing = consent.sharing();
    if !card.sharing.character {
        card.state = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::card::{CARD_SCHEMA, FieldValue, Fields, Life, Tone};

    #[test]
    fn consent_is_per_field_within_groups() {
        let mut c = SendConsent::none();
        assert_eq!(c.sharing(), Sharing::default());
        c.set("char.health", true);
        assert!(c.allows("char.health"));
        assert!(!c.allows("char.oxygen"));
        assert_eq!(
            c.sharing(),
            Sharing {
                character: true,
                ..Sharing::default()
            }
        );
        assert!(!c.set("char.secret", true));
        c.group(Consent::Ship, true);
        assert!(c.allows("ship.hull") && c.allows("ship.name"));
        c.group(Consent::Ship, false);
        assert!(!c.allows("ship.hull"));
        assert_eq!(SendConsent::groups(Sharing::ALL).sharing(), Sharing::ALL);
    }

    #[test]
    fn a_sender_sends_the_intersection() {
        let mut c = SendConsent::none();
        c.set("ship.hull", true);
        c.set("ship.name", true);
        let s = Subscription::of(["ship.hull", "char.health"]);
        let ids: Vec<_> = sendable(&c, |id| s.wants(id)).map(|f| f.id).collect();
        assert_eq!(ids, ["ship.hull"]);
    }

    #[test]
    fn filter_card_drops_what_is_not_consented_or_wanted() {
        let mut card = Card {
            schema: CARD_SCHEMA,
            member: "m".to_owned(),
            handle: "M".to_owned(),
            sharing: Sharing::ALL,
            state: Some(Life::Downed),
            fields: Fields(vec![
                ("ship.name", FieldValue::text("x", Tone::Info)),
                ("ship.hull", FieldValue::text("x", Tone::Info)),
                ("char.health", FieldValue::text("x", Tone::Info)),
            ]),
            source: None,
        };
        let mut c = SendConsent::none();
        c.group(Consent::Ship, true);
        filter_card(
            &mut card,
            &c,
            &Subscription::of(["ship.hull", "char.health"]),
        );
        assert_eq!(card.fields.ids().collect::<Vec<_>>(), ["ship.hull"]);
        assert_eq!(card.state, None);
        assert!(card.check().is_ok());
    }

    #[test]
    fn consent_and_subscriptions_read_known_ids_only() {
        let s: Subscription =
            serde_json::from_str(r#"{"fields":["char.health","nope","ship.name"]}"#).unwrap();
        assert_eq!(
            serde_json::to_string(&s).unwrap(),
            r#"{"fields":["ship.name","char.health"]}"#
        );
        let c: SendConsent = serde_json::from_str(r#"{"fields":["ship.hull"]}"#).unwrap();
        assert!(c.allows("ship.hull"));
        let u = s.union(&Subscription::of(["ship.hull"]));
        assert_eq!(u.fields.len(), 3);
    }
}
