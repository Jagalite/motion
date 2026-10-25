//! A distinct command attempt requires a synchronous, strictly older-generation
//! refusal. Receipt absence and ambiguous transport outcomes are not evidence.
use sha2::{Digest, Sha256};
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    NotOlder,
    Exhausted,
    InvalidIdentity,
}
pub fn next_attempt(current: u32, expected: i64, observed: i64) -> Result<u32, Refusal> {
    if expected < 0 || observed <= expected {
        return Err(Refusal::NotOlder);
    }
    if current >= 31 {
        return Err(Refusal::Exhausted);
    }
    Ok(current + 1)
}
pub fn operation_id(original_digest: &str, attempt: u32) -> Result<String, Refusal> {
    if original_digest.len() != 64
        || !original_digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || !(1..=31).contains(&attempt)
    {
        return Err(Refusal::InvalidIdentity);
    }
    Ok(format!(
        "motion-attempt-{:x}",
        Sha256::digest(format!(
            "motion-native-attempt-v1:{original_digest}:{attempt}"
        ))
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_strictly_older_bounded_attempts_are_admitted() {
        assert_eq!(next_attempt(0, 4, 5), Ok(1));
        assert_eq!(next_attempt(30, 4, 9), Ok(31));
        assert_eq!(next_attempt(31, 4, 9), Err(Refusal::Exhausted));
        assert_eq!(next_attempt(0, 5, 5), Err(Refusal::NotOlder));
        assert_eq!(next_attempt(0, 6, 5), Err(Refusal::NotOlder));
        let digest = "a".repeat(64);
        assert_ne!(
            operation_id(&digest, 1).unwrap(),
            operation_id(&digest, 2).unwrap()
        );
        assert_eq!(operation_id(&digest, 1), operation_id(&digest, 1));
    }
}
