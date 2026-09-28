//! Logdateien: inkrementelles Mitlesen ([`LogFollower`]) für die
//! Überwachung und begrenzte Abfragen ([`read_log`]) für `job.logs`.
//!
//! # Zeilenbegriff
//! Eine Zeile endet mit `\n`; eine unvollständige letzte Zeile zählt bei
//! [`read_log`] als Zeile (Ausgabe „in Arbeit"), beim Mitlesen erst, wenn
//! ihr Zeilenende da ist (oder der Prozess endet). Zeilennummern beginnen
//! bei 1. Für die Fortschrittserkennung zerlegt der Aufrufer eine Zeile
//! zusätzlich an `\r` (Fortschrittsbalken schreiben mit `\r` über).
//!
//! Jobs laufen ohne `RLIMIT_FSIZE`; eine Zeile ohne `\n` kann daher beliebig
//! lang werden (binäre Ausgabe, ein Fortschrittsbalken nur mit `\r`). Damit
//! [`read_log`] dabei nicht die ganze Zeile in einem Vec puffert, begrenzt
//! es Rohbytes je Zeile schon beim Lesen (nicht erst bei der Ausgabe) auf
//! wenige Kilobyte (`MAX_RAW_LINE_BYTES`); der Rest bis zum nächsten `\n`
//! wird verworfen, ohne gepuffert zu werden. Ein `grep`-Treffer hinter
//! dieser Grenze wird dadurch auf einer solchen Zeile nicht gefunden.
//!
//! # Nebenläufigkeit
//! Blockierendes `std::fs`; [`read_log`] läuft im Aufrufer über
//! `spawn_blocking`, [`LogFollower::read_new`] liest je Aufruf höchstens
//! [`FOLLOW_CHUNK_BYTES`].

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Höchstens so viele Bytes liest [`LogFollower::read_new`] je Aufruf.
pub const FOLLOW_CHUNK_BYTES: usize = 1024 * 1024;
/// Eine unvollständige Zeile, die länger wird, gilt ab hier als Zeile
/// (nur für die Erkennung, nicht für die Zählung).
const MAX_PARTIAL_BYTES: usize = 64 * 1024;
/// Höchstlänge einer einzelnen zurückgegebenen Zeile (Zeichen).
pub const MAX_LINE_CHARS: usize = 1000;
/// Höchstzahl Rohbytes, die [`read_log`] je Zeile puffert (4 Byte je
/// UTF-8-Zeichen reichen für [`MAX_LINE_CHARS`] auch im ungünstigsten Fall).
/// Begrenzt das Lesen selbst, nicht erst die Ausgabe, damit eine Zeile ohne
/// `\n` keine unbeschränkte Allokation auslöst.
const MAX_RAW_LINE_BYTES: usize = MAX_LINE_CHARS * 4;

/// Kürzt eine Zeile auf `max` Zeichen (mit `…`).
#[must_use]
pub fn clip_line(line: &str, max: usize) -> String {
    if line.chars().count() <= max {
        return line.to_owned();
    }
    let mut out: String = line.chars().take(max).collect();
    out.push('…');
    out
}

/// Liest neu angehängte Zeilen einer wachsenden Datei.
#[derive(Debug)]
pub struct LogFollower {
    path: PathBuf,
    file: Option<File>,
    offset: u64,
    partial: Vec<u8>,
    /// Vollständige Zeilen bisher.
    complete_lines: u64,
}

/// Ergebnis eines [`LogFollower::read_new`]-Aufrufs.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct FollowChunk {
    /// Neue Zeilen (ohne Zeilenende, verlustbehaftet nach UTF-8).
    pub lines: Vec<String>,
    /// Ob noch ungelesene Bytes übrig sein könnten.
    pub more: bool,
}

impl LogFollower {
    /// Beginnt am Dateianfang; die Datei darf noch fehlen.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            file: None,
            offset: 0,
            partial: Vec::new(),
            complete_lines: 0,
        }
    }

    /// Zahl vollständiger Zeilen bisher.
    #[must_use]
    pub fn complete_lines(&self) -> u64 {
        self.complete_lines
    }

    /// Liest höchstens [`FOLLOW_CHUNK_BYTES`] neue Bytes und liefert die
    /// darin abgeschlossenen Zeilen. Mit `flush_partial` wird auch eine
    /// unvollständige letzte Zeile ausgegeben (Prozessende).
    ///
    /// # Errors
    /// I/O-Fehler beim Öffnen/Lesen (eine fehlende Datei ist kein Fehler).
    pub fn read_new(&mut self, flush_partial: bool) -> io::Result<FollowChunk> {
        let mut chunk = FollowChunk::default();
        if self.file.is_none() {
            match File::open(&self.path) {
                Ok(file) => self.file = Some(file),
                Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(chunk),
                Err(err) => return Err(err),
            }
        }
        let Some(file) = self.file.as_mut() else {
            return Ok(chunk);
        };
        file.seek(SeekFrom::Start(self.offset))?;
        let mut buffer = Vec::new();
        let limit = u64::try_from(FOLLOW_CHUNK_BYTES).unwrap_or(u64::MAX);
        let read = file.by_ref().take(limit).read_to_end(&mut buffer)?;
        self.offset = self
            .offset
            .saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        chunk.more = read == FOLLOW_CHUNK_BYTES;

        for byte in buffer {
            if byte == b'\n' {
                let line = std::mem::take(&mut self.partial);
                self.complete_lines = self.complete_lines.saturating_add(1);
                chunk
                    .lines
                    .push(String::from_utf8_lossy(&line).into_owned());
            } else {
                self.partial.push(byte);
                if self.partial.len() >= MAX_PARTIAL_BYTES {
                    // Sehr lange Zeile ohne Ende: für die Erkennung ausgeben,
                    // aber nicht als Zeile zählen.
                    let line = std::mem::take(&mut self.partial);
                    chunk
                        .lines
                        .push(String::from_utf8_lossy(&line).into_owned());
                }
            }
        }
        if flush_partial && !self.partial.is_empty() {
            let line = std::mem::take(&mut self.partial);
            chunk
                .lines
                .push(String::from_utf8_lossy(&line).into_owned());
        }
        Ok(chunk)
    }
}

/// Die letzten `count` Zeilen einer Datei, ohne sie ganz zu lesen (höchstens
/// die letzten 64 KiB). Eine fehlende oder unlesbare Datei ist leer.
#[must_use]
pub fn tail_of_file(path: &Path, count: usize) -> Vec<String> {
    const WINDOW: u64 = 64 * 1024;
    let Ok(mut file) = File::open(path) else {
        return Vec::new();
    };
    let Ok(len) = file.metadata().map(|meta| meta.len()) else {
        return Vec::new();
    };
    let start = len.saturating_sub(WINDOW);
    let mut buffer = Vec::new();
    if file.seek(SeekFrom::Start(start)).is_err() || file.read_to_end(&mut buffer).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&buffer);
    let mut lines: Vec<&str> = text.lines().collect();
    if start > 0 && !lines.is_empty() {
        // Die erste Zeile ist angeschnitten.
        lines.remove(0);
    }
    let skip = lines.len().saturating_sub(count);
    lines
        .into_iter()
        .skip(skip)
        .map(|line| clip_line(line, MAX_LINE_CHARS))
        .collect()
}

/// Abfrage für [`read_log`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogQuery {
    /// Nur die letzten `tail` (passenden) Zeilen.
    pub tail: Option<usize>,
    /// Nur Zeilen ab dieser Nummer (1-basiert).
    pub since_line: Option<u64>,
    /// Nur Zeilen, die diesen Text enthalten (einfacher Teilstring). Bei
    /// einer sehr langen Zeile ohne `\n` wird nur ihr Anfang durchsucht
    /// (siehe Modul-Doku).
    pub grep: Option<String>,
    /// Höchstzahl zurückgegebener Zeilen.
    pub max_lines: usize,
    /// Höchstzahl zurückgegebener Bytes (Summe der Zeilen).
    pub max_bytes: usize,
}

/// Ergebnis von [`read_log`].
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct LogSlice {
    /// `(Zeilennummer, Text)`.
    pub lines: Vec<(u64, String)>,
    /// Zeilen der Datei insgesamt (inkl. unvollständiger letzter Zeile).
    pub total_lines: u64,
    /// Passende Zeilen insgesamt (nach `since_line`/`grep`).
    pub matched_lines: u64,
    /// Es gab mehr passende Zeilen als zurückgegeben.
    pub truncated: bool,
}

/// Liest eine Zeile in `raw` (ohne Zeilenende), begrenzt auf höchstens
/// [`MAX_RAW_LINE_BYTES`]. Eine überlange Zeile wird bis zum nächsten `\n`
/// verworfen, statt sie zu puffern; `raw` wächst dadurch nie über die
/// Grenze hinaus. Liefert `false` bei EOF ohne gelesene Bytes (wie
/// `BufRead::read_until() == 0`), sonst `true` (auch für eine unvollständige
/// letzte Zeile ohne `\n`).
fn read_bounded_line(reader: &mut impl BufRead, raw: &mut Vec<u8>) -> io::Result<bool> {
    raw.clear();
    let mut saw_byte = false;
    loop {
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            break;
        }
        saw_byte = true;
        let newline_at = buf.iter().position(|&byte| byte == b'\n');
        let scan_len = newline_at.unwrap_or(buf.len());
        let take = scan_len.min(MAX_RAW_LINE_BYTES.saturating_sub(raw.len()));
        raw.extend_from_slice(&buf[..take]);
        let consumed = newline_at.map_or(buf.len(), |pos| pos + 1);
        reader.consume(consumed);
        if newline_at.is_some() {
            break;
        }
    }
    Ok(saw_byte)
}

/// Liest eine Logdatei gemäß `query`.
///
/// # Description
/// Ohne `tail` gewinnen die **ersten** passenden Zeilen ab `since_line`
/// (bis `max_lines`/`max_bytes`); mit `tail` die **letzten** `tail`
/// passenden Zeilen (ebenfalls gedeckelt). Eine fehlende Datei ist leer.
///
/// # Errors
/// I/O-Fehler beim Lesen.
pub fn read_log(path: &Path, query: &LogQuery) -> io::Result<LogSlice> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(LogSlice::default()),
        Err(err) => return Err(err),
    };
    let mut reader = BufReader::new(file);
    let since = query.since_line.unwrap_or(1).max(1);
    let max_lines = query.max_lines.max(1);
    let keep_tail = query.tail.map(|tail| tail.clamp(1, max_lines));

    let mut slice = LogSlice::default();
    let mut window: VecDeque<(u64, String)> = VecDeque::new();
    let mut raw = Vec::new();
    let mut number = 0u64;
    loop {
        if !read_bounded_line(&mut reader, &mut raw)? {
            break;
        }
        number += 1;
        if number < since {
            continue;
        }
        let text = String::from_utf8_lossy(&raw);
        if let Some(needle) = &query.grep {
            if !text.contains(needle.as_str()) {
                continue;
            }
        }
        slice.matched_lines += 1;
        match keep_tail {
            Some(tail) => {
                window.push_back((number, clip_line(&text, MAX_LINE_CHARS)));
                if window.len() > tail {
                    window.pop_front();
                }
            }
            None => {
                if window.len() < max_lines {
                    window.push_back((number, clip_line(&text, MAX_LINE_CHARS)));
                }
            }
        }
    }
    slice.total_lines = number;

    // Byte-Budget: bei `tail` von hinten, sonst von vorne.
    let mut used = 0usize;
    let mut kept: VecDeque<(u64, String)> = VecDeque::new();
    if keep_tail.is_some() {
        while let Some(entry) = window.pop_back() {
            used += entry.1.len() + 12;
            if used > query.max_bytes && !kept.is_empty() {
                break;
            }
            kept.push_front(entry);
        }
    } else {
        while let Some(entry) = window.pop_front() {
            used += entry.1.len() + 12;
            if used > query.max_bytes && !kept.is_empty() {
                break;
            }
            kept.push_back(entry);
        }
    }
    slice.truncated = u64::try_from(kept.len()).unwrap_or(u64::MAX) < slice.matched_lines;
    slice.lines = kept.into_iter().collect();
    Ok(slice)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};
    use std::io::{Cursor, Write};

    fn write_lines(path: &Path, lines: &[&str]) -> TestResult {
        let mut file = File::create(path).map_err(ctx("create log"))?;
        for line in lines {
            writeln!(file, "{line}").map_err(ctx("write log"))?;
        }
        Ok(())
    }

    fn query() -> LogQuery {
        LogQuery {
            max_lines: 100,
            max_bytes: 64 * 1024,
            ..LogQuery::default()
        }
    }

    #[test]
    fn test_read_log_tail_since_grep() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("out.log");
        write_lines(&path, &["a1", "b2", "a3", "b4", "a5"])?;

        let all = read_log(&path, &query()).map_err(ctx("read all"))?;
        assert_eq!(all.total_lines, 5);
        assert_eq!(all.lines.len(), 5);
        assert!(!all.truncated);

        let tail = read_log(
            &path,
            &LogQuery {
                tail: Some(2),
                ..query()
            },
        )
        .map_err(ctx("tail"))?;
        assert_eq!(tail.lines, vec![(4, "b4".to_owned()), (5, "a5".to_owned())]);
        assert!(tail.truncated);

        let since = read_log(
            &path,
            &LogQuery {
                since_line: Some(4),
                ..query()
            },
        )
        .map_err(ctx("since"))?;
        assert_eq!(
            since.lines,
            vec![(4, "b4".to_owned()), (5, "a5".to_owned())]
        );

        let grep = read_log(
            &path,
            &LogQuery {
                grep: Some("a".into()),
                tail: Some(2),
                ..query()
            },
        )
        .map_err(ctx("grep"))?;
        assert_eq!(grep.lines, vec![(3, "a3".to_owned()), (5, "a5".to_owned())]);
        assert_eq!(grep.matched_lines, 3);

        let capped = read_log(
            &path,
            &LogQuery {
                max_lines: 2,
                ..query()
            },
        )
        .map_err(ctx("cap"))?;
        assert_eq!(capped.lines.len(), 2);
        assert!(capped.truncated);

        let missing = read_log(&dir.path().join("nope.log"), &query()).map_err(ctx("missing"))?;
        assert_eq!(missing, LogSlice::default());
        Ok(())
    }

    #[test]
    fn test_read_bounded_line_caps_overlong_lines() -> TestResult {
        // Baut eine überlange Zeile MIT `\n`, die weit über
        // `MAX_RAW_LINE_BYTES` hinausgeht, gefolgt von einer kurzen Zeile
        // MIT `\n` und einer ebenso überlangen letzten Zeile OHNE
        // Zeilenende (wie ein Job ohne FSIZE-Limit sie am Ende ohne `\n`
        // hinterlassen könnte).
        let mut data = vec![b'a'; MAX_RAW_LINE_BYTES * 5];
        data.push(b'\n');
        data.extend_from_slice(b"short\n");
        data.resize(data.len() + MAX_RAW_LINE_BYTES * 3, b'b');

        // Kleine Puffergröße erzwingt mehrere fill_buf/consume-Runden wie
        // bei einer echten Datei mit Default-Puffer.
        let mut reader = BufReader::with_capacity(4096, Cursor::new(data));
        let mut raw = Vec::new();

        assert!(read_bounded_line(&mut reader, &mut raw).map_err(ctx("line 1"))?);
        assert_eq!(raw.len(), MAX_RAW_LINE_BYTES);
        assert!(raw.iter().all(|&byte| byte == b'a'));
        assert!(
            raw.capacity() < 1024 * 1024,
            "Puffer darf nicht auf Dateigröße wachsen: {}",
            raw.capacity()
        );

        assert!(read_bounded_line(&mut reader, &mut raw).map_err(ctx("line 2"))?);
        assert_eq!(raw, b"short");

        // Letzte Zeile: läuft bis EOF ohne `\n`, gilt trotzdem als Zeile.
        assert!(read_bounded_line(&mut reader, &mut raw).map_err(ctx("line 3"))?);
        assert_eq!(raw.len(), MAX_RAW_LINE_BYTES);
        assert!(raw.iter().all(|&byte| byte == b'b'));

        assert!(!read_bounded_line(&mut reader, &mut raw).map_err(ctx("eof"))?);
        Ok(())
    }

    #[test]
    fn test_read_log_bounds_overlong_line_without_newline() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("stdout.log");
        // Simuliert Job-Ausgabe ohne Zeilenende (binäre Daten, ein
        // Fortschrittsbalken nur mit `\r`); Jobs laufen ohne FSIZE-Limit, die
        // Zeile kann also beliebig lang werden. 20 KiB liegen weit über
        // `MAX_RAW_LINE_BYTES` und reichen, um die Grenze zu prüfen, ohne die
        // Testsuite mit einer echten mehrstelligen-MiB-Datei zu belasten.
        let mut body = String::from("near-start-needle");
        body.push_str(&"x".repeat(20_000));
        body.push_str("late-needle");
        let mut file = File::create(&path).map_err(ctx("create"))?;
        file.write_all(body.as_bytes()).map_err(ctx("write"))?;

        let tail = read_log(
            &path,
            &LogQuery {
                tail: Some(5),
                ..query()
            },
        )
        .map_err(ctx("tail"))?;
        assert_eq!(tail.total_lines, 1);
        assert_eq!(tail.lines.len(), 1);
        let (number, text) = &tail.lines[0];
        assert_eq!(*number, 1);
        // Geklemmt auf MAX_LINE_CHARS Zeichen plus die angehängte Ellipse.
        assert_eq!(text.chars().count(), MAX_LINE_CHARS + 1);
        assert!(text.starts_with("near-start-needle"));

        // Ein Treffer am Zeilenanfang bleibt auffindbar …
        let grep_near = read_log(
            &path,
            &LogQuery {
                grep: Some("near-start-needle".into()),
                tail: Some(5),
                ..query()
            },
        )
        .map_err(ctx("grep near"))?;
        assert_eq!(grep_near.matched_lines, 1);

        // … ein Treffer weit hinter der Lesegrenze nicht mehr (dokumentierte
        // Nebenwirkung der Speicherbegrenzung, siehe Modul-Doku).
        let grep_late = read_log(
            &path,
            &LogQuery {
                grep: Some("late-needle".into()),
                tail: Some(5),
                ..query()
            },
        )
        .map_err(ctx("grep late"))?;
        assert_eq!(grep_late.matched_lines, 0);
        Ok(())
    }

    #[test]
    fn test_follower_reads_incrementally_and_flushes_partial() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("out.log");
        let mut follower = LogFollower::new(&path);
        assert_eq!(
            follower.read_new(false).map_err(ctx("absent"))?,
            FollowChunk::default()
        );

        let mut file = File::create(&path).map_err(ctx("create"))?;
        file.write_all(b"one\ntw").map_err(ctx("write"))?;
        let chunk = follower.read_new(false).map_err(ctx("read 1"))?;
        assert_eq!(chunk.lines, vec!["one".to_owned()]);
        assert_eq!(follower.complete_lines(), 1);

        file.write_all(b"o\nthree").map_err(ctx("write 2"))?;
        let chunk = follower.read_new(false).map_err(ctx("read 2"))?;
        assert_eq!(chunk.lines, vec!["two".to_owned()]);
        let chunk = follower.read_new(true).map_err(ctx("read 3"))?;
        assert_eq!(chunk.lines, vec!["three".to_owned()]);
        assert_eq!(follower.complete_lines(), 2);
        Ok(())
    }

    #[test]
    fn test_tail_of_file() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("out.log");
        write_lines(&path, &["1", "2", "3"])?;
        assert_eq!(tail_of_file(&path, 2), vec!["2".to_owned(), "3".to_owned()]);
        assert!(tail_of_file(&dir.path().join("missing"), 2).is_empty());
        Ok(())
    }

    #[test]
    fn test_clip_line() {
        assert_eq!(clip_line("abc", 5), "abc");
        assert_eq!(clip_line("abcdef", 3), "abc…");
    }
}
