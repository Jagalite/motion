//! Bounded decoding of FFmpeg's advisory progress stream. Progress never decides
//! whether an output can be published; validation and the job reducer do that.

/// Numeric progress fields fit comfortably within this bound. Oversized lines
/// are discarded in full, including a suffix that resembles a valid field.
const MAX_LINE_BYTES: usize = 128;

pub(super) struct Decoder {
    line: [u8; MAX_LINE_BYTES],
    len: usize,
    oversized: bool,
}

impl Default for Decoder {
    fn default() -> Self {
        Self {
            line: [0; MAX_LINE_BYTES],
            len: 0,
            oversized: false,
        }
    }
}

impl Decoder {
    /// Consume one bounded read and return its last valid progress value.
    pub(super) fn push(&mut self, bytes: &[u8]) -> Option<f64> {
        let mut latest = None;
        for &byte in bytes {
            if byte == b'\n' {
                if let Some(seconds) = self.finish() {
                    latest = Some(seconds);
                }
            } else if !self.oversized {
                if self.len == MAX_LINE_BYTES {
                    self.oversized = true;
                } else {
                    self.line[self.len] = byte;
                    self.len += 1;
                }
            }
        }
        latest
    }

    /// Also handles a final line without a newline at EOF.
    pub(super) fn finish(&mut self) -> Option<f64> {
        let value = (!self.oversized)
            .then(|| std::str::from_utf8(&self.line[..self.len]).ok())
            .flatten()
            .map(|line| line.strip_suffix('\r').unwrap_or(line))
            .and_then(|line| line.strip_prefix("out_time_us="))
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| value.is_finite() && *value >= 0.0)
            .map(|micros| micros / 1_000_000.0);
        self.len = 0;
        self.oversized = false;
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragmented_progress_preserves_crlf_and_last_valid_value() {
        let input = b"frame=12\nout_time_us=1250000\r\nout_time_us=2500000\nprogress=end\n";
        for chunk_size in 1..=input.len() {
            let mut decoder = Decoder::default();
            let mut latest = None;
            for chunk in input.chunks(chunk_size) {
                latest = decoder.push(chunk).or(latest);
            }
            assert_eq!(latest, Some(2.5), "chunk size {chunk_size}");
            assert_eq!(decoder.finish(), None);
        }
    }

    #[test]
    fn invalid_fields_do_not_replace_progress_and_eof_flushes_once() {
        let mut decoder = Decoder::default();
        assert_eq!(decoder.push(b"out_time_us=1000000\nout_time_us=NaN\nout_time_us=inf\nout_time_us=-1\nout_time_us=\xff\n"), Some(1.0));
        assert_eq!(decoder.push(b"out_time_us=3e6"), None);
        assert_eq!(decoder.finish(), Some(3.0));
        assert_eq!(decoder.finish(), None);
    }

    #[test]
    fn oversized_line_is_discarded_until_newline_then_progress_recovers() {
        let mut decoder = Decoder::default();
        // Sixteen MiB without a newline retains only the fixed 128-byte prefix.
        let chunk = [b'x'; 4096];
        for _ in 0..4096 {
            assert_eq!(decoder.push(&chunk), None);
            assert_eq!(decoder.len, MAX_LINE_BYTES);
            assert!(decoder.oversized);
        }
        assert_eq!(decoder.push(b"out_time_us=9000000\n"), None);
        assert_eq!(decoder.push(b"out_time_us=4000000\n"), Some(4.0));
        assert_eq!(decoder.len, 0);
        assert!(!decoder.oversized);
    }

    #[test]
    fn line_limit_is_inclusive_and_oversized_eof_does_not_parse_a_prefix() {
        let prefix = b"out_time_us=";
        for extra in [0, 1] {
            let mut line = prefix.to_vec();
            line.resize(MAX_LINE_BYTES + extra, b'0');
            let mut decoder = Decoder::default();
            assert_eq!(decoder.push(&line), None);
            assert_eq!(decoder.finish(), (extra == 0).then_some(0.0));
            assert_eq!(decoder.push(b"out_time_us=1000000\n"), Some(1.0));
        }
    }
}
