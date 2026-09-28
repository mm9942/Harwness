//! Bounded head + tail capture of a job's stdout/stderr.
//!
//! A [`JobResult`](super::JobResult) keeps, per stream, the first
//! [`OutputCapture::head_bytes`] and the last [`OutputCapture::tail_bytes`]
//! bytes. Everything in between is counted but dropped, so memory stays
//! bounded by `head_bytes + tail_bytes` per stream no matter how much a job
//! writes. The tail matters most for verification evidence: test failures
//! and summaries are printed at the end.
//!
//! ```text
//! written:  [ head_bytes ][ ....... omitted ....... ][ tail_bytes ]
//! captured: [ head_bytes ][ tail_bytes ]            (omitted counted)
//! ```
//!
//! Output is handled as raw bytes; a cut may split a UTF-8 sequence.

use std::collections::VecDeque;

/// Default number of leading bytes kept per stream.
pub const DEFAULT_OUTPUT_HEAD_BYTES: usize = 16 * 1024;
/// Default number of trailing bytes kept per stream.
pub const DEFAULT_OUTPUT_TAIL_BYTES: usize = 64 * 1024;

/// Capture policy for stdout/stderr: keep the first `head_bytes` and the
/// last `tail_bytes` of each stream, count the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OutputCapture {
    /// Leading bytes kept per stream.
    pub head_bytes: usize,
    /// Trailing bytes kept per stream (ring buffer).
    pub tail_bytes: usize,
}

impl OutputCapture {
    /// A policy keeping `head_bytes` leading and `tail_bytes` trailing bytes.
    #[must_use]
    pub const fn new(head_bytes: usize, tail_bytes: usize) -> Self {
        Self {
            head_bytes,
            tail_bytes,
        }
    }

    /// Upper bound of captured bytes per stream.
    #[must_use]
    pub const fn retained_bytes(self) -> usize {
        self.head_bytes.saturating_add(self.tail_bytes)
    }

    /// Splits a captured stream at the omitted gap into `(head, tail)`.
    ///
    /// Without a gap (`omitted == 0`) nothing was dropped: both halves are
    /// the whole capture, which is then contiguous output.
    #[must_use]
    pub fn split(self, captured: &[u8], omitted: u64) -> (&[u8], &[u8]) {
        if omitted == 0 {
            return (captured, captured);
        }
        // With a gap the ring buffer was full, so the tail is exactly the
        // last `tail_bytes` (clamped for hand-built results).
        captured.split_at(captured.len().saturating_sub(self.tail_bytes))
    }
}

impl Default for OutputCapture {
    fn default() -> Self {
        Self::new(DEFAULT_OUTPUT_HEAD_BYTES, DEFAULT_OUTPUT_TAIL_BYTES)
    }
}

/// Capture state of one stream.
#[derive(Debug, Default)]
struct StreamCapture {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    total: u64,
}

impl StreamCapture {
    fn push(&mut self, policy: OutputCapture, chunk: &[u8]) {
        self.total = self
            .total
            .saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
        let room = policy.head_bytes.saturating_sub(self.head.len());
        let (head_part, rest) = chunk.split_at(chunk.len().min(room));
        self.head.extend_from_slice(head_part);
        if rest.is_empty() || policy.tail_bytes == 0 {
            return;
        }
        if rest.len() >= policy.tail_bytes {
            let (_, last) = rest.split_at(rest.len() - policy.tail_bytes);
            self.tail.clear();
            self.tail.extend(last.iter().copied());
        } else {
            let overflow = (self.tail.len() + rest.len()).saturating_sub(policy.tail_bytes);
            self.tail.drain(..overflow);
            self.tail.extend(rest.iter().copied());
        }
    }

    /// Head + tail concatenated, and the number of bytes omitted between.
    fn finish(self) -> (Vec<u8>, u64) {
        let mut bytes = self.head;
        bytes.reserve(self.tail.len());
        bytes.extend(self.tail);
        let kept = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        (bytes, self.total.saturating_sub(kept))
    }
}

/// Bounded capture of stdout and stderr of one attempt.
#[derive(Debug, Default)]
pub(super) struct Output {
    policy: OutputCapture,
    stdout: StreamCapture,
    stderr: StreamCapture,
}

/// A finished capture, ready for a [`JobResult`](super::JobResult).
pub(super) struct CapturedOutput {
    pub(super) policy: OutputCapture,
    pub(super) stdout: Vec<u8>,
    pub(super) stdout_omitted: u64,
    pub(super) stderr: Vec<u8>,
    pub(super) stderr_omitted: u64,
}

impl Output {
    pub(super) fn new(policy: OutputCapture) -> Self {
        Self {
            policy,
            stdout: StreamCapture::default(),
            stderr: StreamCapture::default(),
        }
    }

    pub(super) fn stdout(&mut self, chunk: &[u8]) {
        self.stdout.push(self.policy, chunk);
    }

    pub(super) fn stderr(&mut self, chunk: &[u8]) {
        self.stderr.push(self.policy, chunk);
    }

    pub(super) fn finish(self) -> CapturedOutput {
        let (stdout, stdout_omitted) = self.stdout.finish();
        let (stderr, stderr_omitted) = self.stderr.finish();
        CapturedOutput {
            policy: self.policy,
            stdout,
            stdout_omitted,
            stderr,
            stderr_omitted,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Output, OutputCapture};

    #[test]
    fn small_output_is_kept_whole() {
        let mut output = Output::new(OutputCapture::new(4, 4));
        output.stdout(b"ab");
        output.stdout(b"cdef");
        output.stderr(b"xy");
        let captured = output.finish();
        assert_eq!(captured.stdout, b"abcdef");
        assert_eq!(captured.stdout_omitted, 0);
        assert_eq!(captured.stderr, b"xy");
        assert_eq!(captured.stderr_omitted, 0);
        let (head, tail) = captured.policy.split(&captured.stdout, 0);
        assert_eq!(head, b"abcdef");
        assert_eq!(tail, b"abcdef");
    }

    #[test]
    fn exactly_head_plus_tail_is_not_truncated() {
        let mut output = Output::new(OutputCapture::new(3, 3));
        output.stdout(b"abcdef");
        let captured = output.finish();
        assert_eq!(captured.stdout, b"abcdef");
        assert_eq!(captured.stdout_omitted, 0);
    }

    #[test]
    fn large_output_keeps_head_and_tail() {
        let policy = OutputCapture::new(4, 6);
        let mut output = Output::new(policy);
        // 1000 bytes in uneven chunks, including non-UTF-8 bytes.
        let data: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
        for chunk in data.chunks(7) {
            output.stdout(chunk);
        }
        output.stderr(b"\xff\xfe");
        let captured = output.finish();
        let mut expected = data.get(..4).unwrap_or_default().to_vec();
        expected.extend_from_slice(data.get(994..).unwrap_or_default());
        assert_eq!(captured.stdout, expected);
        assert_eq!(captured.stdout_omitted, 990);
        assert_eq!(captured.stderr, b"\xff\xfe");
        assert_eq!(captured.stderr_omitted, 0);
        let (head, tail) = policy.split(&captured.stdout, captured.stdout_omitted);
        assert_eq!(head, data.get(..4).unwrap_or_default());
        assert_eq!(tail, data.get(994..).unwrap_or_default());
    }

    #[test]
    fn one_huge_chunk_fills_head_and_tail() {
        let mut output = Output::new(OutputCapture::new(2, 3));
        output.stdout(b"0123456789");
        output.stdout(b"ab");
        let captured = output.finish();
        assert_eq!(captured.stdout, b"019ab");
        assert_eq!(captured.stdout_omitted, 7);
    }

    #[test]
    fn head_only_and_tail_only_policies() {
        let mut head_only = Output::new(OutputCapture::new(4, 0));
        head_only.stdout(b"abcdef");
        let captured = head_only.finish();
        assert_eq!(captured.stdout, b"abcd");
        assert_eq!(captured.stdout_omitted, 2);
        let (head, tail) = captured.policy.split(&captured.stdout, 2);
        assert_eq!(head, b"abcd");
        assert!(tail.is_empty());

        let mut tail_only = Output::new(OutputCapture::new(0, 4));
        tail_only.stdout(b"abc");
        tail_only.stdout(b"def");
        let captured = tail_only.finish();
        assert_eq!(captured.stdout, b"cdef");
        assert_eq!(captured.stdout_omitted, 2);
    }
}
