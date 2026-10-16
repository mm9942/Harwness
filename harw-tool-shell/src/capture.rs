//! Gekappte Ausgabe-Erfassung von stdout/stderr (W1-03, F-060).
//!
//! Das Lesen der Pipes und die Kappung übernimmt die Job-Runtime
//! (`harw-command`, ein gemeinsames Byte-Budget für beide Ströme). Hier bleibt
//! der Wert, den die Formatierer der Werkzeuge (`shell.exec`, `latex.*`)
//! nutzen: die Teilausgabe samt der Angabe, ob das Budget gesprengt wurde.

/// Ausgabe von stdout/stderr mit einem gemeinsamen Byte-Budget.
#[derive(Debug)]
pub(crate) struct BoundedCapture {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    /// Anzahl insgesamt behaltener Bytes, bei der die Kappung greift:
    /// `limit + 1`, damit „mehr als `limit`“ erkennbar ist.
    retain: usize,
}

impl BoundedCapture {
    /// Leeres Capture mit gemeinsamem Budget `limit`.
    pub(crate) fn new(limit: usize) -> Self {
        Self::from_parts(Vec::new(), Vec::new(), limit, false)
    }

    /// Capture einer Ausgabe, die die Runtime unter dem Budget `limit`
    /// gesammelt hat; `exceeded` markiert einen am Budget abgebrochenen Lauf.
    pub(crate) fn from_parts(
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        limit: usize,
        exceeded: bool,
    ) -> Self {
        let mut capture = Self {
            stdout,
            stderr,
            retain: limit.saturating_add(1),
        };
        if exceeded && !capture.limit_exceeded() {
            // Die Runtime stoppt exakt am Budget; den Überlauf kenntlich machen.
            capture.retain = capture.retained();
        }
        capture
    }

    pub(crate) fn stdout(&self) -> &[u8] {
        &self.stdout
    }

    pub(crate) fn stderr(&self) -> &[u8] {
        &self.stderr
    }

    /// Insgesamt behaltene Bytes.
    pub(crate) fn retained(&self) -> usize {
        self.stdout.len() + self.stderr.len()
    }

    /// `true`, sobald mehr als `limit` Bytes gelesen wurden.
    pub(crate) fn limit_exceeded(&self) -> bool {
        self.retained() >= self.retain
    }
}

#[cfg(test)]
mod tests {
    use super::BoundedCapture;

    #[test]
    fn output_within_the_budget_is_not_exceeded() {
        let capture = BoundedCapture::from_parts(b"ab".to_vec(), b"c".to_vec(), 16, false);
        assert!(!capture.limit_exceeded());
        assert_eq!(capture.retained(), 3);
    }

    #[test]
    fn a_run_cut_at_the_budget_is_marked_exceeded() {
        let capture = BoundedCapture::from_parts(b"abcd".to_vec(), Vec::new(), 4, true);
        assert!(capture.limit_exceeded());
        assert_eq!(capture.stdout(), b"abcd");
    }

    #[test]
    fn an_empty_capture_is_not_exceeded() {
        assert!(!BoundedCapture::new(0).limit_exceeded());
    }
}
