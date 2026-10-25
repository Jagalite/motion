//! Fresh-target installation ordering only. No filesystem or restore authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Prepared,
    IntentDurable,
    MotionDurable,
    NativeDurable,
    Complete,
    RecoveryRequired,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    IntentSynced,
    MotionSynced,
    NativeSynced,
    CompletionSynced,
    Uncertain,
}
pub fn advance(phase: Phase, input: Input) -> Result<Phase, &'static str> {
    use Input::*;
    use Phase::*;
    match (phase, input) {
        (Prepared, IntentSynced) => Ok(IntentDurable),
        (IntentDurable, MotionSynced) => Ok(MotionDurable),
        (MotionDurable, NativeSynced) => Ok(NativeDurable),
        (NativeDurable, CompletionSynced) => Ok(Complete),
        (Prepared | IntentDurable | MotionDurable | NativeDurable, Uncertain) => {
            Ok(RecoveryRequired)
        }
        _ => Err("invalid pair installation transition"),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn images_cannot_precede_durable_intent_or_skip_completion() {
        assert!(advance(Phase::Prepared, Input::MotionSynced).is_err());
        assert!(advance(Phase::IntentDurable, Input::NativeSynced).is_err());
        let mut p = Phase::Prepared;
        for i in [
            Input::IntentSynced,
            Input::MotionSynced,
            Input::NativeSynced,
            Input::CompletionSynced,
        ] {
            p = advance(p, i).unwrap();
        }
        assert_eq!(p, Phase::Complete);
    }
    #[test]
    fn partial_or_unknown_install_cannot_be_retried_as_new() {
        for p in [
            Phase::Prepared,
            Phase::IntentDurable,
            Phase::MotionDurable,
            Phase::NativeDurable,
        ] {
            let p = advance(p, Input::Uncertain).unwrap();
            assert_eq!(p, Phase::RecoveryRequired);
            assert!(advance(p, Input::IntentSynced).is_err());
            assert!(advance(p, Input::CompletionSynced).is_err());
        }
    }
}
