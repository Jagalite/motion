//! Production grant composition and explicit device ownership boundaries.
use playscale_core::{
    access::{AccessError, Device, Grant, Permission, Policy, RatingEligibility},
    access_admin::{self as admin, User},
};
use std::collections::BTreeSet;

fn strings(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn user() -> User {
    User {
        id: "user".into(),
        name: "Person".into(),
        revision: 3,
        disabled: false,
        grant: Grant {
            profile_ids: strings(&["profile"]),
            permissions: [Permission::CatalogRead, Permission::PlaybackRequest].into(),
        },
        policy: Policy {
            library_ids: strings(&["library"]),
            ..Policy::default()
        },
    }
}

fn device() -> Device {
    let user = user();
    let mut device = Device::approved("device".into(), user.grant);
    device.policy = user.policy;
    device
}

#[test]
fn legacy_policies_remain_unrestricted_for_ratings_and_bottom_is_explicit() {
    let policy: Policy = serde_json::from_str(
        r#"{"library_ids":["library"],"allow_unrated":true,"allowed_ratings":[],"blocked_labels":[]}"#,
    )
    .unwrap();
    assert_eq!(policy.rating_eligibility(), RatingEligibility::Any);
    assert!(!policy.rating_restricted());
    let mut user = user();
    let mut device = device();
    user.policy.allowed_ratings = strings(&["G"]);
    device.policy.allowed_ratings = strings(&["R"]);
    let next = admin::constrain(&device, &user).unwrap();
    assert_eq!(next.policy.library_ids, strings(&["library"]));
    assert_eq!(next.policy.rating_eligibility(), RatingEligibility::None);
    assert!(next.policy.deny_rated && next.policy.allowed_ratings.is_empty());
    assert!(next.policy.allow_unrated && next.policy.rating_restricted());
    assert!(admin::within(&next.grant, &next.policy, &user));
    assert_eq!(
        serde_json::from_value::<Policy>(serde_json::to_value(&next.policy).unwrap()).unwrap(),
        next.policy
    );
}

#[test]
fn composition_respects_each_effective_ceiling_including_mixed_administration() {
    let eligibility = [
        RatingEligibility::Any,
        RatingEligibility::None,
        RatingEligibility::Only(strings(&["G"])),
        RatingEligibility::Only(strings(&["R"])),
        RatingEligibility::Only(strings(&["G", "R"])),
    ];
    // Independent finite eligibility oracle includes an unnamed future rating.
    let universe = strings(&["G", "R", "future"]);
    let expand = |value: &RatingEligibility| match value {
        RatingEligibility::Any => universe.clone(),
        RatingEligibility::None => BTreeSet::new(),
        RatingEligibility::Only(values) => values.clone(),
    };
    for left in &eligibility {
        for right in &eligibility {
            for user_admin in [false, true] {
                for device_admin in [false, true] {
                    let mut user = user();
                    let mut device = device();
                    user.policy.set_rating_eligibility(left.clone());
                    device.policy.set_rating_eligibility(right.clone());
                    user.policy.blocked_labels = strings(&["user-label"]);
                    device.policy.blocked_labels = strings(&["device-label"]);
                    user.policy.allow_unrated = false;
                    if user_admin {
                        user.grant.permissions = [Permission::SystemAdmin].into();
                        user.grant.profile_ids.clear();
                        user.policy.library_ids.clear();
                    }
                    if device_admin {
                        device.grant.permissions = [Permission::SystemAdmin].into();
                        device.grant.profile_ids.clear();
                        device.policy.library_ids.clear();
                    }
                    let next = admin::constrain(&device, &user).unwrap();
                    assert_eq!(
                        next.grant.permissions.contains(&Permission::SystemAdmin),
                        user_admin && device_admin
                    );
                    match (user_admin, device_admin) {
                        (true, _) => {
                            assert_eq!(next.grant, device.grant);
                            assert_eq!(next.policy, device.policy);
                        }
                        (false, true) => {
                            assert_eq!(next.grant, user.grant);
                            assert_eq!(next.policy, user.policy);
                        }
                        (false, false) => {
                            let expected: BTreeSet<_> =
                                expand(left).intersection(&expand(right)).cloned().collect();
                            assert_eq!(expand(&next.policy.rating_eligibility()), expected);
                            assert_eq!(next.policy.library_ids, strings(&["library"]));
                            assert_eq!(
                                next.policy.blocked_labels,
                                strings(&["user-label", "device-label"])
                            );
                            assert!(!next.policy.allow_unrated);
                        }
                    }
                    assert!(admin::within(&next.grant, &next.policy, &user));
                }
            }
        }
    }
}

#[test]
fn unassigned_is_distinct_from_missing_mismatched_and_disabled_owners() {
    let device = device();
    let mut user = user();
    assert!(admin::owner_allows(&device, None, None));
    assert!(!admin::owner_allows(&device, Some("user"), None));
    assert!(!admin::owner_allows(&device, Some("other"), Some(&user)));
    assert!(!admin::owner_allows(&device, None, Some(&user)));
    assert!(admin::owner_allows(&device, Some("user"), Some(&user)));
    user.disabled = true;
    assert!(!admin::owner_allows(&device, Some("user"), Some(&user)));
}

#[test]
fn assignment_pins_both_revisions_and_never_transfers_or_revives() {
    let device = device();
    let mut user = user();
    user.grant.permissions.remove(&Permission::PlaybackRequest);
    let assigned = admin::assign_owner(&device, None, 1, &user, 3).unwrap();
    assert_eq!(assigned.revision, 2);
    assert_eq!(assigned.grant.permissions, [Permission::CatalogRead].into());
    assert_eq!(assigned.id, device.id);
    assert_eq!(assigned.generation, device.generation);
    for (owner, device_revision, user_revision) in [
        (Some("user"), 1, 3),
        (Some("other"), 1, 3),
        (None, 0, 3),
        (None, 1, 2),
    ] {
        assert_eq!(
            admin::assign_owner(&device, owner, device_revision, &user, user_revision),
            Err(AccessError::StaleRevision)
        );
    }
    user.disabled = true;
    assert_eq!(
        admin::assign_owner(&device, None, 1, &user, 3),
        Err(AccessError::UserDisabled)
    );
    user.disabled = false;
    let mut revoked = device;
    revoked.revoked = true;
    assert_eq!(
        admin::assign_owner(&revoked, None, 1, &user, 3),
        Err(AccessError::DeviceRevoked)
    );
}

#[test]
fn widening_and_reenabling_never_restore_materialized_authority() {
    let mut user = user();
    let mut device = device();
    device.policy.allowed_ratings = strings(&["R"]);
    user.policy.allowed_ratings = strings(&["G"]);
    user.grant.permissions.remove(&Permission::PlaybackRequest);
    let narrowed = admin::constrain(&device, &user).unwrap();
    user.policy.allowed_ratings.clear();
    user.grant.permissions.insert(Permission::SystemAdmin);
    let widened = admin::constrain(&narrowed, &user).unwrap();
    assert_eq!(widened.grant, narrowed.grant);
    assert_eq!(widened.policy, narrowed.policy);
    user.disabled = true;
    let disabled = admin::constrain(&widened, &user).unwrap();
    assert!(disabled.revoked);
    user.disabled = false;
    assert!(admin::constrain(&disabled, &user).unwrap().revoked);
}

#[test]
fn linked_and_reenabled_methods_preserve_the_initiating_device_ceiling() {
    let mut owner = user();
    owner.grant.permissions.insert(Permission::SystemAdmin);
    let initiating = device();
    let new = admin::link_ceiling(&owner, &initiating, None).unwrap();
    assert_eq!(new.grant, initiating.grant);
    assert_eq!(new.policy, initiating.policy);
    let mut previous = initiating.clone();
    previous.grant.permissions.insert(Permission::SystemAdmin);
    let relinked = admin::link_ceiling(&owner, &initiating, Some(&previous)).unwrap();
    assert_eq!(relinked.grant, initiating.grant);
    assert_eq!(relinked.policy, initiating.policy);
    assert!(!admin::adds_administrator(&previous, &relinked));
    assert!(admin::adds_administrator(&initiating, &previous));
    owner.disabled = true;
    assert_eq!(
        admin::link_ceiling(&owner, &initiating, None),
        Err(AccessError::UserDisabled)
    );
}

#[test]
fn revoked_device_is_not_reconstrained_by_later_user_edits() {
    let mut device = device();
    device.revoked = true;
    device.revision = u64::MAX;
    device.policy.blocked_labels = (0..200).map(|i| format!("old-{i}")).collect();
    for disabled in [false, true] {
        let mut owner = user();
        owner.disabled = disabled;
        owner.policy.blocked_labels.insert("new-restriction".into());
        assert_eq!(admin::constrain(&device, &owner).unwrap(), device);
    }
}
