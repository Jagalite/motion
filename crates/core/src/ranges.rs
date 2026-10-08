#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeDecision {
    Full,
    Partial { start: u64, end: u64 },
    Unsatisfiable,
}

/// Single-range policy. Unsupported/malformed/multiple ranges are ignored.
/// Call only for GET, after evaluating preconditions and If-Range.
pub fn select_range(header: Option<&str>, len: u64) -> RangeDecision {
    use RangeDecision::*;
    let Some(value) = header.and_then(|s| s.strip_prefix("bytes=")) else {
        return Full;
    };
    if value.contains(',') {
        return Full;
    }
    let Some((start, end)) = value.trim().split_once('-') else {
        return Full;
    };
    let number = |s: &str| {
        if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) {
            s.parse::<u64>().ok()
        } else {
            None
        }
    };
    if start.is_empty() {
        let Some(count) = number(end) else {
            return Full;
        };
        if count == 0 || len == 0 {
            return Unsatisfiable;
        }
        return Partial {
            start: len.saturating_sub(count),
            end: len - 1,
        };
    }
    let Some(start) = number(start) else {
        return Full;
    };
    let end = if end.is_empty() {
        u64::MAX
    } else {
        let Some(n) = number(end) else { return Full };
        n
    };
    if start > end {
        return Full;
    }
    if start >= len {
        return Unsatisfiable;
    }
    Partial {
        start,
        end: end.min(len - 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn protocol_edges() {
        use RangeDecision::*;
        for (range, len, expected) in [
            ("bytes=0-3", 10, Partial { start: 0, end: 3 }),
            ("bytes=4-99", 10, Partial { start: 4, end: 9 }),
            ("bytes=-3", 10, Partial { start: 7, end: 9 }),
            ("bytes=-99", 10, Partial { start: 0, end: 9 }),
            ("bytes=4-", 10, Partial { start: 4, end: 9 }),
            ("bytes=0-3", 0, Unsatisfiable),
            ("bytes=-1", 0, Unsatisfiable),
            ("bytes=10-", 10, Unsatisfiable),
            ("bytes=-0", 10, Unsatisfiable),
            ("bytes=0-1,4-5", 10, Full),
            ("bytes=9-4", 10, Full),
            ("bytes=+1-3", 10, Full),
            ("bytes=18446744073709551616-", 10, Full),
        ] {
            assert_eq!(select_range(Some(range), len), expected, "{range}");
        }
    }
}
