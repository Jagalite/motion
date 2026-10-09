//! Shared execution capacity. A reservation belongs to one execution owner and is
//! released only when that owner reports confirmed termination. Cancellation fences
//! the owner; it does not free capacity. Adapters supply exits and stuck
//! classifications as observations and execute Start/Terminate effects.
//!
//! Every accepted request receives a ticket that is never reused. Cancellation,
//! exit and stuck observations name the ticket, so a delayed observation from an
//! earlier execution cannot release or fence a later request by the same owner.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Class {
    /// Playback delivery. May use the reserve that background work cannot.
    Interactive,
    Preparation,
    Inventory,
    Maintenance,
}
impl Class {
    const BACKGROUND: [Class; 3] = [Class::Preparation, Class::Inventory, Class::Maintenance];
    pub fn interactive(self) -> bool {
        self == Class::Interactive
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Budget {
    pub units: u32,
    /// Units background classes can never occupy.
    pub interactive_reserve: u32,
}
impl Budget {
    pub fn valid(&self) -> bool {
        self.units > 0 && self.interactive_reserve < self.units
    }
    /// The most units one owner of `class` can ever hold.
    pub fn limit(&self, class: Class) -> u32 {
        if class.interactive() {
            self.units
        } else {
            self.units - self.interactive_reserve
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Hold {
    Running,
    /// Termination requested; the owner may still be executing.
    Terminating,
    /// Termination did not complete in time. Capacity stays reserved until an exit
    /// is observed, so a replacement cannot oversubscribe a live worker.
    Stuck,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Reservation {
    pub owner: String,
    pub class: Class,
    pub units: u32,
    pub hold: Hold,
    /// Recorded from a previous process at recovery, regardless of budget.
    #[serde(default)]
    pub adopted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Waiter {
    pub ticket: u64,
    pub owner: String,
    pub class: Class,
    pub units: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Ledger {
    pub budget: Budget,
    /// Keyed by ticket.
    pub held: BTreeMap<u64, Reservation>,
    /// Arrival order. Within a class admission is strictly first-in, first-out.
    pub waiting: Vec<Waiter>,
    /// Background class admitted most recently; rotation continues after it.
    pub last_background: Option<Class>,
    /// Last issued ticket; tickets are never reused.
    pub last_ticket: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Input {
    Request {
        owner: String,
        class: Class,
        units: u32,
    },
    /// Record an execution that survived a previous process (its witness is
    /// still held). It is held as stuck even if that exceeds the budget, so no
    /// new work is admitted beside it until it exits.
    Adopt {
        owner: String,
        class: Class,
        units: u32,
    },
    /// Withdraw a waiting request, or fence and request termination of a holder.
    Cancel { ticket: u64 },
    /// The execution owner observed that its process tree terminated and was reaped.
    Exited { ticket: u64 },
    /// Termination was requested but has not been confirmed within its deadline.
    Stuck { ticket: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Effect {
    /// The request was accepted under `ticket` (also emitted for an identical retry).
    Accepted {
        owner: String,
        ticket: u64,
    },
    Start {
        ticket: u64,
    },
    Terminate {
        ticket: u64,
    },
    Released {
        ticket: u64,
        units: u32,
    },
    Rejected {
        owner: String,
        reason: Rejection,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rejection {
    ExceedsBudget,
    OwnerConflict,
    TicketsExhausted,
}

impl Ledger {
    pub fn new(budget: Budget) -> Self {
        assert!(budget.valid(), "invalid execution budget");
        Self {
            budget,
            held: BTreeMap::new(),
            waiting: Vec::new(),
            last_background: None,
            last_ticket: 0,
        }
    }
    fn sum(&self, include: impl Fn(&Reservation) -> bool) -> u64 {
        self.held
            .values()
            .filter(|r| include(r))
            .map(|r| u64::from(r.units))
            .sum()
    }
    pub fn used(&self) -> u64 {
        self.sum(|_| true)
    }
    pub fn background_used(&self) -> u64 {
        self.sum(|r| !r.class.interactive())
    }
    fn fits_with(&self, class: Class, units: u32, used: u64, background: u64) -> bool {
        let units = u64::from(units);
        used + units <= u64::from(self.budget.units)
            && (class.interactive() || background + units <= u64::from(self.budget.limit(class)))
    }
    pub fn fits(&self, class: Class, units: u32) -> bool {
        self.fits_with(class, units, self.used(), self.background_used())
    }
    /// Whether the request could fit once every live (non-stuck) holder exits.
    /// A request blocked only by stuck capacity must not hold priority over others.
    pub fn fits_after_live_exit(&self, class: Class, units: u32) -> bool {
        let stuck = |r: &Reservation| r.hold == Hold::Stuck;
        self.fits_with(
            class,
            units,
            self.sum(stuck),
            self.sum(|r| stuck(r) && !r.class.interactive()),
        )
    }
    pub fn holds(&self, ticket: u64) -> Option<&Reservation> {
        self.held.get(&ticket)
    }
    pub fn is_waiting(&self, ticket: u64) -> bool {
        self.waiting.iter().any(|w| w.ticket == ticket)
    }
    pub fn ticket(&self, owner: &str) -> Option<u64> {
        self.held
            .iter()
            .find(|(_, r)| r.owner == owner)
            .map(|(t, _)| *t)
            .or_else(|| {
                self.waiting
                    .iter()
                    .find(|w| w.owner == owner)
                    .map(|w| w.ticket)
            })
    }

    /// Waiters in policy order: interactive first, then background classes rotating
    /// after the one admitted most recently; arrival order within each class.
    fn ordered(&self) -> Vec<usize> {
        let start = self
            .last_background
            .and_then(|c| Class::BACKGROUND.iter().position(|b| *b == c))
            .map_or(0, |i| i + 1);
        std::iter::once(Class::Interactive)
            .chain(
                (0..Class::BACKGROUND.len())
                    .map(|k| Class::BACKGROUND[(start + k) % Class::BACKGROUND.len()]),
            )
            .flat_map(|class| {
                self.waiting
                    .iter()
                    .enumerate()
                    .filter(move |(_, w)| w.class == class)
                    .map(|(i, _)| i)
            })
            .collect()
    }

    /// Admit waiters in policy order. A waiter that does not fit stops all later
    /// admission (so larger requests are not starved by smaller later ones), unless
    /// it is blocked only by stuck capacity, which may never be returned; then later
    /// waiters may proceed. Once the stuck owner exits it regains its priority.
    fn admit(&mut self, effects: &mut Vec<Effect>) {
        'admit: loop {
            for index in self.ordered() {
                let w = &self.waiting[index];
                if self.fits(w.class, w.units) {
                    let w = self.waiting.remove(index);
                    if !w.class.interactive() {
                        self.last_background = Some(w.class);
                    }
                    effects.push(Effect::Start { ticket: w.ticket });
                    self.held.insert(
                        w.ticket,
                        Reservation {
                            owner: w.owner,
                            class: w.class,
                            units: w.units,
                            hold: Hold::Running,
                            adopted: false,
                        },
                    );
                    continue 'admit;
                }
                if self.fits_after_live_exit(w.class, w.units) {
                    return;
                }
            }
            return;
        }
    }
}

/// The production reducer, also exercised directly by the Stateless model.
pub fn transition(before: &Ledger, input: &Input) -> (Ledger, Vec<Effect>) {
    let mut next = before.clone();
    let mut effects = Vec::new();
    match input {
        Input::Request {
            owner,
            class,
            units,
        } => {
            let reject = |reason| Effect::Rejected {
                owner: owner.clone(),
                reason,
            };
            let known = next.ticket(owner).map(|ticket| {
                let shape = next.holds(ticket).map(|r| (r.class, r.units)).or_else(|| {
                    next.waiting
                        .iter()
                        .find(|w| w.ticket == ticket)
                        .map(|w| (w.class, w.units))
                });
                (ticket, shape)
            });
            match known {
                // An identical retry is acknowledged without a second reservation.
                Some((ticket, shape)) if shape == Some((*class, *units)) => {
                    effects.push(Effect::Accepted {
                        owner: owner.clone(),
                        ticket,
                    })
                }
                Some(_) => effects.push(reject(Rejection::OwnerConflict)),
                None if *units == 0 || *units > next.budget.limit(*class) => {
                    effects.push(reject(Rejection::ExceedsBudget))
                }
                None if next.last_ticket == u64::MAX => {
                    effects.push(reject(Rejection::TicketsExhausted))
                }
                None => {
                    next.last_ticket += 1;
                    let ticket = next.last_ticket;
                    effects.push(Effect::Accepted {
                        owner: owner.clone(),
                        ticket,
                    });
                    next.waiting.push(Waiter {
                        ticket,
                        owner: owner.clone(),
                        class: *class,
                        units: *units,
                    });
                    next.admit(&mut effects);
                }
            }
        }
        Input::Adopt {
            owner,
            class,
            units,
        } => {
            if next.ticket(owner).is_some() {
                effects.push(Effect::Rejected {
                    owner: owner.clone(),
                    reason: Rejection::OwnerConflict,
                });
            } else if next.last_ticket == u64::MAX {
                effects.push(Effect::Rejected {
                    owner: owner.clone(),
                    reason: Rejection::TicketsExhausted,
                });
            } else {
                next.last_ticket += 1;
                let ticket = next.last_ticket;
                next.held.insert(
                    ticket,
                    Reservation {
                        owner: owner.clone(),
                        class: *class,
                        units: *units,
                        hold: Hold::Stuck,
                        adopted: true,
                    },
                );
                effects.push(Effect::Accepted {
                    owner: owner.clone(),
                    ticket,
                });
            }
        }
        Input::Cancel { ticket } => {
            if let Some(index) = next.waiting.iter().position(|w| w.ticket == *ticket) {
                // A withdrawn head may have been the only obstacle for others.
                next.waiting.remove(index);
                next.admit(&mut effects);
            } else if let Some(r) = next.held.get_mut(ticket)
                && r.hold == Hold::Running
            {
                r.hold = Hold::Terminating;
                effects.push(Effect::Terminate { ticket: *ticket });
            }
        }
        Input::Stuck { ticket } => {
            if let Some(r) = next.held.get_mut(ticket)
                && r.hold == Hold::Terminating
            {
                r.hold = Hold::Stuck;
                // Waiters blocked by this owner may now be blocked only by stuck
                // capacity, which no longer holds priority over other classes.
                next.admit(&mut effects);
            }
        }
        Input::Exited { ticket } => {
            if let Some(r) = next.held.remove(ticket) {
                effects.push(Effect::Released {
                    ticket: *ticket,
                    units: r.units,
                });
                next.admit(&mut effects);
            }
        }
    }
    (next, effects)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request(owner: &str, class: Class, units: u32) -> Input {
        Input::Request {
            owner: owner.into(),
            class,
            units,
        }
    }
    fn run(ledger: &mut Ledger, input: Input) -> Vec<Effect> {
        let (next, effects) = transition(ledger, &input);
        *ledger = next;
        effects
    }
    /// Effects other than request acknowledgements.
    fn act(ledger: &mut Ledger, input: Input) -> Vec<Effect> {
        run(ledger, input)
            .into_iter()
            .filter(|e| !matches!(e, Effect::Accepted { .. }))
            .collect()
    }
    fn start(ticket: u64) -> Effect {
        Effect::Start { ticket }
    }
    fn released(ticket: u64, units: u32) -> Effect {
        Effect::Released { ticket, units }
    }

    #[test]
    fn stuck_owner_keeps_capacity_until_exit() {
        let mut l = Ledger::new(Budget {
            units: 2,
            interactive_reserve: 1,
        });
        assert_eq!(
            run(&mut l, request("a", Class::Preparation, 1)),
            [
                Effect::Accepted {
                    owner: "a".into(),
                    ticket: 1
                },
                start(1)
            ]
        );
        assert_eq!(act(&mut l, request("b", Class::Preparation, 1)), []);
        assert_eq!(
            act(&mut l, Input::Cancel { ticket: 1 }),
            [Effect::Terminate { ticket: 1 }]
        );
        assert_eq!(act(&mut l, Input::Stuck { ticket: 1 }), []);
        assert_eq!(l.holds(1).unwrap().hold, Hold::Stuck);
        assert!(l.is_waiting(2));
        // A second cancellation does not re-request termination or release.
        assert_eq!(act(&mut l, Input::Cancel { ticket: 1 }), []);
        assert_eq!(
            act(&mut l, Input::Exited { ticket: 1 }),
            [released(1, 1), start(2)]
        );
        assert_eq!(act(&mut l, Input::Exited { ticket: 1 }), []);
    }

    #[test]
    fn stale_observations_cannot_touch_a_reused_owner() {
        let mut l = Ledger::new(Budget {
            units: 1,
            interactive_reserve: 0,
        });
        act(&mut l, request("a", Class::Preparation, 1));
        act(&mut l, Input::Exited { ticket: 1 });
        act(&mut l, request("a", Class::Preparation, 1));
        assert_eq!(l.ticket("a"), Some(2));
        for stale in [
            Input::Exited { ticket: 1 },
            Input::Cancel { ticket: 1 },
            Input::Stuck { ticket: 1 },
        ] {
            assert_eq!(run(&mut l, stale), []);
        }
        assert_eq!(l.holds(2).unwrap().hold, Hold::Running);
    }

    #[test]
    fn reserve_is_interactive_only_and_interactive_goes_first() {
        let mut l = Ledger::new(Budget {
            units: 3,
            interactive_reserve: 1,
        });
        assert_eq!(act(&mut l, request("p", Class::Preparation, 2)), [start(1)]);
        assert_eq!(act(&mut l, request("i", Class::Inventory, 1)), []);
        assert_eq!(act(&mut l, request("v", Class::Interactive, 1)), [start(3)]);
        assert_eq!(act(&mut l, request("w", Class::Interactive, 2)), []);
        // Background cannot take capacity freed while interactive work waits.
        assert_eq!(act(&mut l, Input::Exited { ticket: 3 }), [released(3, 1)]);
        assert_eq!(
            act(&mut l, Input::Exited { ticket: 1 }),
            [released(1, 2), start(4), start(2)]
        );
        assert_eq!(
            act(&mut l, request("x", Class::Interactive, 4)),
            [Effect::Rejected {
                owner: "x".into(),
                reason: Rejection::ExceedsBudget
            }]
        );
        assert_eq!(
            act(&mut l, request("y", Class::Maintenance, 3)),
            [Effect::Rejected {
                owner: "y".into(),
                reason: Rejection::ExceedsBudget
            }]
        );
    }

    #[test]
    fn background_classes_rotate_and_heads_are_not_bypassed() {
        let mut l = Ledger::new(Budget {
            units: 2,
            interactive_reserve: 0,
        });
        act(&mut l, request("p1", Class::Preparation, 2));
        act(&mut l, request("p2", Class::Preparation, 1));
        act(&mut l, request("m1", Class::Maintenance, 2));
        act(&mut l, request("m2", Class::Maintenance, 1));
        // Maintenance is due after preparation; its large head blocks p2 rather
        // than being overtaken by smaller work.
        assert_eq!(
            act(&mut l, Input::Exited { ticket: 1 }),
            [released(1, 2), start(3)]
        );
        assert_eq!(
            act(&mut l, Input::Exited { ticket: 3 }),
            [released(3, 2), start(2), start(4)]
        );
        assert_eq!(
            act(&mut l, request("p2", Class::Preparation, 2)),
            [Effect::Rejected {
                owner: "p2".into(),
                reason: Rejection::OwnerConflict
            }]
        );
        assert_eq!(
            run(&mut l, request("p2", Class::Preparation, 1)),
            [Effect::Accepted {
                owner: "p2".into(),
                ticket: 2
            }]
        );
    }

    #[test]
    fn stuck_capacity_does_not_let_one_head_block_other_classes() {
        let mut l = Ledger::new(Budget {
            units: 3,
            interactive_reserve: 1,
        });
        act(&mut l, request("p", Class::Preparation, 1));
        act(&mut l, request("i", Class::Inventory, 2));
        act(&mut l, request("m", Class::Maintenance, 1));
        assert!(l.is_waiting(2) && l.is_waiting(3));
        act(&mut l, Input::Cancel { ticket: 1 });
        // Inventory can never fit beside the stuck holder; maintenance proceeds.
        assert_eq!(act(&mut l, Input::Stuck { ticket: 1 }), [start(3)]);
        assert!(l.is_waiting(2));
        assert_eq!(act(&mut l, Input::Exited { ticket: 1 }), [released(1, 1)]);
        assert_eq!(
            act(&mut l, Input::Exited { ticket: 3 }),
            [released(3, 1), start(2)]
        );
    }

    #[test]
    fn large_budgets_do_not_overflow() {
        let mut l = Ledger::new(Budget {
            units: u32::MAX,
            interactive_reserve: 0,
        });
        assert_eq!(
            act(&mut l, request("a", Class::Interactive, u32::MAX)),
            [start(1)]
        );
        assert_eq!(act(&mut l, request("b", Class::Interactive, 1)), []);
        assert_eq!(l.used(), u64::from(u32::MAX));
    }

    #[test]
    fn adopted_survivors_hold_capacity_even_beyond_budget() {
        let mut l = Ledger::new(Budget {
            units: 2,
            interactive_reserve: 1,
        });
        let adopt = |owner: &str| Input::Adopt {
            owner: owner.into(),
            class: Class::Preparation,
            units: 2,
        };
        run(&mut l, adopt("old-1"));
        run(&mut l, adopt("old-2"));
        assert_eq!(l.used(), 4);
        assert_eq!(act(&mut l, request("v", Class::Interactive, 1)), []);
        assert_eq!(act(&mut l, Input::Exited { ticket: 1 }), [released(1, 2)]);
        assert_eq!(
            act(&mut l, Input::Exited { ticket: 2 }),
            [released(2, 2), start(3)]
        );
    }
}
