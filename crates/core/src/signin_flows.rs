//! Sign-in orchestration decisions. Adapters observe current rows under their
//! writer lock, execute the exact plan, and commit its receipt with all effects.
use crate::{
    access::{AccessError, Device},
    access_admin::{self, User},
    signin::{self, Phase, Purpose, Transaction},
};
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    Oidc,
    Email,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Authority {
    pub authenticated: bool,
    pub administrator: bool,
    pub fresh_action: Option<&'static str>,
    pub owned_user: bool,
}
pub fn authentication_required(purpose: Purpose) -> bool {
    purpose != Purpose::Login
}
pub fn method_available(
    protocol: Protocol,
    purpose: Purpose,
    enabled: bool,
    prepared: bool,
    restore_hold: bool,
) -> bool {
    !restore_hold
        && prepared
        && (enabled || (protocol == Protocol::Oidc && purpose == Purpose::Test))
}
pub fn authority(protocol: Protocol, purpose: Purpose) -> Result<Authority, signin::Error> {
    let valid = match protocol {
        Protocol::Oidc => matches!(
            purpose,
            Purpose::Login | Purpose::Link | Purpose::Test | Purpose::Reauth
        ),
        Protocol::Email => matches!(purpose, Purpose::Login | Purpose::Enroll | Purpose::Reauth),
    };
    if !valid {
        return Err(signin::Error::Unavailable);
    }
    Ok(Authority {
        authenticated: authentication_required(purpose),
        administrator: purpose == Purpose::Test,
        fresh_action: match purpose {
            Purpose::Test => Some("provider.test"),
            Purpose::Link | Purpose::Enroll => Some("identity.link"),
            _ => None,
        },
        owned_user: !matches!(purpose, Purpose::Login | Purpose::Test),
    })
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserBinding {
    pub id: String,
    pub revision: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecipientIdentity {
    pub id: String,
    pub revision: u64,
    pub user: UserBinding,
    pub enabled: bool,
    pub user_enabled: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Recipient {
    IssueChallenge {
        user: UserBinding,
        identity: Option<(String, u64)>,
    },
    SilentPlaceholder,
}
pub fn email_recipient(
    purpose: Purpose,
    address_valid: bool,
    initiating: Option<&UserBinding>,
    identity: Option<&RecipientIdentity>,
) -> Recipient {
    if !address_valid {
        return Recipient::SilentPlaceholder;
    }
    let user = if purpose == Purpose::Enroll {
        initiating.filter(|u| identity.is_none_or(|i| i.user.id == u.id))
    } else {
        identity
            .filter(|i| {
                i.enabled
                    && i.user_enabled
                    && (purpose == Purpose::Login || initiating.is_some_and(|u| u.id == i.user.id))
            })
            .map(|i| &i.user)
    };
    match user {
        Some(user) => Recipient::IssueChallenge {
            user: user.clone(),
            identity: identity.map(|i| (i.id.clone(), i.revision)),
        },
        None => Recipient::SilentPlaceholder,
    }
}
/// Bind exactly the eligible recipient or erase all recipient evidence for a
/// silent placeholder. Deadlines, browser/epoch and actor revisions are retained.
pub fn bind_recipient(mut transaction: Transaction, recipient: &Recipient) -> Transaction {
    match recipient {
        Recipient::IssueChallenge { user, identity } => {
            transaction.user_id = Some(user.id.clone());
            transaction.user_revision = Some(user.revision);
            transaction.identity_revision = identity.as_ref().map(|i| i.1)
        }
        Recipient::SilentPlaceholder => {
            transaction.user_id = None;
            transaction.user_revision = None;
            transaction.identity_revision = None
        }
    }
    transaction
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Completion {
    Test,
    Link,
    Session { identity: String },
    PendingApproval,
    Registration,
}
pub fn completion(
    protocol: Protocol,
    purpose: Purpose,
    bound_user: Option<&str>,
    identity: Option<(&str, &str)>,
) -> Result<Completion, signin::Error> {
    match (protocol, purpose) {
        (Protocol::Oidc, Purpose::Test) => Ok(Completion::Test),
        (Protocol::Oidc, Purpose::Link) | (Protocol::Email, Purpose::Enroll) => {
            Ok(Completion::Link)
        }
        (Protocol::Email, Purpose::Register) => Ok(Completion::Registration),
        (_, Purpose::Login | Purpose::Reauth) => {
            if let Some((id, owner)) = identity {
                if protocol == Protocol::Email || purpose == Purpose::Reauth {
                    signin::link_owner(bound_user, owner)?;
                }
                Ok(Completion::Session {
                    identity: id.into(),
                })
            } else if protocol == Protocol::Oidc && purpose == Purpose::Login {
                Ok(Completion::PendingApproval)
            } else {
                Err(signin::Error::Unavailable)
            }
        }
        _ => Err(signin::Error::Unavailable),
    }
}
/// Only registration has a no-secret receipt replay. No family or credential
/// is reissued, even if the response that first carried the cookie was lost.
pub fn consumed_replay(transaction: &Transaction, browser_hash: Option<&str>) -> bool {
    transaction.purpose == Purpose::Register
        && transaction.phase == Phase::Consumed
        && browser_hash == Some(transaction.browser_hash.as_str())
}
pub fn protocol_failure(mut transaction: Transaction, denied: bool) -> (Transaction, &'static str) {
    transaction.phase = Phase::Failed;
    (transaction, if denied { "denied" } else { "failed" })
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Relink {
    Retain,
    Replace {
        identity: String,
        revision: u64,
        ceiling: Box<Device>,
        revoke_families: Vec<String>,
    },
}
pub fn relink(
    user: &User,
    initiating: &Device,
    previous: &Device,
    enabled: bool,
    families: &[String],
) -> Result<Relink, AccessError> {
    let next = access_admin::link_ceiling(user, initiating, Some(previous))?;
    if enabled && next.grant == previous.grant && next.policy == previous.policy {
        Ok(Relink::Retain)
    } else {
        Ok(Relink::Replace {
            identity: previous.id.clone(),
            revision: previous
                .revision
                .checked_add(1)
                .filter(|n| *n <= i64::MAX as u64)
                .ok_or(AccessError::StaleRevision)?,
            ceiling: Box::new(next),
            revoke_families: families.to_vec(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tx(purpose: Purpose) -> Transaction {
        Transaction {
            id: "tx".into(),
            purpose,
            browser_hash: "browser".into(),
            epoch: "e".into(),
            server_epoch: "s".into(),
            provider_revision: 4,
            user_id: None,
            user_revision: None,
            identity_revision: None,
            actor_revision: Some(3),
            expires_at: 600,
            phase: Phase::Pending,
            attempts: 0,
            registration: None,
        }
    }
    #[test]
    fn recipients_never_enumerate_or_substitute_foreign_and_disabled_identities() {
        let owner = UserBinding {
            id: "owner".into(),
            revision: 2,
        };
        for purpose in [Purpose::Login, Purpose::Enroll, Purpose::Reauth] {
            for valid in [false, true] {
                for enabled in [false, true] {
                    for active in [false, true] {
                        for foreign in [false, true] {
                            let identity = RecipientIdentity {
                                id: "identity".into(),
                                revision: 7,
                                user: UserBinding {
                                    id: if foreign { "foreign" } else { "owner" }.into(),
                                    revision: 2,
                                },
                                enabled,
                                user_enabled: active,
                            };
                            let decision =
                                email_recipient(purpose, valid, Some(&owner), Some(&identity));
                            let expected = valid
                                && if purpose == Purpose::Enroll {
                                    !foreign
                                } else {
                                    enabled && active && (purpose == Purpose::Login || !foreign)
                                };
                            assert_eq!(
                                matches!(decision, Recipient::IssueChallenge { .. }),
                                expected
                            );
                            let state = bind_recipient(tx(purpose), &decision);
                            assert_eq!(state.expires_at, 600);
                            assert_eq!(state.actor_revision, Some(3));
                            assert_eq!(state.identity_revision, expected.then_some(7));
                            assert_eq!(state.user_id, expected.then(|| identity.user.id.clone()));
                        }
                    }
                }
            }
        }
        assert!(matches!(
            email_recipient(Purpose::Login, true, None, None),
            Recipient::SilentPlaceholder
        ));
        assert!(matches!(
            email_recipient(Purpose::Enroll, true, Some(&owner), None),
            Recipient::IssueChallenge { identity: None, .. }
        ));
    }
    #[test]
    fn authority_completion_replay_and_protocol_failure_are_exact() {
        for purpose in [
            Purpose::Login,
            Purpose::Link,
            Purpose::Test,
            Purpose::Reauth,
            Purpose::Enroll,
            Purpose::Register,
        ] {
            for protocol in [Protocol::Oidc, Protocol::Email] {
                if let Ok(a) = authority(protocol, purpose) {
                    assert_eq!(a.authenticated, purpose != Purpose::Login);
                    assert_eq!(a.administrator, purpose == Purpose::Test);
                    assert_eq!(
                        a.owned_user,
                        !matches!(purpose, Purpose::Login | Purpose::Test)
                    );
                }
            }
        }
        assert_eq!(
            completion(Protocol::Oidc, Purpose::Login, None, None),
            Ok(Completion::PendingApproval)
        );
        assert_eq!(
            completion(Protocol::Email, Purpose::Login, None, None),
            Err(signin::Error::Unavailable)
        );
        assert_eq!(
            completion(Protocol::Oidc, Purpose::Reauth, Some("a"), Some(("i", "b"))),
            Err(signin::Error::IdentityConflict)
        );
        assert_eq!(
            completion(Protocol::Oidc, Purpose::Login, None, Some(("i", "b"))),
            Ok(Completion::Session {
                identity: "i".into()
            })
        );
        let mut registration = tx(Purpose::Register);
        registration.phase = Phase::Consumed;
        assert!(consumed_replay(&registration, Some("browser")));
        assert!(!consumed_replay(&registration, Some("other")));
        registration.purpose = Purpose::Login;
        assert!(!consumed_replay(&registration, Some("browser")));
        let (failed, result) = protocol_failure(tx(Purpose::Login), true);
        assert_eq!(failed.phase, Phase::Failed);
        assert_eq!(result, "denied");
    }
    #[test]
    fn relinking_emits_exact_dependent_family_set_only_when_authority_changes() {
        use crate::access::{Grant, Permission, Policy};
        let grant = Grant {
            profile_ids: ["profile".into()].into(),
            permissions: [Permission::CatalogRead].into(),
        };
        let user = User {
            id: "owner".into(),
            name: "Owner".into(),
            revision: 1,
            disabled: false,
            grant: grant.clone(),
            policy: Policy::default(),
        };
        let previous = Device::approved("identity".into(), grant);
        let families = vec!["family-a".into(), "family-b".into()];
        assert_eq!(
            relink(&user, &previous, &previous, true, &families),
            Ok(Relink::Retain)
        );
        let Relink::Replace {
            ceiling,
            revoke_families,
            ..
        } = relink(&user, &previous, &previous, false, &families).unwrap()
        else {
            panic!("disabled identity must invalidate families")
        };
        assert_eq!(revoke_families, families);
        assert_eq!(ceiling.grant, previous.grant);
        let mut narrower = previous.clone();
        narrower.grant.permissions.clear();
        let Relink::Replace {
            ceiling,
            revoke_families,
            ..
        } = relink(&user, &narrower, &previous, true, &families).unwrap()
        else {
            panic!("narrowed identity must invalidate families")
        };
        assert_eq!(revoke_families, families);
        assert!(ceiling.grant.permissions.is_empty());
    }
}
