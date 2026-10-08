//! Playback selection is a pure decision; planning never starts processing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Auto,
    Original,
    Convert,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Support {
    Unknown,
    Supported,
    Unsupported,
}
#[derive(Debug)]
pub struct Candidate {
    pub original: bool,
    pub available: bool,
    pub support: Support,
    pub matches_recipe: bool,
    pub within_budget: bool,
    pub can_process: bool,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    Play(usize),
    Prepare(usize),
    Blocked(&'static str),
}

/// `selected` pins an explicit version. It cannot silently fall back to another.
/// Candidates are ordered by the caller; originals and renditions retain that order.
pub fn decide(mode: Mode, candidates: &[Candidate], selected: Option<usize>) -> Decision {
    let allowed = |c: &Candidate| {
        c.available
            && c.support != Support::Unsupported
            && c.within_budget
            && match mode {
                Mode::Auto => true,
                Mode::Original => c.original,
                Mode::Convert => !c.original && c.matches_recipe,
            }
    };
    if let Some(index) = selected {
        return match candidates.get(index) {
            Some(c) if allowed(c) => Decision::Play(index),
            _ => Decision::Blocked("selected_version_unavailable"),
        };
    }
    // Original delivery takes precedence in Auto. Supported candidates take precedence
    // over unknown ones within each tier, without pretending an unknown is supported.
    for original in [true, false] {
        for support in [Support::Supported, Support::Unknown] {
            if let Some(index) = candidates
                .iter()
                .position(|c| c.original == original && c.support == support && allowed(c))
            {
                return Decision::Play(index);
            }
        }
    }
    if mode == Mode::Original {
        return Decision::Blocked("original_unavailable_or_unsupported");
    }
    // Re-encoding the same recipe does not resolve a known failure or budget
    // mismatch in its current output. Require another recipe/version instead.
    if candidates
        .iter()
        .any(|c| !c.original && c.available && c.matches_recipe)
    {
        return Decision::Blocked("prepared_version_unusable");
    }
    if let Some(index) = candidates
        .iter()
        .position(|c| c.original && c.available && c.can_process)
    {
        Decision::Prepare(index)
    } else {
        Decision::Blocked("no_processable_source")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn candidate(original: bool) -> Candidate {
        Candidate {
            original,
            available: true,
            support: Support::Unknown,
            matches_recipe: true,
            within_budget: true,
            can_process: original,
        }
    }
    #[test]
    fn strict_original_never_uses_rendition_or_prepares() {
        let mut c = [candidate(true), candidate(false)];
        c[0].support = Support::Unsupported;
        assert!(matches!(
            decide(Mode::Original, &c, None),
            Decision::Blocked(_)
        ));
        assert_eq!(decide(Mode::Auto, &c, None), Decision::Play(1));
        c[1].available = false;
        assert_eq!(decide(Mode::Auto, &c, None), Decision::Prepare(0));
    }
    #[test]
    fn conversion_requires_matching_current_output() {
        let mut c = [candidate(true), candidate(false)];
        assert_eq!(decide(Mode::Convert, &c, None), Decision::Play(1));
        c[1].matches_recipe = false;
        assert_eq!(decide(Mode::Convert, &c, None), Decision::Prepare(0));
        c[0].available = false;
        assert_eq!(
            decide(Mode::Convert, &c, None),
            Decision::Blocked("no_processable_source")
        );
    }
    #[test]
    fn explicit_version_and_budget_never_silently_fallback() {
        let mut c = [candidate(true), candidate(false)];
        c[0].within_budget = false;
        assert_eq!(decide(Mode::Auto, &c, None), Decision::Play(1));
        assert!(matches!(
            decide(Mode::Auto, &c, Some(0)),
            Decision::Blocked(_)
        ));
        assert!(matches!(
            decide(Mode::Original, &c, Some(1)),
            Decision::Blocked(_)
        ));
    }
    #[test]
    fn unusable_current_recipe_does_not_propose_identical_processing() {
        let mut c = [candidate(true), candidate(false)];
        c[1].within_budget = false;
        assert_eq!(
            decide(Mode::Convert, &c, None),
            Decision::Blocked("prepared_version_unusable")
        );
        c[1].within_budget = true;
        c[1].support = Support::Unsupported;
        assert_eq!(
            decide(Mode::Convert, &c, None),
            Decision::Blocked("prepared_version_unusable")
        );
        c[1].available = false;
        assert_eq!(decide(Mode::Convert, &c, None), Decision::Prepare(0));
    }
    #[test]
    fn unknown_is_attemptable_but_supported_wins_within_tier() {
        let mut c = [candidate(true), candidate(true), candidate(false)];
        c[1].support = Support::Supported;
        c[2].support = Support::Supported;
        assert_eq!(decide(Mode::Auto, &c, None), Decision::Play(1));
        c[1].available = false;
        assert_eq!(decide(Mode::Auto, &c, None), Decision::Play(0));
    }
}
