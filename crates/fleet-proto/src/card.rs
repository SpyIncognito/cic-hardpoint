//! The fleet card: what one member shares. Its fields, their consent groups, and the values a
//! viewer draws. Building a card from game state is the app's job; this is only the shape.

use std::fmt;

use serde::de::{self, MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Bumped when a card's shape changes in a way an older viewer would misread.
pub const CARD_SCHEMA: u32 = 1;
/// A member id (the fleet identity) is 1 to 64 of `A-Z a-z 0-9 _ -`.
pub const MAX_MEMBER_ID: usize = 64;
/// A handle is 1 to 40 characters of plain text.
pub const MAX_HANDLE: usize = 40;

/// The consent groups a sender's fields belong to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Consent {
    /// Ship status: name, mode, shields, hull, fuel, cautions, weapons, in a ship or not.
    Ship,
    /// Character status: health, oxygen, on foot or seated, and the UP/DOWNED/DEAD state.
    Character,
    /// The detail view: shield faces, power, speed and altitude. Sent only while a viewer has
    /// that member's detail open.
    Detail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FieldDef {
    pub id: &'static str,
    pub label: &'static str,
    pub consent: Consent,
}

const fn field(id: &'static str, label: &'static str, consent: Consent) -> FieldDef {
    FieldDef { id, label, consent }
}

/// Every field a card can carry, in display order.
pub const FIELDS: &[FieldDef] = &[
    field("ship.name", "Ship", Consent::Ship),
    field("ship.mode", "Master mode", Consent::Ship),
    field("ship.shields", "Shields", Consent::Ship),
    field("ship.hull", "Hull", Consent::Ship),
    field("ship.fuel", "Hydrogen fuel", Consent::Ship),
    field("ship.quantum", "Quantum fuel", Consent::Ship),
    field("ship.cautions", "Cautions", Consent::Ship),
    field("ship.weapons", "Weapons ready", Consent::Ship),
    field("ship.aboard", "In a ship", Consent::Ship),
    field("char.health", "Health", Consent::Character),
    field("char.oxygen", "Oxygen", Consent::Character),
    field("char.activity", "On foot or seated", Consent::Character),
    field("detail.shield_faces", "Shield faces", Consent::Detail),
    field("detail.power", "Power", Consent::Detail),
    field("detail.speed", "Speed", Consent::Detail),
    field("detail.altitude", "Altitude", Consent::Detail),
];

pub fn field_def(id: &str) -> Option<&'static FieldDef> {
    FIELDS.iter().find(|f| f.id == id)
}

fn field_index(id: &str) -> usize {
    FIELDS.iter().position(|f| f.id == id).unwrap_or(usize::MAX)
}

/// Fields a viewer can choose for the cards they see; detail fields come with the detail view.
pub fn card_fields() -> impl Iterator<Item = &'static FieldDef> {
    FIELDS.iter().filter(|f| f.consent != Consent::Detail)
}

pub fn detail_fields() -> impl Iterator<Item = &'static FieldDef> {
    FIELDS.iter().filter(|f| f.consent == Consent::Detail)
}

pub fn is_member_id(s: &str) -> bool {
    (1..=MAX_MEMBER_ID).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

pub fn is_handle(s: &str) -> bool {
    (1..=MAX_HANDLE).contains(&s.chars().count())
        && !s.chars().any(|c| c.is_control() || c == '<' || c == '>')
}

/// Which consent groups a card carries fields of. A group is on when the sender sends at least
/// one of its fields.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sharing {
    pub ship: bool,
    pub character: bool,
    pub detail: bool,
}

impl Sharing {
    pub const ALL: Self = Self {
        ship: true,
        character: true,
        detail: true,
    };

    pub fn allows(self, consent: Consent) -> bool {
        match consent {
            Consent::Ship => self.ship,
            Consent::Character => self.character,
            Consent::Detail => self.detail,
        }
    }
}

/// The member's own state, as the sender's app tracks it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Life {
    Up,
    Downed,
    Dead,
    Respawned,
}

/// How a value reads at a glance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tone {
    Ok,
    Warn,
    Bad,
    Off,
    Info,
    Unknown,
}

/// One row of a list field, such as one shield face.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reading {
    pub label: String,
    pub text: String,
    pub tone: Tone,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fraction: Option<f64>,
}

/// One field's value: text as the viewer shows it, a tone, a 0..1 fraction for a bar, and rows
/// for the list fields of the detail view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldValue {
    pub text: String,
    pub tone: Tone,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fraction: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<Reading>>,
}

impl FieldValue {
    pub fn text(text: impl Into<String>, tone: Tone) -> Self {
        Self {
            text: text.into(),
            tone,
            fraction: None,
            items: None,
        }
    }
}

/// Fields by id, in [`FIELDS`] order. On reading, unknown ids are left out (a newer sender may
/// know more fields) and a repeated id keeps its last value.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Fields(pub Vec<(&'static str, FieldValue)>);

impl Fields {
    pub fn get(&self, id: &str) -> Option<&FieldValue> {
        self.0.iter().find(|(f, _)| *f == id).map(|(_, v)| v)
    }

    pub fn ids(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.0.iter().map(|(id, _)| *id)
    }

    pub fn retain(&mut self, mut keep: impl FnMut(&'static str) -> bool) {
        self.0.retain(|(id, _)| keep(id));
    }
}

impl Serialize for Fields {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(Some(self.0.len()))?;
        for (id, value) in &self.0 {
            map.serialize_entry(id, value)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for Fields {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct FieldsVisitor;

        impl<'de> Visitor<'de> for FieldsVisitor {
            type Value = Fields;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a map of field ids to values")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Fields, A::Error> {
                let mut fields: Vec<(&'static str, FieldValue)> = Vec::new();
                while let Some(id) = map.next_key::<String>()? {
                    let Some(def) = field_def(&id) else {
                        map.next_value::<de::IgnoredAny>()?;
                        continue;
                    };
                    let value = map.next_value::<FieldValue>()?;
                    fields.retain(|(f, _)| *f != def.id);
                    fields.push((def.id, value));
                }
                fields.sort_by_key(|(id, _)| field_index(id));
                Ok(Fields(fields))
            }
        }

        d.deserialize_map(FieldsVisitor)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Card {
    pub schema: u32,
    /// The member's fleet identity (their public key's text form): stable, unlike the handle.
    pub member: String,
    /// The name the member chose for their card.
    pub handle: String,
    pub sharing: Sharing,
    /// Only with character sharing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<Life>,
    pub fields: Fields,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CardProblem {
    #[error("card schema {0} is newer than {CARD_SCHEMA}")]
    NewerSchema(u32),
    #[error("bad member id")]
    BadMember,
    #[error("bad handle")]
    BadHandle,
    #[error("field {0} is outside what the card says it shares")]
    NotShared(&'static str),
    #[error("state is sent without character sharing")]
    StateNotShared,
}

impl Card {
    /// Whether a received card is one this side can read, and is consistent with its own
    /// `sharing`.
    pub fn check(&self) -> Result<(), CardProblem> {
        if self.schema > CARD_SCHEMA {
            return Err(CardProblem::NewerSchema(self.schema));
        }
        if !is_member_id(&self.member) {
            return Err(CardProblem::BadMember);
        }
        if !is_handle(&self.handle) {
            return Err(CardProblem::BadHandle);
        }
        for id in self.fields.ids() {
            let allowed = field_def(id).is_some_and(|d| self.sharing.allows(d.consent));
            if !allowed {
                return Err(CardProblem::NotShared(id));
            }
        }
        if self.state.is_some() && !self.sharing.character {
            return Err(CardProblem::StateNotShared);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn card() -> Card {
        Card {
            schema: CARD_SCHEMA,
            member: "m-1".to_owned(),
            handle: "Tester".to_owned(),
            sharing: Sharing {
                ship: true,
                ..Sharing::default()
            },
            state: None,
            fields: Fields(vec![
                ("ship.name", FieldValue::text("Cutter", Tone::Info)),
                ("ship.hull", FieldValue::text("80%", Tone::Ok)),
            ]),
        }
    }

    #[test]
    fn a_card_round_trips_through_json() {
        let c = card();
        let text = serde_json::to_string(&c).unwrap();
        let back: Card = serde_json::from_str(&text).unwrap();
        assert_eq!(back, c);
        assert!(back.check().is_ok());
    }

    #[test]
    fn reading_leaves_out_unknown_fields_and_keeps_display_order() {
        let v = json!({
            "schema": 1, "member": "m-1", "handle": "T", "sharing": { "ship": true, "character": false, "detail": false },
            "fields": {
                "ship.hull": { "text": "1", "tone": "ok" },
                "ship.future": { "text": "x", "tone": "info" },
                "ship.name": { "text": "2", "tone": "info" },
            },
        });
        let c: Card = serde_json::from_value(v).unwrap();
        assert_eq!(
            c.fields.ids().collect::<Vec<_>>(),
            ["ship.name", "ship.hull"]
        );
    }

    #[test]
    fn check_refuses_what_the_card_does_not_share() {
        let mut c = card();
        c.fields
            .0
            .push(("char.health", FieldValue::text("1", Tone::Ok)));
        assert_eq!(c.check(), Err(CardProblem::NotShared("char.health")));

        let mut c = card();
        c.state = Some(Life::Up);
        assert_eq!(c.check(), Err(CardProblem::StateNotShared));

        let mut c = card();
        c.schema = CARD_SCHEMA + 1;
        assert_eq!(c.check(), Err(CardProblem::NewerSchema(CARD_SCHEMA + 1)));

        let mut c = card();
        c.member = "bad id!".to_owned();
        assert_eq!(c.check(), Err(CardProblem::BadMember));

        let mut c = card();
        c.handle = "<b>".to_owned();
        assert_eq!(c.check(), Err(CardProblem::BadHandle));
    }
}
