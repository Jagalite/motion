use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Queued,
    Running,
    Cancelling,
    Cancelled,
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Job {
    pub phase: Phase,
    pub attempt: u32,
}

impl Default for Job {
    fn default() -> Self {
        Self {
            phase: Phase::Queued,
            attempt: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Input {
    Start,
    Cancel,
    Finished { attempt: u32, success: bool },
    Retry,
    Recover,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Effect {
    Run { attempt: u32 },
    Stop { attempt: u32 },
    Publish { attempt: u32 },
}

/// The production reducer, also exercised directly by the Stateless adapter.
pub fn transition(before: &Job, input: Input) -> (Job, Vec<Effect>) {
    use Phase::*;
    let mut next = before.clone();
    let effects = match (before.phase, input) {
        (Queued, Input::Start) if before.attempt < u32::MAX => {
            next.attempt += 1;
            next.phase = Running;
            vec![Effect::Run {
                attempt: next.attempt,
            }]
        }
        (Queued, Input::Start) => {
            next.phase = Failed;
            vec![]
        }
        (Queued, Input::Cancel) => {
            next.phase = Cancelled;
            vec![]
        }
        (Running, Input::Cancel) => {
            next.phase = Cancelling;
            vec![Effect::Stop {
                attempt: before.attempt,
            }]
        }
        (Running, Input::Finished { attempt, success }) if attempt == before.attempt => {
            next.phase = if success { Completed } else { Failed };
            if success {
                vec![Effect::Publish { attempt }]
            } else {
                vec![]
            }
        }
        (Cancelling, Input::Finished { attempt, .. }) if attempt == before.attempt => {
            next.phase = Cancelled;
            vec![]
        }
        (Failed | Cancelled, Input::Retry) if before.attempt < u32::MAX => {
            next.phase = Queued;
            vec![]
        }
        (Running, Input::Recover) => {
            next.phase = if before.attempt == u32::MAX {
                Failed
            } else {
                Queued
            };
            vec![]
        }
        (Cancelling, Input::Recover) => {
            next.phase = Cancelled;
            vec![]
        }
        _ => vec![],
    };
    (next, effects)
}

impl Job {
    pub fn executing(&self, attempt: u32) -> bool {
        self.phase == Phase::Running && self.attempt == attempt
    }
    pub fn retryable(&self) -> bool {
        matches!(self.phase, Phase::Failed | Phase::Cancelled) && self.attempt < u32::MAX
    }
}
