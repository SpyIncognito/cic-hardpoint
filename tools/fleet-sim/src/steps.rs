//! Scripted steps for testing a peer-to-peer host: at a time after start, one member drops,
//! rejoins by key, leaves, or joins with a given invite (such as one with an old code, to see
//! the refusal).

use std::str::FromStr;
use std::time::Duration;

use fleet_proto::SendConsent;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Drop,
    Rejoin,
    Leave,
    Join(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub at: Duration,
    /// From 0; written from 1.
    pub member: usize,
    pub action: Action,
}

impl FromStr for Step {
    type Err = String;

    /// `SECS:drop|rejoin|leave:MEMBER` or `SECS:join:MEMBER:INVITE`, members from 1.
    fn from_str(text: &str) -> Result<Self, String> {
        let mut parts = text.splitn(4, ':');
        let usage = || format!("{text:?} is not SECS:ACTION:MEMBER[:INVITE]");
        let at: u64 = parts
            .next()
            .and_then(|s| s.trim().parse().ok())
            .ok_or_else(usage)?;
        let action = parts.next().ok_or_else(usage)?.trim().to_owned();
        let member: usize = parts
            .next()
            .and_then(|s| s.trim().parse().ok())
            .filter(|m| *m >= 1)
            .ok_or_else(usage)?;
        let action = match (action.as_str(), parts.next()) {
            ("drop", None) => Action::Drop,
            ("rejoin", None) => Action::Rejoin,
            ("leave", None) => Action::Leave,
            ("join", Some(invite)) if !invite.trim().is_empty() => {
                Action::Join(invite.trim().to_owned())
            }
            _ => return Err(usage()),
        };
        Ok(Self {
            at: Duration::from_secs(at),
            member: member - 1,
            action,
        })
    }
}

impl Step {
    /// The action's word, for the log (never the invite).
    pub fn action_name(&self) -> &'static str {
        match self.action {
            Action::Drop => "drop",
            Action::Rejoin => "rejoin",
            Action::Leave => "leave",
            Action::Join(_) => "join",
        }
    }
}

/// `all`, `none`, or field ids separated by commas.
pub fn parse_consent(text: &str) -> Result<SendConsent, String> {
    match text.trim() {
        "all" => Ok(crate::default_consent()),
        "none" => Ok(SendConsent::none()),
        list => {
            let mut consent = SendConsent::none();
            for id in list.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                if !consent.set(id, true) {
                    return Err(format!("--consent: unknown field {id:?}"));
                }
            }
            Ok(consent)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_and_consent_parse() {
        assert_eq!(
            "5:drop:2".parse::<Step>().unwrap(),
            Step {
                at: Duration::from_secs(5),
                member: 1,
                action: Action::Drop
            }
        );
        let join: Step = "9:join:1:cic://h:1?key=k&code=a".parse().unwrap();
        assert_eq!(
            join.action,
            Action::Join("cic://h:1?key=k&code=a".to_owned())
        );
        for bad in [
            "x:drop:1",
            "5:drop:0",
            "5:fly:1",
            "5:join:1",
            "5:drop:1:extra",
        ] {
            assert!(bad.parse::<Step>().is_err(), "{bad}");
        }
        let c = parse_consent("ship.hull, ship.name").unwrap();
        assert!(c.allows("ship.hull") && !c.allows("char.health"));
        assert!(parse_consent("ship.nope").is_err());
        assert!(!parse_consent("none").unwrap().allows("ship.hull"));
        assert!(parse_consent("all").unwrap().allows("detail.power"));
    }
}
