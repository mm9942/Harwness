//! `fs.read` — Tool-Executor für lesenden Dateizugriff.
//!
//! # Verantwortung
//! Dieses Modul besitzt den `fs.read`-Executor:
//! - [`FsReadExecutor`]: implementiert [`ToolExecutor`]; liest eine Datei aus dem
//!   Workspace und erzwingt Permission-Check, symlinkfreies Öffnen unterhalb
//!   der Workspace-Wurzel (`open_beneath`), Typ-Check und Ausgabegrenze.
//!
//! # Drei sich ausschließende Modi (W1-03: Zeilenmodus als Standard)
//! - **Zeilenmodus** (Standard, wenn keine Byte-/Tail-Parameter gesetzt
//!   sind): `line` (1-basierte erste Zeile, Default 1) und `limit` (Anzahl
//!   Zeilen, Default 400, hart begrenzt auf 2000). Ausgabe im `cat -n`-Stil:
//!   rechtsbündige Zeilennummer, Tab, Zeileninhalt. Ein Header nennt den
//!   gelieferten Bereich und die Gesamtzeilenzahl; ein Fußzeilen-Hinweis
//!   nennt die Folgezeile, wenn mehr Inhalt folgt.
//! - **Tail-Modus** (`tail` gesetzt): liefert die letzten `tail` Zeilen mit
//!   echten Zeilennummern im selben Ausgabeformat. Ein einziger Durchlauf
//!   mit Ringpuffer ([`std::collections::VecDeque`]) hält nur die letzten
//!   `tail` Zeilen im Speicher, während bis Dateiende weitergezählt wird.
//!   `tail` schließt `line` > 1/`offset` > 0/`max_bytes` aus; ein `limit`
//!   begrenzt die Zeilenzahl zusätzlich.
//! - **Byte-Modus** (Rückfall, `offset`/`max_bytes` gesetzt): unverändertes
//!   Verhalten von vor W1-03 — Kürzung statt Ablehnung (siehe unten).
//!
//! `null` gilt überall als „nicht gesetzt“, ebenso `0` bei
//! `tail`/`limit`/`max_bytes`. Harmlose Kombinationen werden toleriert
//! (Standardwerte `line` = 1 bzw. `offset` = 0 neben einem anderen Modus;
//! `limit` neben `tail` begrenzt die Tail-Zeilen). Echte Widersprüche (z. B.
//! `line` = 50 und `offset` = 100, oder `tail` und `line` = 50) liefern
//! `Ok(ToolOutput::error(...))` mit einer Meldung, die die Felder samt Werten
//! nennt und sagt, welche auf `null` müssen — siehe `resolve_read_mode`.
//!
//! # Kürzung statt Ablehnung (W1-02, Byte-Modus)
//! Dateien über dem Byte-Limit (höchstens 64 KiB) werden gekürzt geliefert.
//! Ein Hinweis am Ende nennt den gelieferten Bereich und den `offset` zum
//! Weiterlesen. Gekürzt wird an einer UTF-8-Zeichengrenze, sofern der Schnitt
//! ein Zeichen teilen würde. Gelesen wird über den geöffneten Deskriptor mit
//! `take(limit + 1)` — eine wachsende Datei sprengt den Speicher nicht.
//!
//! Im Zeilen- und Tail-Modus gilt dieselbe Ausgabegrenze
//! ([`crate::tree::MAX_OUTPUT_BYTES`]) für den formatierten Text; wird sie
//! erreicht, endet die Ausgabe an einer Zeilengrenze mit einem
//! Fortsetzungshinweis statt einer abgeschnittenen letzten Zeile.
//!
//! # Schlüsseltypen
//! - [`FsReadExecutor`]
//!
//! # Nebenläufigkeit
//! [`FsReadExecutor`] ist `Send + Sync` und über `Arc` teilbar.
//! `fs.read` ist `parallel_safe` (Reads sind commutative). Die Datei-IO läuft
//! über [`crate::blocking::run_blocking`] im Blocking-Pool.
//!
//! # Fehler
//! Permission-Fehler werden als `ToolOutput::error(...)` + `Ok(...)` signalisiert
//! (fail-closed ohne Panic). I/O-, Pfad- und Modi-Konflikt-Fehler ebenfalls als
//! Tool-Output; Meldungen nennen nur den vom Modell übergebenen relativen Pfad.

use crate::error::FsToolError;
use crate::symlink::open_start;
use crate::tree::{MAX_LINE_BYTES, MAX_OUTPUT_BYTES, MAX_SCAN_FILE_BYTES, normalize_relative};
use harw_authority::Permission;
use harw_tools::{
    ToolCall, ToolOutput,
    error::ToolsError,
    executor::{ToolExecutionContext, ToolExecutor, ToolExecutorFuture},
};
use serde::Deserialize;
use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};

// Default-Zeilenanzahl im Zeilenmodus, wenn `limit` fehlt.
const DEFAULT_LINE_LIMIT: u64 = 400;

// Harte Obergrenze für `limit`/`tail`, unabhängig vom Modell-Wunsch.
const MAX_LINE_LIMIT: u64 = 2000;

// Anhängsel an eine über `MAX_LINE_BYTES` gekürzte Ausgabezeile.
const LINE_TRUNCATION_MARKER: &str = " … [Zeile gekürzt]";

/// Deserialisierte Argumente für `fs.read`.
#[derive(Debug, Deserialize)]
struct FsReadArgs {
    /// Pfad relativ zum Workspace-Root.
    path: String,
    /// Optionales Byte-Limit für diesen Aufruf (Byte-Modus, Rückfall).
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_u64"
    )]
    max_bytes: Option<u64>,
    /// Optionaler Byte-Offset, ab dem gelesen wird (Byte-Modus, Rückfall;
    /// Default 0). Schließt `line`/`limit`/`tail` aus.
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_u64"
    )]
    offset: Option<u64>,
    /// 1-basierte erste Zeile im Zeilenmodus (Default 1). Schließt
    /// `offset`/`max_bytes`/`tail` aus.
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_u64"
    )]
    line: Option<u64>,
    /// Anzahl Zeilen im Zeilenmodus (Default 400, hart begrenzt auf 2000).
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_u64"
    )]
    limit: Option<u64>,
    /// Anzahl der letzten Zeilen im Tail-Modus (hart begrenzt auf 2000).
    /// Schließt `line`/`limit`/`offset`/`max_bytes` aus (die Zeilenanzahl
    /// kommt aus `tail`).
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_u64"
    )]
    tail: Option<u64>,
}

/// Der aus den Argumenten bestimmte Lesemodus von `fs.read`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadMode {
    /// Zeilenmodus (Standard): `line`/`limit`.
    Line,
    /// Tail-Modus: die letzten `tail` Zeilen.
    Tail,
    /// Byte-Modus (Rückfall): `offset`/`max_bytes`.
    Byte,
}

/// Bestimmt den Lesemodus und bereinigt die Argumente dafür.
///
/// # Beschreibung
/// Im Strict-Schema sind alle Parameter `required` und nullable; Modelle
/// senden deshalb oft Werte für Felder, die sie gar nicht meinen (Bugreport
/// Runde 5: `tail` zusammen mit `line`/`limit`, oder `0` statt `null`).
/// Regeln, in dieser Reihenfolge:
/// 1. `null` ist überall „nicht gesetzt“ (erledigt schon die
///    Deserialisierung); ebenso `0` bei `tail`, `limit` und `max_bytes`,
///    die mit `0` keinen Sinn ergeben.
/// 2. Tail-Modus (`tail` gesetzt): `line` = 0/1 und `offset` = 0 sind
///    harmlose Standardwerte und fallen weg; ein zusätzliches `limit`
///    begrenzt die Zeilenzahl (`min(tail, limit)`). Ein echtes `line` > 1,
///    `offset` > 0 oder `max_bytes` widerspricht dem Tail-Modus.
/// 3. Zeilen- und Byte-Parameter zugleich: `offset` = 0 neben `line`/`limit`
///    fällt weg (Zeilenmodus), `line` = 0/1 ohne `limit` neben
///    `offset`/`max_bytes` fällt weg (Byte-Modus). Bleibt danach beides
///    übrig, ist das ein Widerspruch.
/// 4. Sonst: `offset`/`max_bytes` gesetzt → Byte-Modus (auch `offset` = 0
///    allein, wie bisher), andernfalls Zeilenmodus.
///
/// # Errors
/// Eine Meldung für das Modell, die die widersprüchlichen Felder mit ihren
/// Werten nennt und sagt, welche davon `null` sein müssen.
fn resolve_read_mode(args: &mut FsReadArgs) -> Result<ReadMode, String> {
    for field in [&mut args.tail, &mut args.limit, &mut args.max_bytes] {
        if *field == Some(0) {
            *field = None;
        }
    }

    if let Some(tail) = args.tail {
        if matches!(args.line, Some(0 | 1)) {
            args.line = None;
        }
        if args.offset == Some(0) {
            args.offset = None;
        }
        let conflicts = set_fields(&[
            ("line", args.line),
            ("offset", args.offset),
            ("max_bytes", args.max_bytes),
        ]);
        if !conflicts.is_empty() {
            let names = field_names(&conflicts);
            return Err(format!(
                "fs.read: widersprüchliche Parameter — 'tail'={tail} (letzte Zeilen) passt nicht \
                 zu {}. Für die letzten {tail} Zeilen {names} auf null setzen; für einen \
                 anderen Modus stattdessen 'tail' auf null setzen. (null = nicht gesetzt)",
                conflicts.join(", ")
            ));
        }
        if let Some(limit) = args.limit.take() {
            args.tail = Some(tail.min(limit));
        }
        return Ok(ReadMode::Tail);
    }

    let line_requested = args.line.is_some() || args.limit.is_some();
    let byte_requested = args.offset.is_some() || args.max_bytes.is_some();
    if line_requested && byte_requested {
        if args.offset == Some(0) {
            args.offset = None;
        }
        if (args.offset.is_some() || args.max_bytes.is_some())
            && args.limit.is_none()
            && matches!(args.line, Some(0 | 1))
        {
            args.line = None;
        }
    }

    let line_fields = set_fields(&[("line", args.line), ("limit", args.limit)]);
    let byte_fields = set_fields(&[("offset", args.offset), ("max_bytes", args.max_bytes)]);
    match (line_fields.is_empty(), byte_fields.is_empty()) {
        (false, false) => Err(format!(
            "fs.read: widersprüchliche Parameter — Zeilenmodus ({}) und Byte-Modus ({}) \
             zugleich gesetzt. Für den Zeilenmodus {} auf null setzen; für den Byte-Modus {} \
             auf null setzen. (null = nicht gesetzt)",
            line_fields.join(", "),
            byte_fields.join(", "),
            field_names(&byte_fields),
            field_names(&line_fields),
        )),
        (true, false) => Ok(ReadMode::Byte),
        _ => Ok(ReadMode::Line),
    }
}

/// Formatiert die gesetzten Felder als `'name'=wert`.
fn set_fields(fields: &[(&str, Option<u64>)]) -> Vec<String> {
    fields
        .iter()
        .filter_map(|(name, value)| value.map(|value| format!("'{name}'={value}")))
        .collect()
}

/// Macht aus `'name'=wert`-Einträgen die reine Namensliste `'a'/'b'`.
fn field_names(fields: &[String]) -> String {
    fields
        .iter()
        .map(|field| {
            field
                .split_once('=')
                .map_or(field.as_str(), |(name, _)| name)
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Führt `fs.read`-Aufrufe aus.
///
/// # Description
/// Liest eine Datei relativ zum Workspace-Root des Sandbox-Kontexts.
/// Prüft `ReadWorkspace`-Permission, öffnet die Datei über
/// `harw_fsutil::open_beneath` relativ zum Deskriptor der Workspace-Wurzel
/// (kein Pfadglied darf ein Symlink sein), prüft den Typ und liefert dann
/// wahlweise nummerierte Zeilen (Standard), die letzten `tail` Zeilen, oder
/// (Rückfall) höchstens `min(max_bytes, 64 KiB)` Bytes ab `offset` als
/// [`ToolOutput::Text`].
///
/// # Arguments
/// - `max_bytes` (`u64`): konfiguriertes Byte-Maximum (Provider-Level) für den
///   Byte-Modus; Werte über 64 KiB werden auf 64 KiB begrenzt.
///
/// # Concurrency
/// `Send + Sync`; über [`std::sync::Arc`] teilbar. Keine Mutation von Shared State.
///
/// # Errors
/// Permission-Fehler → `Ok(ToolOutput::error(...))`.
/// I/O/Pfad-Fehler → `Ok(ToolOutput::error(...))`.
/// Gemischte Modi-Parameter → `Ok(ToolOutput::error(...))`.
/// Ungültige JSON-Argumente → `Err(ToolsError::InvalidArguments)`.
///
/// # Examples
/// ```rust,no_run
/// use harw_tool_fs::FsReadExecutor;
/// let _executor = FsReadExecutor { max_bytes: 65_536 };
/// ```
pub struct FsReadExecutor {
    /// Maximale Bytes, die pro Aufruf im Byte-Modus gelesen werden.
    pub max_bytes: u64,
}

impl FsReadExecutor {
    /// Führt den Lese-Vorgang synchron (blockierend) aus.
    ///
    /// # Errors
    /// Gibt nur `Err` bei einem Deserialisierungs-Fehler zurück (via
    /// [`ToolsError::InvalidArguments`]). Alle anderen Fehler — inklusive
    /// gemischter Modi-Parameter — werden in `Ok(ToolOutput::error(...))`
    /// gewandelt.
    fn read_file(
        &self,
        ctx: &ToolExecutionContext,
        call: &ToolCall,
    ) -> Result<ToolOutput, ToolsError> {
        // Permission check — fail closed
        if let Some(err) =
            harw_tools::sandbox_guard::require_permission(ctx, Permission::ReadWorkspace, "fs.read")
        {
            return Ok(err);
        }

        // Parse arguments
        let mut args: FsReadArgs =
            match serde_json::from_value::<FsReadArgs>(call.arguments.clone()) {
                Ok(a) => a,
                Err(err) => {
                    return Err(ToolsError::InvalidArguments {
                        name: "fs.read".to_owned(),
                        reason: err.to_string(),
                    });
                }
            };

        // Modus bestimmen: `null`, sinnlose Nullen und harmlose Standardwerte
        // gelten als nicht gesetzt; nur echte Widersprüche werden abgelehnt
        // (siehe `resolve_read_mode`).
        let mode = match resolve_read_mode(&mut args) {
            Ok(mode) => mode,
            Err(message) => return Ok(ToolOutput::error(message)),
        };

        let relative = match normalize_relative(&args.path) {
            Ok(rel) => rel,
            Err(reason) => return Ok(ToolOutput::error(format!("fs.read: {reason}"))),
        };

        // Symlinks im Pfad: nach innen frei, nach außen nur mit Freigabe;
        // geöffnet wird danach strikt symlinkfrei (kein TOCTOU, siehe
        // `crate::symlink`). Eine Ablehnung nennt Link und Ziel.
        let opened = open_start(
            ctx.sandbox().workspace().canonical_root(),
            &relative,
            "fs.read",
            &args.path,
        )
        .and_then(|start| start.workspace.open_any(&start.rel));
        let mut file = match opened {
            Ok(file) => file,
            Err(err) => {
                return Ok(ToolOutput::error(format!(
                    "fs.read: '{}' kann nicht geöffnet werden: {err}",
                    args.path
                )));
            }
        };
        let metadata = match file.metadata() {
            Ok(m) => m,
            Err(err) => return Ok(ToolOutput::error(FsToolError::Io(err).to_string())),
        };
        if !metadata.is_file() {
            return Ok(ToolOutput::error(
                FsToolError::NotAFile {
                    path: args.path.clone(),
                }
                .to_string(),
            ));
        }

        match mode {
            ReadMode::Tail => Ok(Self::read_tail_mode(&file, &args)),
            ReadMode::Byte => Ok(self.read_byte_mode(&mut file, &args, metadata.len())),
            ReadMode::Line => Ok(Self::read_line_mode(&file, &args)),
        }
    }

    // Byte-Modus (Rückfall): unverändertes Verhalten von vor W1-03. Liefert
    // höchstens `min(max_bytes, self.max_bytes, MAX_OUTPUT_BYTES)` Bytes ab
    // `offset`; kürzt statt abzulehnen (siehe Moduldoku).
    fn read_byte_mode(&self, file: &mut File, args: &FsReadArgs, size: u64) -> ToolOutput {
        let output_cap = u64::try_from(MAX_OUTPUT_BYTES).unwrap_or(u64::MAX);
        let cap = args
            .max_bytes
            .unwrap_or(self.max_bytes)
            .min(self.max_bytes)
            .min(output_cap)
            .max(1);
        let offset = args.offset.unwrap_or(0);
        if offset > size {
            return ToolOutput::error(format!(
                "fs.read: offset {offset} liegt hinter dem Dateiende ({size} Bytes)"
            ));
        }
        if let Err(err) = file.seek(SeekFrom::Start(offset)) {
            return ToolOutput::error(FsToolError::Io(err).to_string());
        }

        // Ein Byte mehr lesen, um „es gibt weitere Bytes“ ohne `fstat`-Wettlauf
        // zu erkennen.
        let mut raw = Vec::new();
        if let Err(err) = file.take(cap.saturating_add(1)).read_to_end(&mut raw) {
            return ToolOutput::error(FsToolError::Io(err).to_string());
        }
        let cap_len = usize::try_from(cap).unwrap_or(usize::MAX);
        let truncated = raw.len() > cap_len;
        if truncated {
            raw.truncate(cap_len);
            trim_partial_utf8(&mut raw);
        }

        let mut content = String::from_utf8_lossy(&raw).into_owned();
        if truncated {
            let end = offset.saturating_add(u64::try_from(raw.len()).unwrap_or(u64::MAX));
            content.push_str(&format!(
                "\n\n[fs.read: Ausgabe gekürzt auf {} Bytes (Bytes {offset}..{end} von {size}); \
                 weiterlesen mit offset={end}]",
                raw.len()
            ));
        }
        ToolOutput::text(content)
    }

    // Zeilenmodus (Standard): `line`/`limit`, `cat -n`-Ausgabe. Liest
    // zeilenweise über `BufRead` nur bis zur letzten benötigten Zeile; zählt
    // darüber hinaus bis Dateiende (begrenzt durch `MAX_SCAN_FILE_BYTES`)
    // weiter, um die Gesamtzeilenzahl zu melden, ohne den Zeileninhalt danach
    // noch zu speichern.
    fn read_line_mode(file: &File, args: &FsReadArgs) -> ToolOutput {
        let path = args.path.as_str();
        let start_line = args.line.unwrap_or(1).max(1);
        let limit = args
            .limit
            .unwrap_or(DEFAULT_LINE_LIMIT)
            .clamp(1, MAX_LINE_LIMIT);
        let last_wanted = start_line.saturating_add(limit).saturating_sub(1);

        let mut reader = BufReader::new(file);
        let mut raw_line: Vec<u8> = Vec::new();
        let mut current_line: u64 = 0;
        let mut bytes_scanned: u64 = 0;
        let mut candidates: Vec<(u64, String)> = Vec::new();
        let mut reached_eof = false;

        loop {
            if bytes_scanned >= MAX_SCAN_FILE_BYTES {
                break;
            }
            raw_line.clear();
            let read = match reader.read_until(b'\n', &mut raw_line) {
                Ok(n) => n,
                Err(err) => return ToolOutput::error(FsToolError::Io(err).to_string()),
            };
            if read == 0 {
                reached_eof = true;
                break;
            }
            current_line += 1;
            bytes_scanned = bytes_scanned.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
            if current_line >= start_line && current_line <= last_wanted {
                candidates.push((current_line, decode_line(&raw_line)));
            }
        }

        let lines_counted = current_line;
        let total_display = format_total(lines_counted, reached_eof);

        if candidates.is_empty() {
            return ToolOutput::text(format!(
                "{path} — Datei hat nur {total_display} Zeilen (angefordert: Zeile {start_line})"
            ));
        }

        let width = digit_count(candidates.last().map_or(start_line, |(n, _)| *n));
        let mut body_lines: Vec<String> = Vec::with_capacity(candidates.len());
        let mut body_bytes: usize = 0;
        let mut included = 0usize;
        for (n, text) in &candidates {
            let formatted = format!("{n:>width$}\t{text}");
            let extra = formatted.len() + 1;
            if included > 0 && body_bytes.saturating_add(extra) > MAX_OUTPUT_BYTES {
                break;
            }
            body_bytes = body_bytes.saturating_add(extra);
            body_lines.push(formatted);
            included += 1;
        }
        let y_actual = candidates[included - 1].0;
        let more_remains = included < candidates.len() || y_actual < lines_counted || !reached_eof;
        // Nach dem Kürzen an der Ausgabegrenze die Nummernspalte an der
        // tatsächlich letzten Zeile ausrichten (nie breiter als vorher, die
        // Grenze bleibt also eingehalten).
        let final_width = digit_count(y_actual);
        let body = if final_width == width {
            body_lines.join("\n")
        } else {
            candidates[..included]
                .iter()
                .map(|(n, text)| format!("{n:>final_width$}\t{text}"))
                .collect::<Vec<_>>()
                .join("\n")
        };

        let mut content =
            format!("{path} — Zeilen {start_line}–{y_actual} von {total_display}\n{body}");
        if more_remains {
            content.push_str(&format!(
                "\n\n[fs.read: … weiter mit line={}]",
                y_actual + 1
            ));
        }
        ToolOutput::text(content)
    }

    // Tail-Modus: letzte `tail` Zeilen mit echten Zeilennummern, gleiches
    // Ausgabeformat wie der Zeilenmodus. Ein Durchlauf mit Ringpuffer
    // (`VecDeque`) hält nur die letzten `tail` Zeilen im Speicher, während
    // bis Dateiende (begrenzt durch `MAX_SCAN_FILE_BYTES`) weitergezählt
    // wird.
    fn read_tail_mode(file: &File, args: &FsReadArgs) -> ToolOutput {
        let path = args.path.as_str();
        let tail_count = args.tail.unwrap_or(1).clamp(1, MAX_LINE_LIMIT);
        let tail_capacity = usize::try_from(tail_count).unwrap_or(usize::MAX);

        let mut reader = BufReader::new(file);
        let mut raw_line: Vec<u8> = Vec::new();
        let mut current_line: u64 = 0;
        let mut bytes_scanned: u64 = 0;
        let mut ring: VecDeque<(u64, String)> = VecDeque::with_capacity(tail_capacity);
        let mut reached_eof = false;

        loop {
            if bytes_scanned >= MAX_SCAN_FILE_BYTES {
                break;
            }
            raw_line.clear();
            let read = match reader.read_until(b'\n', &mut raw_line) {
                Ok(n) => n,
                Err(err) => return ToolOutput::error(FsToolError::Io(err).to_string()),
            };
            if read == 0 {
                reached_eof = true;
                break;
            }
            current_line += 1;
            bytes_scanned = bytes_scanned.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
            if ring.len() == tail_capacity {
                ring.pop_front();
            }
            ring.push_back((current_line, decode_line(&raw_line)));
        }

        let lines_counted = current_line;
        let total_display = format_total(lines_counted, reached_eof);

        if ring.is_empty() {
            return ToolOutput::text(format!(
                "{path} — Datei hat nur {total_display} Zeilen (angefordert: die letzten \
                 {tail_count} Zeilen)"
            ));
        }

        let width = digit_count(lines_counted.max(1));
        let entries: Vec<(u64, String)> = ring.into_iter().collect();

        // Von hinten (jüngste Zeile) nach vorne auffüllen: bei erreichter
        // Ausgabegrenze bleiben die neuesten Zeilen erhalten (Tail-Semantik),
        // ältere werden verworfen statt der zuletzt gelesenen.
        let mut fitted_from_end = 0usize;
        let mut acc: usize = 0;
        for (n, text) in entries.iter().rev() {
            let formatted_len = format!("{n:>width$}\t{text}").len() + 1;
            if fitted_from_end > 0 && acc.saturating_add(formatted_len) > MAX_OUTPUT_BYTES {
                break;
            }
            acc = acc.saturating_add(formatted_len);
            fitted_from_end += 1;
        }
        let start_index = entries.len().saturating_sub(fitted_from_end);
        let included = &entries[start_index..];
        let start_line = included.first().map_or(lines_counted.max(1), |(n, _)| *n);
        let body = included
            .iter()
            .map(|(n, text)| format!("{n:>width$}\t{text}"))
            .collect::<Vec<_>>()
            .join("\n");

        ToolOutput::text(format!(
            "{path} — Zeilen {start_line}–{lines_counted} von {total_display}\n{body}"
        ))
    }
}

/// Entfernt ein am Ende abgeschnittenes, unvollständiges UTF-8-Zeichen.
///
/// Nur wenn davor gültiger Text steht; sonst bleibt der Puffer unverändert
/// (verlustbehaftete Dekodierung, garantiert Fortschritt beim Weiterlesen).
fn trim_partial_utf8(raw: &mut Vec<u8>) {
    if let Err(err) = std::str::from_utf8(raw) {
        if err.error_len().is_none() && err.valid_up_to() > 0 {
            raw.truncate(err.valid_up_to());
        }
    }
}

// Entfernt den Zeilenumbruch (`\n`, optional vorangehendes `\r`) einer über
// `BufRead::read_until` gelesenen Rohzeile und kürzt sie danach für die
// Anzeige. Das Trennen auf Byte-Ebene ist UTF-8-sicher, da `\n`/`\r` nie als
// Fortsetzungsbyte einer Mehrbyte-Sequenz vorkommen.
fn decode_line(raw: &[u8]) -> String {
    let mut bytes = raw;
    if bytes.last() == Some(&b'\n') {
        bytes = &bytes[..bytes.len() - 1];
        if bytes.last() == Some(&b'\r') {
            bytes = &bytes[..bytes.len() - 1];
        }
    }
    truncate_display_line(&String::from_utf8_lossy(bytes))
}

// Kürzt `line` UTF-8-sicher auf höchstens `MAX_LINE_BYTES` und hängt
// `LINE_TRUNCATION_MARKER` an, falls gekürzt wurde.
fn truncate_display_line(line: &str) -> String {
    if line.len() <= MAX_LINE_BYTES {
        return line.to_owned();
    }
    let mut cut = MAX_LINE_BYTES;
    while !line.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}{LINE_TRUNCATION_MARKER}", &line[..cut])
}

// Anzahl Dezimalstellen von `n` (mindestens 1) — Breite der rechtsbündigen
// Zeilennummern-Spalte.
fn digit_count(n: u64) -> usize {
    n.to_string().len()
}

// Formatiert die gemeldete Gesamtzeilenzahl: exakt, wenn das Dateiende
// erreicht wurde, sonst als Untergrenze („≥ M“), wenn `MAX_SCAN_FILE_BYTES`
// zuerst erreicht wurde (siehe Moduldoku).
fn format_total(lines_counted: u64, reached_eof: bool) -> String {
    if reached_eof {
        lines_counted.to_string()
    } else {
        format!("≥ {lines_counted}")
    }
}

impl ToolExecutor for FsReadExecutor {
    /// Führt eine `fs.read`-Invokation aus.
    ///
    /// # Arguments
    /// - `context` (`&ToolExecutionContext`): harness-etablierte Autorität mit
    ///   Sandbox-Spec (Permissions + Workspace).
    /// - `call` (`&ToolCall`): die ungeprüfte Invokation mit JSON-Argumenten.
    ///
    /// # Returns
    /// `Ok(ToolOutput::Text { content })` bei Erfolg.
    /// `Ok(ToolOutput::Error { message })` bei Permission-, Modi-Konflikt-
    /// oder I/O-Fehler.
    ///
    /// # Errors
    /// - [`ToolsError::InvalidArguments`]: fehlende oder fehlerhafte JSON-Argumente.
    ///
    /// # Concurrency
    /// Sicher für parallele Aufrufe; die Arbeit läuft im Blocking-Pool.
    fn execute<'a>(
        &'a self,
        context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        let executor = Self {
            max_bytes: self.max_bytes,
        };
        let context = context.clone();
        let call = call.clone();
        Box::pin(crate::blocking::run_blocking("fs.read", move || {
            executor.read_file(&context, &call)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Fixture, SECRET, TestError, TestResult, call, ctx, render};
    use harw_authority::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_types::{SessionId, TenantId, ToolCallId, TurnId, WorkspaceId};
    use std::fs;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    fn make_sandbox_with_permissions(
        root: &Path,
        permissions: Vec<Permission>,
    ) -> TestResult<SandboxSpec> {
        let ws_dir = root.join("ws");
        fs::create_dir_all(&ws_dir)?;
        let registry = WorkspaceRegistry::build(
            root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("t"),
                workspace: WorkspaceId::from_str("w"),
                root: PathBuf::from("ws"),
            }],
        )
        .map_err(ctx("registry"))?;
        let binding = registry
            .resolve(&TenantId::from_str("t"), &WorkspaceId::from_str("w"))
            .map_err(ctx("binding"))?;
        Ok(SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy(permissions),
        ))
    }

    fn make_ctx(sandbox: SandboxSpec) -> ToolExecutionContext {
        ToolExecutionContext::new(SessionId::new(), TurnId::new(), sandbox)
    }

    fn make_call(args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: ToolCallId::new(),
            name: harw_tools::spec::ToolName::new("fs.read"),
            arguments: args,
        }
    }

    #[test]
    fn test_fs_read_reads_file_successfully() -> TestResult {
        let dir = TempDir::new()?;
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws)?;
        fs::write(ws.join("hello.txt"), "hello world")?;

        let executor = FsReadExecutor { max_bytes: 65_536 };
        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::ReadWorkspace])?;
        let ctx = make_ctx(sandbox);
        // W1-03: der Standardmodus ist jetzt der Zeilenmodus; `offset: 0`
        // erzwingt hier explizit den (unveränderten) Byte-Modus, damit dieser
        // Test weiterhin reine Byte-Ausgabe prüft.
        let call = make_call(serde_json::json!({ "path": "hello.txt", "offset": 0 }));

        let result = executor.read_file(&ctx, &call)?;
        match result {
            ToolOutput::Text { content } => assert_eq!(content, "hello world"),
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected text output, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_fs_read_denied_when_no_read_permission() -> TestResult {
        let dir = TempDir::new()?;
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws)?;

        let executor = FsReadExecutor { max_bytes: 65_536 };
        let sandbox = make_sandbox_with_permissions(dir.path(), vec![])?;
        let ctx = make_ctx(sandbox);
        let call = make_call(serde_json::json!({ "path": "any.txt" }));

        let result = executor.read_file(&ctx, &call)?;
        match result {
            ToolOutput::Error { message } => {
                assert!(message.contains("ReadWorkspace"), "unexpected: {message}");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected error output, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_fs_read_invalid_args_returns_err() -> TestResult {
        let dir = TempDir::new()?;
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws)?;

        let executor = FsReadExecutor { max_bytes: 65_536 };
        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::ReadWorkspace])?;
        let ctx = make_ctx(sandbox);
        // Missing "path"
        let call = make_call(serde_json::json!({ "max_bytes": 100 }));

        let result = executor.read_file(&ctx, &call);
        assert!(
            matches!(result, Err(ToolsError::InvalidArguments { .. })),
            "expected InvalidArguments, got: {result:?}"
        );
        Ok(())
    }

    #[test]
    fn test_fs_read_rejects_missing_file() -> TestResult {
        let dir = TempDir::new()?;
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws)?;

        let executor = FsReadExecutor { max_bytes: 65_536 };
        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::ReadWorkspace])?;
        let ctx = make_ctx(sandbox);
        let call = make_call(serde_json::json!({ "path": "ghost.txt" }));

        let result = executor.read_file(&ctx, &call)?;
        assert!(matches!(result, ToolOutput::Error { .. }));
        Ok(())
    }

    #[test]
    fn test_fs_read_rejects_directory() -> TestResult {
        let dir = TempDir::new()?;
        let ws = dir.path().join("ws");
        fs::create_dir_all(ws.join("subdir"))?;

        let executor = FsReadExecutor { max_bytes: 65_536 };
        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::ReadWorkspace])?;
        let ctx = make_ctx(sandbox);
        let call = make_call(serde_json::json!({ "path": "subdir" }));

        let result = executor.read_file(&ctx, &call)?;
        assert!(matches!(result, ToolOutput::Error { .. }));
        Ok(())
    }

    #[test]
    fn test_fs_read_truncates_instead_of_rejecting() -> TestResult {
        let dir = TempDir::new()?;
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws)?;
        fs::write(ws.join("big.txt"), "A".repeat(100))?;

        let executor = FsReadExecutor { max_bytes: 50 };
        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::ReadWorkspace])?;
        let ctx = make_ctx(sandbox);
        // W1-03: Byte-Modus jetzt explizit über `offset: 0` angefordert (siehe
        // Kommentar in `test_fs_read_reads_file_successfully`).
        let call = make_call(serde_json::json!({ "path": "big.txt", "offset": 0 }));

        let result = executor.read_file(&ctx, &call)?;
        match result {
            ToolOutput::Text { content } => {
                assert!(
                    content.starts_with(&"A".repeat(50)),
                    "unexpected: {content}"
                );
                assert!(
                    !content.starts_with(&"A".repeat(51)),
                    "unexpected: {content}"
                );
                assert!(content.contains("gekürzt"), "missing hint: {content}");
                assert!(
                    content.contains("offset=50"),
                    "missing offset hint: {content}"
                );
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected text output, got: {other:?}"
                )));
            }
        }

        // Weiterlesen ab dem gemeldeten Offset liefert den Rest ohne Hinweis.
        let call = make_call(serde_json::json!({ "path": "big.txt", "offset": 50 }));
        match executor.read_file(&ctx, &call)? {
            ToolOutput::Text { content } => assert_eq!(content, "A".repeat(50)),
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected text output, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_fs_read_caps_provider_limit_at_64_kib() -> TestResult {
        let fixture = Fixture::new()?;
        fs::write(fixture.ws.join("huge.txt"), "B".repeat(200_000))?;
        let executor = FsReadExecutor {
            max_bytes: u64::MAX,
        };
        let ctx = fixture.ctx(vec![Permission::ReadWorkspace])?;
        // W1-03: Byte-Modus jetzt explizit über `offset: 0` angefordert (siehe
        // Kommentar in `test_fs_read_reads_file_successfully`).
        let call = call(
            "fs.read",
            serde_json::json!({ "path": "huge.txt", "offset": 0 }),
        );
        match executor.read_file(&ctx, &call)? {
            ToolOutput::Text { content } => {
                assert!(content.starts_with(&"B".repeat(MAX_OUTPUT_BYTES)));
                assert!(!content.starts_with(&"B".repeat(MAX_OUTPUT_BYTES + 1)));
                assert!(content.contains(&format!("offset={MAX_OUTPUT_BYTES}")));
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected text output, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_fs_read_truncation_respects_utf8_boundary() -> TestResult {
        let fixture = Fixture::new()?;
        fs::write(fixture.ws.join("umlaut.txt"), "aäb")?; // a, ä (2 Bytes), b
        let executor = FsReadExecutor { max_bytes: 2 };
        let ctx = fixture.ctx(vec![Permission::ReadWorkspace])?;
        // W1-03: Byte-Modus jetzt explizit über `offset: 0` angefordert (siehe
        // Kommentar in `test_fs_read_reads_file_successfully`).
        let call = call(
            "fs.read",
            serde_json::json!({ "path": "umlaut.txt", "offset": 0 }),
        );
        match executor.read_file(&ctx, &call)? {
            ToolOutput::Text { content } => {
                assert!(content.starts_with("a\n"), "unexpected: {content}");
                assert!(content.contains("offset=1"), "unexpected: {content}");
                assert!(!content.contains('\u{FFFD}'), "unexpected: {content}");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected text output, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_fs_read_offset_beyond_end_is_error() -> TestResult {
        let fixture = Fixture::new()?;
        fs::write(fixture.ws.join("short.txt"), "abc")?;
        let executor = FsReadExecutor { max_bytes: 65_536 };
        let ctx = fixture.ctx(vec![Permission::ReadWorkspace])?;
        let call = call(
            "fs.read",
            serde_json::json!({ "path": "short.txt", "offset": 4 }),
        );
        assert!(matches!(
            executor.read_file(&ctx, &call)?,
            ToolOutput::Error { .. }
        ));
        Ok(())
    }

    #[test]
    fn test_fs_read_rejects_symlinks_out_of_workspace() -> TestResult {
        let fixture = Fixture::new()?;
        fixture.plant_escapes()?;
        let executor = FsReadExecutor { max_bytes: 65_536 };
        let ctx = fixture.ctx(vec![Permission::ReadWorkspace])?;

        let escapes = [
            "link_file",
            "link_dir/secret.txt",
            "loop/link_file",
            "nested/up/x",
            "../outside/secret.txt",
        ];
        for path in escapes {
            let call = call("fs.read", serde_json::json!({ "path": path }));
            let output = executor.read_file(&ctx, &call)?;
            assert!(
                matches!(output, ToolOutput::Error { .. }),
                "{path}: {output:?}"
            );
            let rendered = render(&output)?;
            assert!(!rendered.contains(SECRET), "{path}: {rendered}");
        }
        Ok(())
    }

    #[test]
    fn test_fs_read_follows_symlink_inside_workspace() -> TestResult {
        let fixture = Fixture::new()?;
        fs::create_dir_all(fixture.ws.join("real/sub"))?;
        fs::write(fixture.ws.join("real/sub/header.h"), "inside")?;
        // Relativer Datei-Link, absoluter Verzeichnis-Link und eine Kette.
        std::os::unix::fs::symlink("real/sub/header.h", fixture.ws.join("file_link.h"))?;
        std::os::unix::fs::symlink(fixture.ws.join("real"), fixture.ws.join("dir_link"))?;
        fs::create_dir_all(fixture.ws.join("gen"))?;
        std::os::unix::fs::symlink("../file_link.h", fixture.ws.join("gen/chain.h"))?;
        let executor = FsReadExecutor { max_bytes: 65_536 };
        let ctx = fixture.ctx(vec![Permission::ReadWorkspace])?;

        for path in ["file_link.h", "dir_link/sub/header.h", "gen/chain.h"] {
            let call = call("fs.read", serde_json::json!({ "path": path, "offset": 0 }));
            match executor.read_file(&ctx, &call)? {
                ToolOutput::Text { content } => assert_eq!(content, "inside", "{path}"),
                other => {
                    return Err(TestError::Unexpected(format!("{path}: {other:?}")));
                }
            }
        }
        Ok(())
    }

    /// Ein Ziel außerhalb fragt (Politik vor dem Dispatch), wird nach der
    /// Freigabe gelesen, und das Verzeichnis ist danach gemerkt.
    #[test]
    fn test_fs_read_outside_symlink_reads_after_approval_and_remembers_the_directory() -> TestResult
    {
        let fixture = Fixture::new()?;
        fixture.plant_escapes()?;
        let executor = FsReadExecutor { max_bytes: 65_536 };
        let ctx = fixture.ctx(vec![Permission::ReadWorkspace])?;
        let access = crate::symlink::global();
        let args = serde_json::json!({ "path": "link_dir/secret.txt", "offset": 0 });

        // Ohne Freigabe abgelehnt; die Wurzel ist danach registriert.
        let first = executor.read_file(&ctx, &call("fs.read", args.clone()))?;
        assert!(matches!(first, ToolOutput::Error { .. }), "{first:?}");
        assert!(!render(&first)?.contains(SECRET));

        // Die Freigabepolitik sieht den Aufruf vor dem Dispatch und fragt.
        let target = access
            .review_call("fs.read", &args)
            .ok_or(TestError::Missing("an outside target must ask"))?;
        assert_eq!(target.grant_dir, fixture.outside);

        // Freigabe erteilt (Dialog bestätigt oder Full Access) → gelesen.
        access.approve_call("fs.read", &args, &target);
        match executor.read_file(&ctx, &call("fs.read", args.clone()))? {
            ToolOutput::Text { content } => assert_eq!(content, SECRET),
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }

        // Gemerkt: dasselbe Verzeichnis fragt nicht noch einmal.
        let deeper = serde_json::json!({ "path": "link_dir/deep/more.txt", "offset": 0 });
        assert!(access.review_call("fs.read", &deeper).is_none());
        match executor.read_file(&ctx, &call("fs.read", deeper))? {
            ToolOutput::Text { content } => assert_eq!(content, SECRET),
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        Ok(())
    }

    #[test]
    fn test_fs_read_symlink_outside_names_target() -> TestResult {
        let fixture = Fixture::new()?;
        fixture.plant_escapes()?;
        let executor = FsReadExecutor { max_bytes: 65_536 };
        let ctx = fixture.ctx(vec![Permission::ReadWorkspace])?;
        let outside = fixture
            .outside
            .to_str()
            .ok_or(TestError::Missing("outside as str"))?;

        for path in ["link_file", "link_dir/secret.txt"] {
            let call = call("fs.read", serde_json::json!({ "path": path }));
            let output = executor.read_file(&ctx, &call)?;
            assert!(
                matches!(output, ToolOutput::Error { .. }),
                "{path}: {output:?}"
            );
            let rendered = render(&output)?;
            assert!(
                rendered.contains("Symlink zeigt außerhalb des Arbeitsbereichs"),
                "{path}: {rendered}"
            );
            assert!(rendered.contains(outside), "{path}: Ziel fehlt: {rendered}");
            assert!(!rendered.contains(SECRET), "{path}: {rendered}");
        }

        // Relativer Ausbruch über `..` wird ebenso abgelehnt.
        std::os::unix::fs::symlink("../outside/secret.txt", fixture.ws.join("rel_escape"))?;
        let call = call("fs.read", serde_json::json!({ "path": "rel_escape" }));
        let rendered = render(&executor.read_file(&ctx, &call)?)?;
        assert!(
            rendered.contains("Symlink zeigt außerhalb des Arbeitsbereichs")
                && rendered.contains("../outside/secret.txt"),
            "{rendered}"
        );
        assert!(!rendered.contains(SECRET), "{rendered}");
        Ok(())
    }

    #[test]
    fn test_fs_read_race_directory_replaced_by_symlink() -> TestResult {
        let fixture = Fixture::new()?;
        fs::create_dir_all(fixture.ws.join("docs"))?;
        fs::write(fixture.ws.join("docs/secret.txt"), "harmless")?;
        let executor = FsReadExecutor { max_bytes: 65_536 };
        let ctx = fixture.ctx(vec![Permission::ReadWorkspace])?;
        let call = call("fs.read", serde_json::json!({ "path": "docs/secret.txt" }));
        assert!(matches!(
            executor.read_file(&ctx, &call)?,
            ToolOutput::Text { .. }
        ));

        // Wettlauf-Surrogat: das geprüfte Verzeichnis wird durch einen Symlink
        // nach außen ersetzt; derselbe Aufruf muss jetzt scheitern.
        fs::rename(fixture.ws.join("docs"), fixture.ws.join("docs_old"))?;
        std::os::unix::fs::symlink(&fixture.outside, fixture.ws.join("docs"))?;
        let output = executor.read_file(&ctx, &call)?;
        assert!(matches!(output, ToolOutput::Error { .. }), "{output:?}");
        assert!(!render(&output)?.contains(SECRET));
        Ok(())
    }

    #[test]
    fn test_fs_read_execute_runs_on_runtime() -> TestResult {
        let fixture = Fixture::new()?;
        fs::write(fixture.ws.join("async.txt"), "via spawn_blocking")?;
        let executor = FsReadExecutor { max_bytes: 65_536 };
        let ctx = fixture.ctx(vec![Permission::ReadWorkspace])?;
        // W1-03: Byte-Modus jetzt explizit über `offset: 0` angefordert (siehe
        // Kommentar in `test_fs_read_reads_file_successfully`).
        let call = call(
            "fs.read",
            serde_json::json!({ "path": "async.txt", "offset": 0 }),
        );
        let runtime = tokio::runtime::Builder::new_current_thread().build()?;
        match runtime.block_on(executor.execute(&ctx, &call))? {
            ToolOutput::Text { content } => assert_eq!(content, "via spawn_blocking"),
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected text output, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    // --- W1-03: Zeilenmodus (Standard) -------------------------------------

    #[test]
    fn test_fs_read_line_mode_default_numbers_all_lines() -> TestResult {
        let fixture = Fixture::new()?;
        fs::write(fixture.ws.join("lines.txt"), "one\ntwo\nthree\n")?;
        let executor = FsReadExecutor { max_bytes: 65_536 };
        let ctx = fixture.ctx(vec![Permission::ReadWorkspace])?;
        let call = call("fs.read", serde_json::json!({ "path": "lines.txt" }));
        match executor.read_file(&ctx, &call)? {
            ToolOutput::Text { content } => {
                assert!(
                    content.starts_with("lines.txt — Zeilen 1–3 von 3"),
                    "{content}"
                );
                assert!(content.contains("1\tone"), "{content}");
                assert!(content.contains("2\ttwo"), "{content}");
                assert!(content.contains("3\tthree"), "{content}");
                assert!(!content.contains("weiter mit line="), "{content}");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected text output, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_fs_read_line_mode_range_and_continuation_footer() -> TestResult {
        let fixture = Fixture::new()?;
        let content: String = (1..=10).map(|i| format!("line{i}\n")).collect();
        fs::write(fixture.ws.join("ten.txt"), content)?;
        let executor = FsReadExecutor { max_bytes: 65_536 };
        let ctx = fixture.ctx(vec![Permission::ReadWorkspace])?;
        let call = call(
            "fs.read",
            serde_json::json!({ "path": "ten.txt", "line": 3, "limit": 2 }),
        );
        match executor.read_file(&ctx, &call)? {
            ToolOutput::Text { content } => {
                assert!(
                    content.starts_with("ten.txt — Zeilen 3–4 von 10"),
                    "{content}"
                );
                assert!(content.contains("3\tline3"), "{content}");
                assert!(content.contains("4\tline4"), "{content}");
                assert!(!content.contains("line2"), "{content}");
                assert!(!content.contains("\tline5"), "{content}");
                assert!(content.contains("weiter mit line=5"), "{content}");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected text output, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_fs_read_line_mode_beyond_eof_reports_total() -> TestResult {
        let fixture = Fixture::new()?;
        fs::write(fixture.ws.join("short3.txt"), "a\nb\nc\n")?;
        let executor = FsReadExecutor { max_bytes: 65_536 };
        let ctx = fixture.ctx(vec![Permission::ReadWorkspace])?;
        let call = call(
            "fs.read",
            serde_json::json!({ "path": "short3.txt", "line": 10 }),
        );
        match executor.read_file(&ctx, &call)? {
            ToolOutput::Text { content } => {
                assert!(content.contains("Datei hat nur 3 Zeilen"), "{content}");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected text output, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_fs_read_line_mode_empty_file_reports_zero_lines() -> TestResult {
        let fixture = Fixture::new()?;
        fs::write(fixture.ws.join("empty.txt"), "")?;
        let executor = FsReadExecutor { max_bytes: 65_536 };
        let ctx = fixture.ctx(vec![Permission::ReadWorkspace])?;
        let call = call("fs.read", serde_json::json!({ "path": "empty.txt" }));
        match executor.read_file(&ctx, &call)? {
            ToolOutput::Text { content } => {
                assert!(content.contains("Datei hat nur 0 Zeilen"), "{content}");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected text output, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_fs_read_line_mode_truncates_long_line() -> TestResult {
        let fixture = Fixture::new()?;
        let long_line = "x".repeat(2000);
        fs::write(
            fixture.ws.join("longline.txt"),
            format!("{long_line}\nshort\n"),
        )?;
        let executor = FsReadExecutor { max_bytes: 65_536 };
        let ctx = fixture.ctx(vec![Permission::ReadWorkspace])?;
        let call = call("fs.read", serde_json::json!({ "path": "longline.txt" }));
        match executor.read_file(&ctx, &call)? {
            ToolOutput::Text { content } => {
                assert!(content.contains("[Zeile gekürzt]"), "{content}");
                assert!(!content.contains(&"x".repeat(2000)), "{content}");
                assert!(content.contains("2\tshort"), "{content}");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected text output, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_fs_read_rejects_mixed_byte_and_line_params() -> TestResult {
        let fixture = Fixture::new()?;
        fs::write(fixture.ws.join("mix.txt"), "a\nb\n")?;
        let executor = FsReadExecutor { max_bytes: 65_536 };
        let ctx = fixture.ctx(vec![Permission::ReadWorkspace])?;
        let call = call(
            "fs.read",
            serde_json::json!({ "path": "mix.txt", "offset": 5, "line": 3 }),
        );
        match executor.read_file(&ctx, &call)? {
            ToolOutput::Error { message } => {
                // Die Meldung nennt die widersprüchlichen Felder samt Werten
                // und sagt, welche auf null müssen.
                assert!(message.contains("'line'=3"), "{message}");
                assert!(message.contains("'offset'=5"), "{message}");
                assert!(message.contains("'offset' auf null"), "{message}");
                assert!(message.contains("'line' auf null"), "{message}");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected error output, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    // --- W1-03: Tail-Modus ---------------------------------------------------

    #[test]
    fn test_fs_read_tail_mode_returns_last_n_lines() -> TestResult {
        let fixture = Fixture::new()?;
        let content: String = (1..=10).map(|i| format!("line{i}\n")).collect();
        fs::write(fixture.ws.join("ten.txt"), content)?;
        let executor = FsReadExecutor { max_bytes: 65_536 };
        let ctx = fixture.ctx(vec![Permission::ReadWorkspace])?;
        let call = call(
            "fs.read",
            serde_json::json!({ "path": "ten.txt", "tail": 3 }),
        );
        match executor.read_file(&ctx, &call)? {
            ToolOutput::Text { content } => {
                assert!(
                    content.starts_with("ten.txt — Zeilen 8–10 von 10"),
                    "{content}"
                );
                assert!(content.contains("8\tline8"), "{content}");
                assert!(content.contains("9\tline9"), "{content}");
                assert!(content.contains("10\tline10"), "{content}");
                assert!(!content.contains("line7"), "{content}");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected text output, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_fs_read_tail_mode_exceeds_total_returns_whole_file() -> TestResult {
        let fixture = Fixture::new()?;
        fs::write(fixture.ws.join("three.txt"), "a\nb\nc\n")?;
        let executor = FsReadExecutor { max_bytes: 65_536 };
        let ctx = fixture.ctx(vec![Permission::ReadWorkspace])?;
        let call = call(
            "fs.read",
            serde_json::json!({ "path": "three.txt", "tail": 50 }),
        );
        match executor.read_file(&ctx, &call)? {
            ToolOutput::Text { content } => {
                assert!(
                    content.starts_with("three.txt — Zeilen 1–3 von 3"),
                    "{content}"
                );
                assert!(content.contains("1\ta"), "{content}");
                assert!(content.contains("2\tb"), "{content}");
                assert!(content.contains("3\tc"), "{content}");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected text output, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_fs_read_rejects_tail_combined_with_line() -> TestResult {
        let fixture = Fixture::new()?;
        fs::write(fixture.ws.join("mix2.txt"), "a\nb\n")?;
        let executor = FsReadExecutor { max_bytes: 65_536 };
        let ctx = fixture.ctx(vec![Permission::ReadWorkspace])?;
        let call = call(
            "fs.read",
            serde_json::json!({ "path": "mix2.txt", "tail": 1, "line": 2 }),
        );
        match executor.read_file(&ctx, &call)? {
            ToolOutput::Error { message } => {
                assert!(message.contains("'tail'=1"), "{message}");
                assert!(message.contains("'line'=2"), "{message}");
                assert!(message.contains("'line' auf null"), "{message}");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected error output, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_fs_read_tail_with_limit_takes_the_smaller_count() -> TestResult {
        // Bugreport Runde 5: `tail` zusammen mit `limit` ist kein Widerspruch
        // mehr — `limit` begrenzt die Tail-Zeilen zusätzlich.
        let fixture = Fixture::new()?;
        fs::write(fixture.ws.join("mix3.txt"), "a\nb\nc\nd\n")?;
        let executor = FsReadExecutor { max_bytes: 65_536 };
        let ctx = fixture.ctx(vec![Permission::ReadWorkspace])?;
        let call = call(
            "fs.read",
            serde_json::json!({ "path": "mix3.txt", "tail": 3, "limit": 2 }),
        );
        match executor.read_file(&ctx, &call)? {
            ToolOutput::Text { content } => {
                assert!(
                    content.starts_with("mix3.txt — Zeilen 3–4 von 4"),
                    "{content}"
                );
                assert!(!content.contains("2\tb"), "{content}");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected text output, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    // --- Runde 5: Strict-Schema, null und harmlose Kombinationen ----------

    #[test]
    fn test_fs_read_all_fields_null_is_the_default_line_mode() -> TestResult {
        // Strict-Schema: alle Felder required, ungenutzte kommen als null.
        let fixture = Fixture::new()?;
        fs::write(fixture.ws.join("nulls.txt"), "eins\nzwei\n")?;
        let executor = FsReadExecutor { max_bytes: 65_536 };
        let ctx = fixture.ctx(vec![Permission::ReadWorkspace])?;
        let call = call(
            "fs.read",
            serde_json::json!({
                "path": "nulls.txt",
                "max_bytes": null,
                "offset": null,
                "line": null,
                "limit": null,
                "tail": null,
            }),
        );
        match executor.read_file(&ctx, &call)? {
            ToolOutput::Text { content } => {
                assert!(
                    content.starts_with("nulls.txt — Zeilen 1–2 von 2"),
                    "{content}"
                );
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected text output, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_fs_read_line_mode_tolerates_zeros_for_unused_fields() -> TestResult {
        // Der Aufruf aus dem Bugreport: line + limit, die übrigen Felder
        // mit 0 statt null.
        let fixture = Fixture::new()?;
        let content: String = (1..=5).map(|i| format!("z{i}\n")).collect();
        fs::write(fixture.ws.join("zeros.txt"), content)?;
        let executor = FsReadExecutor { max_bytes: 65_536 };
        let ctx = fixture.ctx(vec![Permission::ReadWorkspace])?;
        let call = call(
            "fs.read",
            serde_json::json!({
                "path": "zeros.txt",
                "line": 1,
                "limit": 2,
                "offset": 0,
                "max_bytes": 0,
                "tail": 0,
            }),
        );
        match executor.read_file(&ctx, &call)? {
            ToolOutput::Text { content } => {
                assert!(
                    content.starts_with("zeros.txt — Zeilen 1–2 von 5"),
                    "{content}"
                );
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected text output, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_fs_read_tail_tolerates_default_line_and_offset() -> TestResult {
        let fixture = Fixture::new()?;
        let content: String = (1..=10).map(|i| format!("t{i}\n")).collect();
        fs::write(fixture.ws.join("tail.txt"), content)?;
        let executor = FsReadExecutor { max_bytes: 65_536 };
        let ctx = fixture.ctx(vec![Permission::ReadWorkspace])?;
        let call = call(
            "fs.read",
            serde_json::json!({ "path": "tail.txt", "tail": 2, "line": 1, "offset": 0 }),
        );
        match executor.read_file(&ctx, &call)? {
            ToolOutput::Text { content } => {
                assert!(
                    content.starts_with("tail.txt — Zeilen 9–10 von 10"),
                    "{content}"
                );
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected text output, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_fs_read_default_line_next_to_offset_uses_byte_mode() -> TestResult {
        let fixture = Fixture::new()?;
        fs::write(fixture.ws.join("bytes.txt"), "0123456789")?;
        let executor = FsReadExecutor { max_bytes: 65_536 };
        let ctx = fixture.ctx(vec![Permission::ReadWorkspace])?;
        let call = call(
            "fs.read",
            serde_json::json!({ "path": "bytes.txt", "offset": 4, "line": 1 }),
        );
        match executor.read_file(&ctx, &call)? {
            ToolOutput::Text { content } => assert!(content.starts_with("456789"), "{content}"),
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected text output, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_fs_read_rejects_tail_with_offset_and_names_the_fields() -> TestResult {
        let fixture = Fixture::new()?;
        fs::write(fixture.ws.join("mix4.txt"), "a\nb\n")?;
        let executor = FsReadExecutor { max_bytes: 65_536 };
        let ctx = fixture.ctx(vec![Permission::ReadWorkspace])?;
        let call = call(
            "fs.read",
            serde_json::json!({ "path": "mix4.txt", "tail": 5, "offset": 10, "max_bytes": 100 }),
        );
        match executor.read_file(&ctx, &call)? {
            ToolOutput::Error { message } => {
                assert!(message.contains("'offset'=10"), "{message}");
                assert!(message.contains("'max_bytes'=100"), "{message}");
                assert!(
                    message.contains("'offset'/'max_bytes' auf null"),
                    "{message}"
                );
                assert!(message.contains("'tail' auf null"), "{message}");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected error output, got: {other:?}"
                )));
            }
        }
        Ok(())
    }
}
