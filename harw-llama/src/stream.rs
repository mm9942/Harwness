//! Splits the raw model output into reasoning, visible text and tool-call markup,
//! and reassembles UTF-8 from token pieces.

const THINK_OPEN: &str = "<think>";
const THINK_CLOSE: &str = "</think>";
const TOOL_OPEN: &str = "<tool_call>";

/// One streamed fragment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Piece {
    /// Text inside `<think>...</think>`.
    Reasoning(String),
    /// Visible assistant text.
    Text(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Think,
    Text,
    Tool,
}

/// What the model produced, once generation is over.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SplitOutput {
    /// Visible text, trimmed.
    pub text: String,
    /// Reasoning text, trimmed.
    pub reasoning: String,
    /// Raw markup from the first `<tool_call>` on.
    pub tool_raw: String,
}

/// Incremental state machine over the generated text.
#[derive(Debug)]
pub(crate) struct OutputSplitter {
    mode: Mode,
    pending: String,
    out: SplitOutput,
    text_started: bool,
    reasoning_started: bool,
}

/// Length of the longest suffix of `s` that is a proper prefix of one of `tags`.
fn partial_suffix(s: &str, tags: &[&str]) -> usize {
    let mut best = 0;
    for tag in tags {
        let max = tag.len().saturating_sub(1).min(s.len());
        for k in (1..=max).rev() {
            if k <= best {
                break;
            }
            if s.is_char_boundary(s.len() - k) && s.ends_with(&tag[..k]) {
                best = k;
                break;
            }
        }
    }
    best
}

impl OutputSplitter {
    /// `start_in_think`: the prompt already opened a `<think>` block.
    pub(crate) fn new(start_in_think: bool) -> Self {
        Self {
            mode: if start_in_think { Mode::Think } else { Mode::Text },
            pending: String::new(),
            out: SplitOutput::default(),
            text_started: false,
            reasoning_started: false,
        }
    }

    fn emit_reasoning(&mut self, s: &str, pieces: &mut Vec<Piece>) {
        let s = if self.reasoning_started { s } else { s.trim_start() };
        if s.is_empty() {
            return;
        }
        self.reasoning_started = true;
        self.out.reasoning.push_str(s);
        pieces.push(Piece::Reasoning(s.to_owned()));
    }

    fn emit_text(&mut self, s: &str, pieces: &mut Vec<Piece>) {
        let s = if self.text_started { s } else { s.trim_start() };
        if s.is_empty() {
            return;
        }
        self.text_started = true;
        self.out.text.push_str(s);
        pieces.push(Piece::Text(s.to_owned()));
    }

    /// Feeds one decoded chunk; fragments that are safe to show are appended to `pieces`.
    pub(crate) fn push(&mut self, chunk: &str, pieces: &mut Vec<Piece>) {
        self.pending.push_str(chunk);
        loop {
            match self.mode {
                Mode::Tool => {
                    let rest = std::mem::take(&mut self.pending);
                    self.out.tool_raw.push_str(&rest);
                    return;
                }
                Mode::Think => {
                    if let Some(i) = self.pending.find(THINK_CLOSE) {
                        let before: String = self.pending[..i].to_owned();
                        self.pending.drain(..i + THINK_CLOSE.len());
                        self.emit_reasoning(&before, pieces);
                        self.mode = Mode::Text;
                    } else {
                        let cut = self.pending.len() - partial_suffix(&self.pending, &[THINK_CLOSE]);
                        let head: String = self.pending[..cut].to_owned();
                        self.pending.drain(..cut);
                        self.emit_reasoning(&head, pieces);
                        return;
                    }
                }
                Mode::Text => {
                    let think = self.pending.find(THINK_OPEN);
                    let tool = self.pending.find(TOOL_OPEN);
                    let hit = match (think, tool) {
                        (Some(a), Some(b)) if a < b => Some((a, Mode::Think, THINK_OPEN)),
                        (Some(_), Some(b)) => Some((b, Mode::Tool, TOOL_OPEN)),
                        (Some(a), None) => Some((a, Mode::Think, THINK_OPEN)),
                        (None, Some(b)) => Some((b, Mode::Tool, TOOL_OPEN)),
                        (None, None) => None,
                    };
                    if let Some((i, next, tag)) = hit {
                        let before: String = self.pending[..i].to_owned();
                        self.pending.drain(..i + tag.len());
                        self.emit_text(&before, pieces);
                        self.mode = next;
                        if next == Mode::Tool {
                            self.out.tool_raw.push_str(TOOL_OPEN);
                        }
                    } else {
                        let cut = self.pending.len()
                            - partial_suffix(&self.pending, &[THINK_OPEN, TOOL_OPEN]);
                        let head: String = self.pending[..cut].to_owned();
                        self.pending.drain(..cut);
                        self.emit_text(&head, pieces);
                        return;
                    }
                }
            }
        }
    }

    /// Flushes held-back text and returns the final split.
    pub(crate) fn finish(mut self, pieces: &mut Vec<Piece>) -> SplitOutput {
        let rest = std::mem::take(&mut self.pending);
        match self.mode {
            Mode::Think => self.emit_reasoning(&rest, pieces),
            Mode::Text => self.emit_text(&rest, pieces),
            Mode::Tool => self.out.tool_raw.push_str(&rest),
        }
        let mut out = self.out;
        out.text = out.text.trim().to_owned();
        out.reasoning = out.reasoning.trim().to_owned();
        out
    }
}

/// Reassembles UTF-8 text from byte pieces that may split a character.
#[derive(Debug, Default)]
pub(crate) struct Utf8Stream {
    buf: Vec<u8>,
}

impl Utf8Stream {
    /// Appends `bytes` and returns the longest decodable prefix.
    pub(crate) fn push(&mut self, bytes: &[u8]) -> String {
        self.buf.extend_from_slice(bytes);
        let mut out = String::new();
        loop {
            match std::str::from_utf8(&self.buf) {
                Ok(s) => {
                    out.push_str(s);
                    self.buf.clear();
                    return out;
                }
                Err(e) => {
                    let valid = e.valid_up_to();
                    if let Ok(s) = std::str::from_utf8(&self.buf[..valid]) {
                        out.push_str(s);
                    }
                    match e.error_len() {
                        Some(bad) => {
                            out.push('\u{FFFD}');
                            self.buf.drain(..valid + bad);
                        }
                        None => {
                            self.buf.drain(..valid);
                            return out;
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(start_in_think: bool, chunks: &[&str]) -> (Vec<Piece>, SplitOutput) {
        let mut splitter = OutputSplitter::new(start_in_think);
        let mut pieces = Vec::new();
        for chunk in chunks {
            splitter.push(chunk, &mut pieces);
        }
        let out = splitter.finish(&mut pieces);
        (pieces, out)
    }

    #[test]
    fn splits_reasoning_text_and_tool_markup() {
        let (_, out) = run(
            true,
            &["plan it</th", "ink>\n\nHello ", "world<tool_", "call>\n{\"name\":\"x\"}"],
        );
        assert_eq!(out.reasoning, "plan it");
        assert_eq!(out.text, "Hello world");
        assert_eq!(out.tool_raw, "<tool_call>\n{\"name\":\"x\"}");
    }

    #[test]
    fn plain_text_passes_through() {
        let (pieces, out) = run(false, &["a < b ", "and c"]);
        assert_eq!(out.text, "a < b and c");
        assert!(pieces.iter().all(|p| matches!(p, Piece::Text(_))));
    }

    #[test]
    fn utf8_split_across_pieces() {
        let mut stream = Utf8Stream::default();
        let bytes = "\u{e4}".as_bytes();
        assert_eq!(stream.push(&bytes[..1]), "");
        assert_eq!(stream.push(&bytes[1..]), "\u{e4}");
    }
}
