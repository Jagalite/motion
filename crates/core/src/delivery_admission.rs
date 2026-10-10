//! Durable delivery admission decisions. The adapter must serialize observation
//! and insertion of the receipt, commit it with the initial delivery state, and
//! only then dispatch execution. Receipts outlive live transports.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Identity {
    pub principal: String,
    pub key: String,
    /// Canonical request identity, including source revision, tracks and start.
    pub digest: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Receipt {
    pub identity: Identity,
    pub delivery_id: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Create,
    Replay { delivery_id: String },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    KeyConflict,
    WrongScope,
}
/// Authorization must be revalidated by the authority adapter before this call.
/// Transport status is intentionally irrelevant: a retired receipt never creates
/// another encoder. A foreign principal's receipt must never be disclosed.
pub fn decide(request: &Identity, existing: Option<&Receipt>) -> Result<Decision, Error> {
    let Some(receipt) = existing else {
        return Ok(Decision::Create);
    };
    if receipt.identity.principal != request.principal || receipt.identity.key != request.key {
        return Err(Error::WrongScope);
    }
    if receipt.identity.digest != request.digest {
        return Err(Error::KeyConflict);
    }
    Ok(Decision::Replay {
        delivery_id: receipt.delivery_id.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_retries_replay_but_changed_requests_and_foreign_receipts_do_not() {
        let request = Identity {
            principal: "a".into(),
            key: "key".into(),
            digest: "source+tracks+start".into(),
        };
        assert_eq!(decide(&request, None), Ok(Decision::Create));
        let receipt = Receipt {
            identity: request.clone(),
            delivery_id: "retired-delivery".into(),
        };
        assert_eq!(
            decide(&request, Some(&receipt)),
            Ok(Decision::Replay {
                delivery_id: "retired-delivery".into()
            })
        );
        let mut changed = request.clone();
        changed.digest = "different".into();
        assert_eq!(decide(&changed, Some(&receipt)), Err(Error::KeyConflict));
        changed.principal = "b".into();
        assert_eq!(decide(&changed, Some(&receipt)), Err(Error::WrongScope));
    }
}
