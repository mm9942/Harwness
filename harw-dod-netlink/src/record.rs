//! Formungsteil: Audit-Records als Rohtext, Tokenisierung, getypte Felder.
//!
//! # Verantwortungsbereich
//! [`RawRecord`] trägt eine Zeile Audit-Rohtext. [`parse_record`] zerlegt sie
//! in [`AuditFields`]: die sechs Felder, auf die dieses Programm angewiesen
//! ist — `type`, `auid`, `uid`, `pid`, `success` und der Zeitstempel aus dem
//! `msg=audit(…)`-Feld.
//!
//! **Braucht keine Berechtigung und keinen Kernel.** Alles in diesem Modul
//! ist eine reine Funktion auf Bytes bzw. Zeichenketten; jeder Test läuft
//! ohne Root und ohne Audit-Subsystem. Das ist der Punkt der Trennung dieser
//! Crate (siehe Crate-Dokumentation): der weitaus größte Teil ist hier, nicht
//! in [`crate::socket`].
//!
//! # `key=value`-Grammatik dieser Crate
//! Ein Audit-Record ist eine Folge von durch Leerraum getrennten
//! `schlüssel=wert`-Paaren. Ein Wert ist entweder ein nacktes Token (bis zum
//! nächsten Leerzeichen) oder in doppelte Anführungszeichen gefasst
//! (`schlüssel="wert mit leerzeichen"`); [`parse_record`] entpackt beide
//! Formen gleich. Ein Token ohne `=` wird übersprungen statt die ganze Zeile
//! scheitern zu lassen — nur ein fehlendes Pflichtfeld (`type`) oder ein
//! vorhandenes, aber unlesbares Feld macht den Record insgesamt
//! [`crate::error::NetlinkError::MalformedRecord`].
//!
//! # Entscheidung: keine Hex-Dekodierung
//! Audit-Records kodieren manche Textfelder (`exe`, `comm`, `path`, `key`,
//! …) hexadezimal, wenn ihr Wert Zeichen enthält, die als `key=value`-Token
//! nicht sicher darstellbar wären. **Diese Crate dekodiert das nicht.** Keins
//! der sechs Felder in [`AuditFields`] kommt hex-kodiert vor: `type` ist ein
//! fester Bezeichner, `auid`/`uid`/`pid` sind Dezimalzahlen, `success` ist
//! `yes`/`no`, und der Zeitstempel steht unverschlüsselt im `msg`-Feld. Ein
//! hex-kodiertes Token bei einem anderen Schlüssel wird von der Tokenisierung
//! unverändert als opaker Rohwert behandelt und nicht in [`AuditFields`]
//! übernommen — eine bewusste, dokumentierte Entscheidung, kein offener
//! Punkt.
//!
//! # Exportierte Typen
//! [`RawRecord`], [`AuditFields`], die freie Funktion [`parse_record`].
//!
//! # Nebenläufigkeit
//! Reine Werttypen und zustandslose freie Funktionen: `Send + Sync`, aus
//! beliebig vielen Threads gleichzeitig nutzbar.
//!
//! # Fehler
//! [`crate::error::NetlinkError::MalformedRecord`] — inhaltsfrei, nennt nie
//! den Recordinhalt.
//!
//! # Examples
//! ```rust
//! use harw_dod_netlink::{RawRecord, parse_record};
//!
//! let record = RawRecord::new(
//!     "type=SYSCALL msg=audit(1699999999.123:456): auid=1000 uid=0 pid=4242 success=yes",
//! );
//! let fields = parse_record(&record).expect("record ist wohlgeformt");
//!
//! assert_eq!(fields.record_type, "SYSCALL");
//! assert_eq!(fields.auid, Some(1000));
//! assert_eq!(fields.uid, Some(0));
//! assert_eq!(fields.success, Some(true));
//! ```

use crate::error::NetlinkError;

/// Ein Audit-Record als Rohtext, wie er (nach Rahmenzerlegung) aus dem
/// Netlink-Socket kommt.
///
/// # Description
/// Trägt genau eine Zeile `key=value`-Text. Konstruktion ist unfehlbar —
/// jede Zeichenkette ist ein gültiges `RawRecord`; ob sie ein *wohlgeformter*
/// Audit-Record ist, entscheidet erst [`parse_record`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawRecord {
    line: String,
}

impl RawRecord {
    /// Baut einen `RawRecord` aus einer Textzeile.
    ///
    /// # Arguments
    /// - `line` (`impl Into<String>`): der Rohtext, typischerweise eine
    ///   einzelne Audit-Zeile.
    ///
    /// # Returns
    /// Einen `RawRecord`, dessen [`Self::as_str`] exakt `line` zurückgibt.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_netlink::RawRecord;
    ///
    /// let record = RawRecord::new("type=SYSCALL");
    /// assert_eq!(record.as_str(), "type=SYSCALL");
    /// ```
    #[must_use]
    pub fn new(line: impl Into<String>) -> Self {
        Self { line: line.into() }
    }

    /// Der rohe Zeilentext dieses Records.
    ///
    /// # Returns
    /// Den bei [`Self::new`] übergebenen Text, unverändert.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.line
    }
}

impl From<String> for RawRecord {
    fn from(line: String) -> Self {
        Self { line }
    }
}

impl From<&str> for RawRecord {
    fn from(line: &str) -> Self {
        Self { line: line.to_owned() }
    }
}

/// Die getypten Felder eines Audit-Records, die dieses Programm auswertet.
///
/// # Description
/// Nur diese sechs Felder zählen für das Programm (siehe Crate-Dokumentation
/// zur Begründung, warum `auid` und nicht `uid` die entscheidende Kennung
/// ist). Jedes Feld außer `record_type` ist optional, weil nicht jeder
/// Audit-Record-Typ jedes Feld führt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditFields {
    /// Die Recordart, z. B. `"SYSCALL"` oder `"USER_AUTH"` — der Wert des
    /// `type`-Schlüssels, unverändert. Das einzige Pflichtfeld: fehlt es,
    /// liefert [`parse_record`] [`NetlinkError::MalformedRecord`].
    pub record_type: String,

    /// Die **Anmelde-UID** (`login UID`) — der Wert, den `sudo`, `su` oder
    /// ein Setuid-Programm **nicht** verändert, und damit die einzige
    /// Kennung, die beantwortet, wer eine Aktion wirklich ausgelöst hat.
    /// `uid` allein beantwortet diese Frage nicht: `uid` ist die aktuelle
    /// Ausführungs-UID und wechselt mit jedem Rechtewechsel.
    ///
    /// Der Kernel setzt den rohen `auid`-Wert `4294967295`
    /// (`u32::MAX`, `-1` als vorzeichenloses 32-Bit-Wort) für „keine
    /// Anmelde-UID zugewiesen" — typischerweise Kernel-Threads oder Prozesse,
    /// die vor dem ersten `audit_set_loginuid`-Aufruf laufen. **Dieser Wert
    /// erscheint hier als `None`, nicht als `Some(4294967295)`.** Das ist die
    /// klassische Fehlerquelle beim Auswerten von `auid`: wer den
    /// Sentinelwert nicht abfängt, hält einen nicht angemeldeten Prozess für
    /// einen Benutzer mit der (nicht existierenden) UID 4294967295.
    /// `auid=0` dagegen ist ein echter, davon zu unterscheidender Fall: root,
    /// der sich tatsächlich angemeldet hat, und erscheint als `Some(0)`.
    pub auid: Option<u32>,

    /// Die aktuelle Ausführungs-UID (`uid`-Schlüssel). Kann von `auid`
    /// abweichen, sobald ein Prozess die Rechte gewechselt hat (`sudo`, …).
    pub uid: Option<u32>,

    /// Die Prozess-ID (`pid`-Schlüssel) des auslösenden Prozesses.
    pub pid: Option<u32>,

    /// Ob die protokollierte Operation erfolgreich war (`success`-Schlüssel,
    /// `yes`/`no`).
    pub success: Option<bool>,

    /// Der Zeitstempel aus dem `msg=audit(sekunden.millisekunden:seriennummer):`-Feld.
    pub timestamp: Option<jiff::Timestamp>,
}

/// Ein einzelnes `schlüssel=wert`-Token, mit an- oder abwesenden
/// Anführungszeichen um den Wert.
struct Token<'a> {
    key: &'a str,
    value: &'a str,
}

/// Zerlegt eine Audit-Zeile in `schlüssel=wert`-Token.
///
/// Siehe die `key=value`-Grammatik in der Moduldokumentation. Token ohne `=`
/// werden stillschweigend übersprungen; sie tragen für diese Crate keine
/// Information und ein Audit-Record kann durchaus mehr Felder führen, als
/// diese Crate kennt.
fn tokenize(line: &str) -> Vec<Token<'_>> {
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut tokens = Vec::new();
    let mut i = 0usize;

    while i < len {
        while i < len && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= len {
            break;
        }

        let key_start = i;
        while i < len && bytes[i] != b'=' && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }

        if i >= len || bytes[i] != b'=' {
            // Kein '=' vor dem nächsten Leerraum: kein gültiges Token, überspringen.
            while i < len && !bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            continue;
        }

        let key = &line[key_start..i];
        i += 1; // '=' überspringen.

        if i < len && bytes[i] == b'"' {
            i += 1;
            let value_start = i;
            while i < len && bytes[i] != b'"' {
                i += 1;
            }
            let value = &line[value_start..i];
            if i < len {
                i += 1; // schließendes '"' überspringen.
            }
            tokens.push(Token { key, value });
        } else {
            let value_start = i;
            while i < len && !bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            let value = &line[value_start..i];
            tokens.push(Token { key, value });
        }
    }

    tokens
}

/// Entfernt ein optionales, einzeln umschließendes Anführungszeichenpaar.
///
/// Nur zur Verteidigung: [`tokenize`] hat Anführungszeichen bereits entfernt.
/// Bleibt eine Fassade für den Fall, dass ein Wert direkt (nicht über
/// [`tokenize`]) übergeben wird — aktuell ungenutzt außerhalb dieses Moduls,
/// aber Teil der dokumentierten Entpack-Regel „Werte in Anführungszeichen und
/// nackte Werte werden gleich behandelt".
fn unquote(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(value)
}

/// Parst den Zeitstempel aus dem Wert des `msg`-Schlüssels
/// (`audit(sekunden.millisekunden:seriennummer):`).
fn parse_audit_timestamp(value: &str) -> Result<jiff::Timestamp, NetlinkError> {
    let after_prefix = value
        .strip_prefix("audit(")
        .ok_or(NetlinkError::MalformedRecord)?;
    let paren_end = after_prefix
        .find(')')
        .ok_or(NetlinkError::MalformedRecord)?;
    let inner = &after_prefix[..paren_end];

    let (secs_part, _serial_part) = inner.split_once(':').ok_or(NetlinkError::MalformedRecord)?;
    let (whole_str, frac_str) = secs_part.split_once('.').unwrap_or((secs_part, "0"));

    let whole: i64 = whole_str
        .parse()
        .map_err(|_| NetlinkError::MalformedRecord)?;

    let frac_digits = frac_str.len().clamp(1, 9);
    let frac_trimmed = if frac_str.is_empty() { "0" } else { &frac_str[..frac_str.len().min(9)] };
    let frac_value: u32 = frac_trimmed
        .parse()
        .map_err(|_| NetlinkError::MalformedRecord)?;
    let nanos = frac_value.saturating_mul(10u32.pow((9 - frac_digits) as u32));

    jiff::Timestamp::new(whole, nanos as i32).map_err(|_| NetlinkError::MalformedRecord)
}

/// Parst einen [`RawRecord`] in [`AuditFields`].
///
/// # Description
/// Tokenisiert die Zeile (siehe Moduldokumentation) und liest daraus `type`
/// (Pflichtfeld), `auid`, `uid`, `pid`, `success` und den Zeitstempel aus
/// `msg`. Werte in Anführungszeichen und nackte Werte werden gleich
/// behandelt. Der `auid`-Sentinelwert `4294967295` wird zu `None`
/// normalisiert — siehe [`AuditFields::auid`] für die Begründung.
///
/// # Arguments
/// - `record` (`&RawRecord`): der zu parsende Rohtext.
///
/// # Returns
/// Die extrahierten [`AuditFields`].
///
/// # Errors
/// - [`NetlinkError::MalformedRecord`]: der `type`-Schlüssel fehlt, oder ein
///   vorhandenes Feld (`auid`, `uid`, `pid`, `success`, `msg`) hat einen Wert,
///   der nicht dem erwarteten Format entspricht. Die Fehlermeldung nennt
///   nie den Recordinhalt.
///
/// # Examples
/// ```rust
/// use harw_dod_netlink::{RawRecord, parse_record};
///
/// // Der klassische auid-Sonderfall: 4294967295 heißt "nicht angemeldet".
/// let record = RawRecord::new("type=SYSCALL auid=4294967295 uid=0");
/// let fields = parse_record(&record).expect("wohlgeformt");
/// assert_eq!(fields.auid, None);
/// ```
pub fn parse_record(record: &RawRecord) -> Result<AuditFields, NetlinkError> {
    let tokens = tokenize(record.as_str());

    let mut record_type: Option<String> = None;
    let mut auid: Option<u32> = None;
    let mut uid: Option<u32> = None;
    let mut pid: Option<u32> = None;
    let mut success: Option<bool> = None;
    let mut timestamp: Option<jiff::Timestamp> = None;

    for token in &tokens {
        let value = unquote(token.value);
        match token.key {
            "type" => record_type = Some(value.to_owned()),
            "auid" => {
                let raw: u32 = value.parse().map_err(|_| NetlinkError::MalformedRecord)?;
                auid = if raw == u32::MAX { None } else { Some(raw) };
            }
            "uid" => {
                uid = Some(value.parse().map_err(|_| NetlinkError::MalformedRecord)?);
            }
            "pid" => {
                pid = Some(value.parse().map_err(|_| NetlinkError::MalformedRecord)?);
            }
            "success" => {
                success = Some(match value {
                    "yes" => true,
                    "no" => false,
                    _ => return Err(NetlinkError::MalformedRecord),
                });
            }
            "msg" => {
                timestamp = Some(parse_audit_timestamp(value)?);
            }
            _ => {}
        }
    }

    let record_type = record_type.ok_or(NetlinkError::MalformedRecord)?;

    Ok(AuditFields {
        record_type,
        auid,
        uid,
        pid,
        success,
        timestamp,
    })
}

#[cfg(test)]
mod tests {
    use super::{RawRecord, parse_record};

    #[test]
    fn test_parse_record_full_record_extracts_all_fields() {
        let record = RawRecord::new(
            "type=SYSCALL msg=audit(1699999999.123:456): auid=1000 uid=0 pid=4242 success=yes",
        );
        let fields = parse_record(&record).expect("wohlgeformter Record");

        assert_eq!(fields.record_type, "SYSCALL");
        assert_eq!(fields.auid, Some(1000));
        assert_eq!(fields.uid, Some(0));
        assert_eq!(fields.pid, Some(4242));
        assert_eq!(fields.success, Some(true));
        let ts = fields.timestamp.expect("Zeitstempel vorhanden");
        assert_eq!(ts.as_second(), 1_699_999_999);
    }

    #[test]
    fn test_parse_record_auid_sentinel_max_value_is_none() {
        // Der wichtigste Test dieser Crate: 4294967295 (u32::MAX, -1 als u32)
        // heißt "keine Anmelde-UID gesetzt" und muss None ergeben, nicht
        // Some(4294967295).
        let record = RawRecord::new("type=SYSCALL auid=4294967295 uid=0");
        let fields = parse_record(&record).expect("wohlgeformter Record");
        assert_eq!(fields.auid, None);
    }

    #[test]
    fn test_parse_record_auid_zero_is_some_zero() {
        // Abgrenzung zum Sentinelwert: auid=0 ist root, der sich tatsächlich
        // angemeldet hat, kein "nicht gesetzt".
        let record = RawRecord::new("type=SYSCALL auid=0 uid=0");
        let fields = parse_record(&record).expect("wohlgeformter Record");
        assert_eq!(fields.auid, Some(0));
    }

    #[test]
    fn test_parse_record_quoted_value_is_unpacked() {
        let record = RawRecord::new(r#"type=SYSCALL success="yes" auid=1000"#);
        let fields = parse_record(&record).expect("wohlgeformter Record");
        assert_eq!(fields.success, Some(true));
    }

    #[test]
    fn test_parse_record_malformed_record_error_message_has_no_record_content() {
        let record = RawRecord::new("type=SYSCALL auid=not-a-number");
        let err = parse_record(&record).expect_err("auid ist keine Zahl");
        let message = err.to_string();
        assert_eq!(message, "audit record is malformed");
        assert!(!message.contains("not-a-number"));
        assert!(!message.contains("SYSCALL"));
    }

    #[test]
    fn test_parse_record_missing_required_type_field_is_detected() {
        let record = RawRecord::new("auid=1000 uid=0 pid=4242");
        let err = parse_record(&record).expect_err("type fehlt");
        assert_eq!(err.to_string(), "audit record is malformed");
    }

    #[test]
    fn test_parse_record_unrelated_hex_encoded_field_is_ignored() {
        // exe ist hex-kodiert (siehe Moduldokumentation: bewusst nicht
        // dekodiert), stört aber die Extraktion der bekannten Felder nicht.
        let record = RawRecord::new("type=SYSCALL exe=2F62696E2F62617368 auid=1000");
        let fields = parse_record(&record).expect("wohlgeformter Record");
        assert_eq!(fields.auid, Some(1000));
    }

    #[test]
    fn test_raw_record_as_str_roundtrips() {
        let record = RawRecord::new("type=SYSCALL");
        assert_eq!(record.as_str(), "type=SYSCALL");
    }
}
