//! Gekappte, streamende Erfassung von stdout/stderr (W1-03, F-060).
//!
//! Früher sammelte `wait_with_output` beide Pipes vollständig im Speicher und
//! kürzte erst danach; `yes` oder `cat /dev/zero` brachten so den Harness in
//! Sekunden zum OOM. [`BoundedCapture`] liest beide Pipes gleichzeitig in feste
//! Puffer und hört auf, sobald zusammen **mehr** als das Ausgabebudget vorliegt.
//! Der Speicherbedarf ist damit durch `limit + 1 + 2 * READ_CHUNK_BYTES`
//! beschränkt, unabhängig davon, wie viel das Kind schreibt.
//!
//! Die gelesenen Bytes liegen im `BoundedCapture` selbst, nicht im Future von
//! [`BoundedCapture::drain`]. Wird `drain` durch ein Timeout abgebrochen, bleibt
//! die bis dahin gelesene Teilausgabe erhalten (`AsyncReadExt::read` ist
//! cancel-safe: ein abgebrochener Lesevorgang hat keine Daten verbraucht).

use std::io;

use tokio::io::{AsyncRead, AsyncReadExt};

/// Größe eines einzelnen Lesepuffers je Pipe.
pub(crate) const READ_CHUNK_BYTES: usize = 8 * 1024;

/// Warum [`BoundedCapture::drain`] zurückgekehrt ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DrainEnd {
    /// Beide Pipes haben EOF geliefert; die Ausgabe passt ins Budget.
    Eof,
    /// Die Ausgabe hat das Budget überschritten; der Aufrufer muss das Kind
    /// beenden, weil niemand mehr liest.
    LimitExceeded,
}

/// Puffer für stdout/stderr mit einem gemeinsamen Byte-Budget.
#[derive(Debug)]
pub(crate) struct BoundedCapture {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    /// Anzahl insgesamt behaltener Bytes, bei der die Kappung greift:
    /// `limit + 1`, damit „mehr als `limit`“ erkennbar ist.
    retain: usize,
}

#[derive(Clone, Copy)]
enum Stream {
    Stdout,
    Stderr,
}

impl BoundedCapture {
    /// Neues Capture mit gemeinsamem Budget `limit` für stdout und stderr.
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            stdout: Vec::new(),
            stderr: Vec::new(),
            retain: limit.saturating_add(1),
        }
    }

    pub(crate) fn stdout(&self) -> &[u8] {
        &self.stdout
    }

    pub(crate) fn stderr(&self) -> &[u8] {
        &self.stderr
    }

    /// Insgesamt behaltene Bytes (höchstens `limit + 1`).
    pub(crate) fn retained(&self) -> usize {
        self.stdout.len() + self.stderr.len()
    }

    /// `true`, sobald mehr als `limit` Bytes gelesen wurden.
    pub(crate) fn limit_exceeded(&self) -> bool {
        self.retained() >= self.retain
    }

    fn push(&mut self, stream: Stream, bytes: &[u8]) {
        let room = self.retain.saturating_sub(self.retained());
        let kept = &bytes[..bytes.len().min(room)];
        match stream {
            Stream::Stdout => self.stdout.extend_from_slice(kept),
            Stream::Stderr => self.stderr.extend_from_slice(kept),
        }
    }

    /// Liest beide Pipes gleichzeitig, bis beide EOF liefern oder das Budget
    /// überschritten ist. Eine Pipe, die offen bleibt, aber nichts schreibt,
    /// blockiert die Kappung der anderen nicht.
    ///
    /// # Errors
    /// I/O-Fehler einer der beiden Pipes.
    pub(crate) async fn drain<O, E>(
        &mut self,
        stdout: &mut O,
        stderr: &mut E,
    ) -> io::Result<DrainEnd>
    where
        O: AsyncRead + Unpin,
        E: AsyncRead + Unpin,
    {
        let mut stdout_buf = [0_u8; READ_CHUNK_BYTES];
        let mut stderr_buf = [0_u8; READ_CHUNK_BYTES];
        let mut stdout_open = true;
        let mut stderr_open = true;

        loop {
            if self.limit_exceeded() {
                return Ok(DrainEnd::LimitExceeded);
            }
            if !stdout_open && !stderr_open {
                return Ok(DrainEnd::Eof);
            }
            // Zwischen abgeschlossenem `read` und `push` liegt kein `.await`;
            // ein Abbruch von außen verliert daher keine gelesenen Bytes.
            tokio::select! {
                read = stdout.read(&mut stdout_buf), if stdout_open => {
                    let count = read?;
                    if count == 0 {
                        stdout_open = false;
                    } else {
                        self.push(Stream::Stdout, &stdout_buf[..count]);
                    }
                }
                read = stderr.read(&mut stderr_buf), if stderr_open => {
                    let count = read?;
                    if count == 0 {
                        stderr_open = false;
                    } else {
                        self.push(Stream::Stderr, &stderr_buf[..count]);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};
    use std::time::Duration;
    use tokio::io::{AsyncWriteExt, DuplexStream, duplex};

    /// Schreibt endlos, bis die Gegenseite geschlossen wird (dann `BrokenPipe`).
    async fn flood(mut writer: DuplexStream, byte: u8) -> usize {
        let chunk = [byte; 4096];
        let mut written = 0;
        while writer.write_all(&chunk).await.is_ok() {
            written += chunk.len();
        }
        written
    }

    #[tokio::test]
    async fn endless_stdout_is_capped_without_eof() -> TestResult {
        let (stdout_writer, mut stdout_reader) = duplex(64 * 1024);
        // stderr bleibt offen und schweigt: Kappung darf nicht auf EOF warten.
        let (_stderr_writer, mut stderr_reader) = duplex(64 * 1024);
        let writer = tokio::spawn(flood(stdout_writer, b'x'));

        let mut capture = BoundedCapture::new(1000);
        let end = tokio::time::timeout(
            Duration::from_secs(10),
            capture.drain(&mut stdout_reader, &mut stderr_reader),
        )
        .await
        .map_err(ctx("capping must not wait for the writer"))?
        .map_err(ctx("duplex read"))?;

        assert_eq!(end, DrainEnd::LimitExceeded);
        assert_eq!(capture.stdout().len(), 1001);
        assert!(capture.stdout().iter().all(|byte| *byte == b'x'));
        assert!(capture.stderr().is_empty());
        assert!(capture.limit_exceeded());

        drop(stdout_reader);
        let written = writer.await.map_err(ctx("writer task"))?;
        assert!(
            written >= 1001,
            "writer must have produced more than the cap"
        );
        Ok(())
    }

    #[tokio::test]
    async fn stderr_flood_is_capped_while_stdout_stays_silent() -> TestResult {
        let (_stdout_writer, mut stdout_reader) = duplex(64 * 1024);
        let (stderr_writer, mut stderr_reader) = duplex(64 * 1024);
        let writer = tokio::spawn(flood(stderr_writer, b'e'));

        let mut capture = BoundedCapture::new(10_000);
        let end = capture
            .drain(&mut stdout_reader, &mut stderr_reader)
            .await
            .map_err(ctx("duplex read"))?;

        assert_eq!(end, DrainEnd::LimitExceeded);
        assert_eq!(capture.retained(), 10_001);
        assert_eq!(capture.stderr().len(), 10_001);
        drop(stderr_reader);
        writer.await.map_err(ctx("writer task"))?;
        Ok(())
    }

    #[tokio::test]
    async fn stdout_and_stderr_share_one_budget() -> TestResult {
        let (mut stdout_writer, mut stdout_reader) = duplex(64 * 1024);
        let (mut stderr_writer, mut stderr_reader) = duplex(64 * 1024);
        stdout_writer
            .write_all(&[b'o'; 600])
            .await
            .map_err(ctx("write stdout"))?;
        stderr_writer
            .write_all(&[b'e'; 600])
            .await
            .map_err(ctx("write stderr"))?;
        drop(stdout_writer);
        drop(stderr_writer);

        let mut capture = BoundedCapture::new(1000);
        let end = capture
            .drain(&mut stdout_reader, &mut stderr_reader)
            .await
            .map_err(ctx("duplex read"))?;

        assert_eq!(end, DrainEnd::LimitExceeded);
        assert_eq!(capture.retained(), 1001);
        Ok(())
    }

    #[tokio::test]
    async fn output_within_budget_reaches_eof_unchanged() -> TestResult {
        let (mut stdout_writer, mut stdout_reader) = duplex(1024);
        let (mut stderr_writer, mut stderr_reader) = duplex(1024);
        stdout_writer
            .write_all(b"hello")
            .await
            .map_err(ctx("write stdout"))?;
        stderr_writer
            .write_all(b"warn")
            .await
            .map_err(ctx("write stderr"))?;
        drop(stdout_writer);
        drop(stderr_writer);

        let mut capture = BoundedCapture::new(9);
        let end = capture
            .drain(&mut stdout_reader, &mut stderr_reader)
            .await
            .map_err(ctx("duplex read"))?;

        assert_eq!(end, DrainEnd::Eof);
        assert_eq!(capture.stdout(), b"hello");
        assert_eq!(capture.stderr(), b"warn");
        assert!(
            !capture.limit_exceeded(),
            "exactly the limit is not an overflow"
        );
        Ok(())
    }

    #[tokio::test]
    async fn zero_budget_caps_on_first_byte() -> TestResult {
        let (mut stdout_writer, mut stdout_reader) = duplex(1024);
        let (_stderr_writer, mut stderr_reader) = duplex(1024);
        stdout_writer
            .write_all(b"ab")
            .await
            .map_err(ctx("write stdout"))?;

        let mut capture = BoundedCapture::new(0);
        let end = capture
            .drain(&mut stdout_reader, &mut stderr_reader)
            .await
            .map_err(ctx("duplex read"))?;

        assert_eq!(end, DrainEnd::LimitExceeded);
        assert_eq!(capture.stdout(), b"a");
        Ok(())
    }

    #[tokio::test]
    async fn partial_output_survives_cancelled_drain() -> TestResult {
        let (mut stdout_writer, mut stdout_reader) = duplex(1024);
        let (mut stderr_writer, mut stderr_reader) = duplex(1024);
        stdout_writer
            .write_all(b"partial-out")
            .await
            .map_err(ctx("write stdout"))?;
        stderr_writer
            .write_all(b"partial-err")
            .await
            .map_err(ctx("write stderr"))?;

        let mut capture = BoundedCapture::new(64 * 1024);
        // Writer bleiben offen: `drain` endet nur durch das Timeout.
        let result = tokio::time::timeout(
            Duration::from_millis(100),
            capture.drain(&mut stdout_reader, &mut stderr_reader),
        )
        .await;

        assert!(result.is_err(), "drain must still be waiting for EOF");
        assert_eq!(capture.stdout(), b"partial-out");
        assert_eq!(capture.stderr(), b"partial-err");
        drop((stdout_writer, stderr_writer));
        Ok(())
    }
}
