//! Formungsteil: Audit-Records als Rohtext, Tokenisierung, getypte Felder.
//!
//! # Verantwortungsbereich
//! [`RawRecord`] trägt eine Zeile Audit-Rohtext und — wenn sie aus dem
//! Netlink-Socket stammt — den Recordtyp aus dem `nlmsghdr`
//! ([`RawRecord::msg_type`]). [`parse_record`] zerlegt sie in
//! [`AuditFields`]: die Felder, auf die dieses Programm angewiesen ist —
//! Recordart, `auid`, `uid`, `pid`, das Ergebnis der Operation und der
//! Zeitstempel aus `audit(…)`.
//!
//! **Braucht keine Berechtigung und keinen Kernel.** Alles in diesem Modul
//! ist eine reine Funktion auf Bytes bzw. Zeichenketten; jeder Test läuft
//! ohne Root und ohne Audit-Subsystem. Das ist der Punkt der Trennung dieser
//! Crate (siehe Crate-Dokumentation): der weitaus größte Teil ist hier, nicht
//! in `crate::socket`.
//!
//! # Zwei Eingabeformen
//! - **auditd-Textform** (Logdatei, [`RawRecord::new`]):
//!   `type=USER_AUTH msg=audit(1699999999.456:457): pid=4300 uid=0 auid=1000 ses=2 msg='op=PAM:authentication … res=failed'`.
//!   Die Recordart steht als `type=`-Schlüssel, der Zeitstempel im
//!   **unquotierten** `msg=audit(…):`.
//! - **Kernel-Netlink-Nutzlast** (Multicast-Gruppe `AUDIT_NLGRP_READLOG`,
//!   [`RawRecord::from_netlink`]):
//!   `audit(1699999999.456:457): pid=4300 uid=0 auid=1000 ses=2 msg='…'`.
//!   Es gibt **kein** `type=`-Token und keinen `msg=audit(`-Schlüssel: die
//!   Recordart steht als Zahl im `nlmsghdr` (Namen über
//!   [`audit_type_name`]), der Zeitstempel ist das erste, nackte Token.
//!   Trägt ein Record einen Typ aus dem `nlmsghdr`, gilt **nur** dieser —
//!   ein `type=` in der Nutzlast wird ignoriert, denn den Kopf setzt der
//!   Kernel, die Nutzlast ist teilweise Text eines Userspace-Programms.
//!
//! # `key=value`-Grammatik dieser Crate
//! - **Trenner** sind ASCII-Leerraum und das Gruppentrennzeichen `U+001D`
//!   (auditd mit `log_format = ENRICHED` hängt damit die aufgelösten Namen
//!   an).
//! - Ein **Wert** ist ein nacktes Token bis zum nächsten Trenner, in doppelte
//!   Anführungszeichen gefasst bis zum nächsten `"`, oder in einfache
//!   Anführungszeichen gefasst bis zum **letzten** `'` der Zeile (siehe
//!   nächster Abschnitt). Die Anführungszeichen selbst gehören nicht zum
//!   Wert.
//! - Ein **nicht geschlossenes** Anführungszeichen macht den Record zu
//!   [`crate::error::NetlinkError::MalformedRecord`].
//! - Ein Token ohne `=` wird übersprungen statt die ganze Zeile scheitern zu
//!   lassen (Kernel-Records wie AVC tragen solche Wörter). Einzige Ausnahme:
//!   das **erste** Token, wenn es mit `audit(` beginnt — das ist der
//!   Zeitstempel der Netlink-Nutzlast.
//! - Nur ein fehlendes Pflichtfeld (die Recordart) oder ein vorhandenes, aber
//!   unlesbares Feld macht den Record insgesamt
//!   [`crate::error::NetlinkError::MalformedRecord`].
//!
//! # Das `msg='…'`-Feld der Userspace-Records
//! Für USER_*-Records (`USER_AUTH`, `USER_LOGIN`, `USER_CMD`, …) hängt der
//! Kernel den Text des meldenden Programms als `msg='…'` an. Das ist der
//! einzige Teil eines Records, den nicht der Kernel selbst formt. Zwei
//! Regeln halten ihn von den Kernel-Feldern fern:
//! 1. Das schließende `'` ist das **letzte** `'` der Zeile. Ein `'` im Text
//!    (etwa in einem doppelt quotierten `acct="…"`) beendet das Feld damit
//!    nicht vorzeitig, und ein eingeschmuggeltes `' auid=0 '` bleibt innerhalb
//!    von `msg`, statt als äußeres Feld gelesen zu werden.
//! 2. Aus dem Inneren wird ausschließlich `res=` gelesen. `pid`, `uid`,
//!    `auid` und der Zeitstempel kommen nur aus dem äußeren, vom Kernel
//!    geformten Teil.
//! 3. Maßgeblich ist nur das **letzte** `res=` im Inneren. libaudit
//!    (`audit_log_acct_message`, `audit_log_user_message`, …) schreibt
//!    `res=%s` immer als letztes Feld, nach allen Feldern, die der Aufrufer
//!    steuert; `hostname=` (`PAM_RHOST`) und `terminal=` (`PAM_TTY`) stehen
//!    davor und **unkodiert** im Text. Ein Client, der dort ` res=success`
//!    (oder ein offenes `"`) einschmuggelt, darf damit einen Fehlschlag
//!    weder umdrehen noch den Record zum verworfenen Formfehler machen.
//!    Deshalb wird zuerst das letzte Wort des Textes geprüft; nur wenn es
//!    kein `res=` ist (kein libaudit-Format), wird der Text tokenisiert und
//!    das letzte `res=`-Token genommen.
//!
//! # Ergebnis der Operation
//! [`AuditFields::success`] entsteht aus `success=yes|no` (`SYSCALL`), aus
//! einem äußeren `res=1|0` (Kernel-Records wie `LOGIN`) und aus dem
//! maßgeblichen `res=` in `msg='…'` (`res=success|failed`, PAM und sudo
//! über libaudit, Regel 3 oben). Widersprechen sich diese **vom Kernel
//! geformten bzw. maßgeblichen** Ergebnisse, ist der Record
//! [`crate::error::NetlinkError::MalformedRecord`] — ein widersprüchliches
//! Ergebnis wird nie still zugunsten einer Seite aufgelöst. Frühere `res=`
//! im Inneren von `msg='…'` zählen dabei nicht: sie stehen in Feldern, die
//! der Aufrufer steuert.
//!
//! # Entscheidung: keine Hex-Dekodierung
//! Audit-Records kodieren manche Textfelder (`exe`, `comm`, `path`, `cmd`
//! in `USER_CMD`, `proctitle`, …) hexadezimal, wenn ihr Wert Zeichen enthält,
//! die als `key=value`-Token nicht sicher darstellbar wären. **Diese Crate
//! dekodiert das nicht.** Keins der Felder in [`AuditFields`] kommt
//! hex-kodiert vor: die Recordart ist ein fester Bezeichner,
//! `auid`/`uid`/`pid` sind Dezimalzahlen, das Ergebnis ist ein festes Wort,
//! und der Zeitstempel steht unverschlüsselt im `audit(…)`. Ein hex-kodiertes
//! Token bei einem anderen Schlüssel (insbesondere die Kommandozeile `cmd=`
//! eines `USER_CMD`-Records) bleibt ein opaker Rohwert und wird nicht in
//! [`AuditFields`] übernommen — eine bewusste, dokumentierte Entscheidung,
//! kein offener Punkt.
//!
//! # Exportierte Typen
//! [`RawRecord`], [`AuditFields`], die freien Funktionen [`parse_record`] und
//! [`audit_type_name`].
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
//! // auditd-Textform.
//! let record = RawRecord::new(
//!     "type=SYSCALL msg=audit(1699999999.123:456): auid=1000 uid=0 pid=4242 success=yes",
//! );
//! let fields = parse_record(&record)?;
//! assert_eq!(fields.record_type, "SYSCALL");
//! assert_eq!(fields.auid, Some(1000));
//! assert_eq!(fields.uid, Some(0));
//! assert_eq!(fields.success, Some(true));
//!
//! // Kernel-Netlink-Nutzlast: der Typ (1100 = USER_AUTH) kommt aus dem nlmsghdr.
//! let record = RawRecord::from_netlink(
//!     1100,
//!     "audit(1699999999.456:457): pid=4300 uid=1000 auid=1000 ses=2 msg='op=PAM:authentication acct=\"root\" res=failed'",
//! );
//! let fields = parse_record(&record)?;
//! assert_eq!(fields.record_type, "USER_AUTH");
//! assert_eq!(fields.success, Some(false));
//! # Ok::<(), harw_dod_netlink::NetlinkError>(())
//! ```

use crate::error::NetlinkError;

/// Präfix des Zeitstempels: `audit(sekunden.millisekunden:seriennummer):`.
const AUDIT_STAMP_PREFIX: &str = "audit(";

/// Doppeltes Anführungszeichen: umschließt einen Wert bis zum nächsten `"`.
const DOUBLE_QUOTE: char = '"';

/// Einfaches Anführungszeichen: umschließt `msg='…'` bis zum letzten `'`.
const SINGLE_QUOTE: char = '\'';

/// Gruppentrennzeichen (`U+001D`): auditd `log_format = ENRICHED` trennt
/// damit die angereicherten Felder vom Kernel-Text.
const GROUP_SEPARATOR: char = '\u{1d}';

/// Stellenwerte der bis zu neun Nachkommastellen eines Zeitstempels in
/// Nanosekunden.
const FRACTION_SCALES: [i32; 9] = [
    100_000_000,
    10_000_000,
    1_000_000,
    100_000,
    10_000,
    1_000,
    100,
    10,
    1,
];

/// Bekannte Audit-Recordtypen: Nummer aus dem `nlmsghdr` → Name wie in der
/// auditd-Textform.
///
/// Die Kernel-Typen (`LOGIN`, `USER_AVC`, `USER_TTY`, `SYSCALL` … `AVC`)
/// stehen in `<linux/audit.h>`. Die Userspace-Typen 1100–1126 (`USER_AUTH`,
/// `USER_LOGIN`, `USER_CMD`, …) definiert libaudit (`libaudit.h`) im
/// Kernel-Bereich `AUDIT_FIRST_USER_MSG`..`AUDIT_LAST_USER_MSG`; der Kernel
/// reicht sie mit derselben Nummer an die Multicast-Gruppe weiter.
const AUDIT_TYPE_NAMES: [(u16, &str); 26] = [
    (1006, "LOGIN"),
    (1100, "USER_AUTH"),
    (1101, "USER_ACCT"),
    (1102, "USER_MGMT"),
    (1103, "CRED_ACQ"),
    (1104, "CRED_DISP"),
    (1105, "USER_START"),
    (1106, "USER_END"),
    (1107, "USER_AVC"),
    (1108, "USER_CHAUTHTOK"),
    (1109, "USER_ERR"),
    (1110, "CRED_REFR"),
    (1112, "USER_LOGIN"),
    (1113, "USER_LOGOUT"),
    (1123, "USER_CMD"),
    (1124, "USER_TTY"),
    (1126, "GRP_AUTH"),
    (1300, "SYSCALL"),
    (1302, "PATH"),
    (1305, "CONFIG_CHANGE"),
    (1307, "CWD"),
    (1309, "EXECVE"),
    (1320, "EOE"),
    (1326, "SECCOMP"),
    (1327, "PROCTITLE"),
    (1400, "AVC"),
];

/// Ein Audit-Record als Rohtext, wie er aus der auditd-Logdatei oder (nach
/// Rahmenzerlegung) aus dem Netlink-Socket kommt.
///
/// # Description
/// Trägt genau eine Zeile `key=value`-Text und optional die Recordart aus dem
/// `nlmsghdr`. Konstruktion ist unfehlbar — jede Zeichenkette ist ein
/// gültiges `RawRecord`; ob sie ein *wohlgeformter* Audit-Record ist,
/// entscheidet erst [`parse_record`].
///
/// # Concurrency
/// Reiner Werttyp: `Send + Sync`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawRecord {
    /// Der Rohtext.
    line: String,
    /// `nlmsg_type` des Netlink-Rahmens; `None` für die auditd-Textform.
    msg_type: Option<u16>,
}

impl RawRecord {
    /// Baut einen `RawRecord` aus einer Textzeile der auditd-Textform.
    ///
    /// # Arguments
    /// - `line` (`impl Into<String>`): der Rohtext, typischerweise eine
    ///   einzelne Audit-Zeile mit `type=`-Schlüssel.
    ///
    /// # Returns
    /// Einen `RawRecord` ohne Recordart aus dem Netlink-Kopf
    /// ([`Self::msg_type`] ist `None`), dessen [`Self::as_str`] exakt `line`
    /// zurückgibt.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_netlink::RawRecord;
    ///
    /// let record = RawRecord::new("type=SYSCALL");
    /// assert_eq!(record.as_str(), "type=SYSCALL");
    /// assert_eq!(record.msg_type(), None);
    /// ```
    #[must_use]
    pub fn new(line: impl Into<String>) -> Self {
        Self {
            line: line.into(),
            msg_type: None,
        }
    }

    /// Baut einen `RawRecord` aus der Nutzlast einer Netlink-Nachricht.
    ///
    /// # Description
    /// Die Kernel-Nutzlast trägt kein `type=`; die Recordart steht im
    /// `nlmsg_type` des Rahmens und wird hier mitgeführt. [`parse_record`]
    /// leitet die Recordart dann ausschließlich daraus ab.
    ///
    /// # Arguments
    /// - `msg_type` (`u16`): `nlmsg_type` des Netlink-Rahmens, z. B. `1100`
    ///   für `USER_AUTH`.
    /// - `payload` (`impl Into<String>`): der Nutzlasttext,
    ///   `audit(…): key=value …`.
    ///
    /// # Returns
    /// Einen `RawRecord`, dessen [`Self::msg_type`] `Some(msg_type)` ist.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_netlink::RawRecord;
    ///
    /// let record = RawRecord::from_netlink(1112, "audit(1.000:1): pid=1 uid=0");
    /// assert_eq!(record.msg_type(), Some(1112));
    /// ```
    #[must_use]
    pub fn from_netlink(msg_type: u16, payload: impl Into<String>) -> Self {
        Self {
            line: payload.into(),
            msg_type: Some(msg_type),
        }
    }

    /// Der rohe Zeilentext dieses Records.
    ///
    /// # Returns
    /// Den bei der Konstruktion übergebenen Text, unverändert.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.line
    }

    /// Die Recordart aus dem Netlink-Kopf, falls der Record von dort stammt.
    ///
    /// # Returns
    /// `Some(nlmsg_type)` für [`Self::from_netlink`], `None` für
    /// [`Self::new`] und die `From`-Konvertierungen.
    #[must_use]
    pub fn msg_type(&self) -> Option<u16> {
        self.msg_type
    }
}

impl From<String> for RawRecord {
    fn from(line: String) -> Self {
        Self::new(line)
    }
}

impl From<&str> for RawRecord {
    fn from(line: &str) -> Self {
        Self::new(line)
    }
}

/// Der Name einer Audit-Recordart, wie ihn die auditd-Textform schreibt.
///
/// # Description
/// Bildet die Zahl aus dem `nlmsghdr` auf den Bezeichner ab, den
/// [`AuditFields::record_type`] für die Textform trägt — so vergleicht ein
/// Konsument beide Eingabeformen über denselben Namen. Unbekannte Nummern
/// liefern `None`; [`parse_record`] setzt dann die Dezimalzahl als
/// Recordart ein.
///
/// # Arguments
/// - `msg_type` (`u16`): `nlmsg_type` eines Audit-Rahmens.
///
/// # Returns
/// Den Namen (z. B. `"USER_LOGIN"` für `1112`) oder `None`.
///
/// # Examples
/// ```rust
/// use harw_dod_netlink::audit_type_name;
///
/// assert_eq!(audit_type_name(1100), Some("USER_AUTH"));
/// assert_eq!(audit_type_name(1327), Some("PROCTITLE"));
/// assert_eq!(audit_type_name(9999), None);
/// ```
#[must_use]
pub fn audit_type_name(msg_type: u16) -> Option<&'static str> {
    AUDIT_TYPE_NAMES
        .iter()
        .find(|(code, _)| *code == msg_type)
        .map(|(_, name)| *name)
}

/// Die getypten Felder eines Audit-Records, die dieses Programm auswertet.
///
/// # Description
/// Nur diese Felder zählen für das Programm (siehe Crate-Dokumentation zur
/// Begründung, warum `auid` und nicht `uid` die entscheidende Kennung ist).
/// Jedes Feld außer `record_type` ist optional, weil nicht jeder
/// Audit-Record-Typ jedes Feld führt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditFields {
    /// Die Recordart, z. B. `"SYSCALL"` oder `"USER_AUTH"`. Stammt bei einem
    /// Netlink-Record aus dem `nlmsghdr` ([`audit_type_name`], sonst die
    /// Dezimalzahl), bei der Textform aus dem `type`-Schlüssel. Das einzige
    /// Pflichtfeld: fehlt es, liefert [`parse_record`]
    /// [`NetlinkError::MalformedRecord`].
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

    /// Ob die protokollierte Operation erfolgreich war: aus `success=yes|no`,
    /// einem äußeren `res=1|0` oder `res=success|failed` in `msg='…'` (siehe
    /// Moduldokumentation, Abschnitt „Ergebnis der Operation").
    pub success: Option<bool>,

    /// Der Zeitstempel aus `audit(sekunden.millisekunden:seriennummer):` —
    /// unquotiertes `msg=` der Textform oder erstes Token der
    /// Netlink-Nutzlast.
    pub timestamp: Option<jiff::Timestamp>,
}

/// Ein einzelnes Token einer Audit-Zeile.
enum Token<'a> {
    /// `schlüssel=wert`; `quote` ist das umschließende Anführungszeichen
    /// oder `None` für einen nackten Wert.
    Pair {
        /// Der Schlüssel.
        key: &'a str,
        /// Der Wert ohne Anführungszeichen.
        value: &'a str,
        /// Das umschließende Anführungszeichen, falls vorhanden.
        quote: Option<char>,
    },
    /// Ein Wort ohne `=`.
    Bare(&'a str),
}

/// Ob `c` zwei Token trennt (siehe Moduldokumentation, Grammatik).
fn is_separator(c: char) -> bool {
    c.is_ascii_whitespace() || c == GROUP_SEPARATOR
}

/// Zerlegt eine Audit-Zeile in Token.
///
/// Siehe die `key=value`-Grammatik in der Moduldokumentation.
///
/// # Errors
/// [`NetlinkError::MalformedRecord`] bei einem nicht geschlossenen
/// Anführungszeichen.
fn tokenize(line: &str) -> Result<Vec<Token<'_>>, NetlinkError> {
    let mut tokens = Vec::new();
    let mut rest = line.trim_start_matches(is_separator);

    while !rest.is_empty() {
        let word_end = rest
            .find(|c: char| c == '=' || is_separator(c))
            .unwrap_or(rest.len());
        let (word, after_word) = rest
            .split_at_checked(word_end)
            .ok_or(NetlinkError::MalformedRecord)?;

        let remainder = if let Some(after_eq) = after_word.strip_prefix('=') {
            let (value, quote, remainder) = split_value(after_eq)?;
            tokens.push(Token::Pair {
                key: word,
                value,
                quote,
            });
            remainder
        } else {
            tokens.push(Token::Bare(word));
            after_word
        };
        rest = remainder.trim_start_matches(is_separator);
    }

    Ok(tokens)
}

/// Trennt einen Wert vom Rest der Zeile.
///
/// # Returns
/// `(wert, anführungszeichen, rest)`. Ein in `"` gefasster Wert endet am
/// nächsten `"`, ein in `'` gefasster am **letzten** `'` der Zeile, ein
/// nackter am nächsten Trenner.
///
/// # Errors
/// [`NetlinkError::MalformedRecord`], wenn das schließende Anführungszeichen
/// fehlt.
fn split_value(input: &str) -> Result<(&str, Option<char>, &str), NetlinkError> {
    if let Some(quoted) = input.strip_prefix(DOUBLE_QUOTE) {
        let (value, after) = quoted
            .split_once(DOUBLE_QUOTE)
            .ok_or(NetlinkError::MalformedRecord)?;
        return Ok((value, Some(DOUBLE_QUOTE), after));
    }
    if let Some(quoted) = input.strip_prefix(SINGLE_QUOTE) {
        let (value, after) = quoted
            .rsplit_once(SINGLE_QUOTE)
            .ok_or(NetlinkError::MalformedRecord)?;
        return Ok((value, Some(SINGLE_QUOTE), after));
    }
    let end = input.find(is_separator).unwrap_or(input.len());
    let (value, after) = input
        .split_at_checked(end)
        .ok_or(NetlinkError::MalformedRecord)?;
    Ok((value, None, after))
}

/// Ob `text` nicht leer ist und nur aus ASCII-Ziffern besteht.
fn is_decimal(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}

/// Parst eine vorzeichenlose Dezimalzahl (`auid`, `uid`, `pid`).
///
/// # Errors
/// [`NetlinkError::MalformedRecord`] für leere Werte, Vorzeichen, andere
/// Zeichen als Ziffern oder einen Überlauf.
fn parse_decimal_u32(value: &str) -> Result<u32, NetlinkError> {
    if !is_decimal(value) {
        return Err(NetlinkError::MalformedRecord);
    }
    value.parse().map_err(|_| NetlinkError::MalformedRecord)
}

/// Parst die bis zu neun Nachkommastellen eines Zeitstempels in
/// Nanosekunden; weitere Stellen werden abgeschnitten.
///
/// # Errors
/// [`NetlinkError::MalformedRecord`] für andere Zeichen als Ziffern.
fn parse_fraction_nanos(fraction: &str) -> Result<i32, NetlinkError> {
    if fraction.is_empty() {
        return Ok(0);
    }
    if !is_decimal(fraction) {
        return Err(NetlinkError::MalformedRecord);
    }
    let mut nanos: i32 = 0;
    for (digit, scale) in fraction.bytes().zip(FRACTION_SCALES) {
        let value = i32::from(digit.wrapping_sub(b'0'));
        let part = value
            .checked_mul(scale)
            .ok_or(NetlinkError::MalformedRecord)?;
        nanos = nanos
            .checked_add(part)
            .ok_or(NetlinkError::MalformedRecord)?;
    }
    Ok(nanos)
}

/// Parst den Zeitstempel `audit(sekunden.millisekunden:seriennummer):`.
///
/// # Errors
/// [`NetlinkError::MalformedRecord`], wenn Präfix, Klammer, Doppelpunkt,
/// Sekunden, Nachkommastellen oder Seriennummer nicht der Form entsprechen
/// oder der Zeitpunkt außerhalb des Bereichs von `jiff::Timestamp` liegt.
fn parse_audit_timestamp(value: &str) -> Result<jiff::Timestamp, NetlinkError> {
    let after_prefix = value
        .strip_prefix(AUDIT_STAMP_PREFIX)
        .ok_or(NetlinkError::MalformedRecord)?;
    let (inner, _) = after_prefix
        .split_once(')')
        .ok_or(NetlinkError::MalformedRecord)?;
    let (seconds_part, serial) = inner.split_once(':').ok_or(NetlinkError::MalformedRecord)?;
    if !is_decimal(serial) {
        return Err(NetlinkError::MalformedRecord);
    }
    let (whole, fraction) = seconds_part.split_once('.').unwrap_or((seconds_part, ""));
    if !is_decimal(whole) {
        return Err(NetlinkError::MalformedRecord);
    }
    let seconds: i64 = whole.parse().map_err(|_| NetlinkError::MalformedRecord)?;
    let nanos = parse_fraction_nanos(fraction)?;
    jiff::Timestamp::new(seconds, nanos).map_err(|_| NetlinkError::MalformedRecord)
}

/// Parst `success=yes|no` (`SYSCALL`-Records).
///
/// # Errors
/// [`NetlinkError::MalformedRecord`] für jeden anderen Wert.
fn parse_success(value: &str) -> Result<bool, NetlinkError> {
    match value {
        "yes" => Ok(true),
        "no" => Ok(false),
        _ => Err(NetlinkError::MalformedRecord),
    }
}

/// Parst `res=`: `success`/`failed` (libaudit, PAM, sudo) oder `1`/`0`
/// (Kernel-Records wie `LOGIN`).
///
/// # Errors
/// [`NetlinkError::MalformedRecord`] für jeden anderen Wert.
fn parse_res(value: &str) -> Result<bool, NetlinkError> {
    match value {
        "success" | "1" => Ok(true),
        "failed" | "0" => Ok(false),
        _ => Err(NetlinkError::MalformedRecord),
    }
}

/// Führt ein weiteres Ergebnisfeld mit dem bisherigen zusammen.
///
/// # Errors
/// [`NetlinkError::MalformedRecord`], wenn die beiden Ergebnisse sich
/// widersprechen (siehe Moduldokumentation, „Ergebnis der Operation").
fn merge_outcome(current: Option<bool>, next: bool) -> Result<Option<bool>, NetlinkError> {
    match current {
        Some(existing) if existing != next => Err(NetlinkError::MalformedRecord),
        _ => Ok(Some(next)),
    }
}

/// Setzt den Zeitstempel genau einmal.
///
/// # Errors
/// [`NetlinkError::MalformedRecord`], wenn der Record bereits einen
/// Zeitstempel trägt.
fn set_timestamp_once(
    current: Option<jiff::Timestamp>,
    next: jiff::Timestamp,
) -> Result<Option<jiff::Timestamp>, NetlinkError> {
    match current {
        Some(_) => Err(NetlinkError::MalformedRecord),
        None => Ok(Some(next)),
    }
}

/// Liest das Ergebnis aus dem Text eines `msg='…'`-Felds.
///
/// # Description
/// Maßgeblich ist das **letzte** `res=` (siehe Moduldokumentation, „Das
/// `msg='…'`-Feld", Regel 3): ist das letzte Wort des Textes ein `res=…`,
/// gilt es, ohne dass der Rest tokenisiert wird — ein eingeschmuggeltes
/// `res=` oder offenes `"` in `hostname=`/`terminal=` ändert dann nichts.
/// Sonst wird der Text nach derselben Grammatik tokenisiert und das letzte
/// `res=`-Token gewertet; frühere `res=` werden ignoriert.
///
/// # Errors
/// [`NetlinkError::MalformedRecord`] für einen unbekannten Wert des
/// maßgeblichen `res=`, oder — nur ohne `res=` als letztes Wort — für ein
/// nicht geschlossenes Anführungszeichen.
fn inner_outcome(text: &str) -> Result<Option<bool>, NetlinkError> {
    let last_word = text
        .trim_end_matches(is_separator)
        .rsplit(is_separator)
        .next()
        .unwrap_or_default();
    if let Some(value) = last_word.strip_prefix("res=") {
        return parse_res(value).map(Some);
    }

    let mut last_res = None;
    for token in tokenize(text)? {
        if let Token::Pair {
            key: "res", value, ..
        } = token
        {
            last_res = Some(value);
        }
    }
    last_res.map(parse_res).transpose()
}

/// Parst einen [`RawRecord`] in [`AuditFields`].
///
/// # Description
/// Tokenisiert die Zeile (siehe Moduldokumentation) und liest daraus die
/// Recordart (aus dem `nlmsghdr` oder dem `type`-Schlüssel), `auid`, `uid`,
/// `pid`, das Ergebnis und den Zeitstempel. Werte in Anführungszeichen und
/// nackte Werte werden gleich behandelt. Der `auid`-Sentinelwert
/// `4294967295` wird zu `None` normalisiert — siehe [`AuditFields::auid`] für
/// die Begründung.
///
/// # Arguments
/// - `record` (`&RawRecord`): der zu parsende Rohtext.
///
/// # Returns
/// Die extrahierten [`AuditFields`].
///
/// # Errors
/// - [`NetlinkError::MalformedRecord`]: die Recordart fehlt, ein
///   Anführungszeichen ist nicht geschlossen, ein vorhandenes Feld (`auid`,
///   `uid`, `pid`, `success`, `res`, Zeitstempel) hat einen Wert, der nicht
///   dem erwarteten Format entspricht, der Record trägt zwei Zeitstempel,
///   oder seine Ergebnisfelder widersprechen sich. Die Fehlermeldung nennt
///   nie den Recordinhalt.
///
/// # Concurrency
/// Zustandslos; aus beliebig vielen Threads gleichzeitig aufrufbar.
///
/// # Examples
/// ```rust
/// use harw_dod_netlink::{RawRecord, parse_record};
///
/// // Der klassische auid-Sonderfall: 4294967295 heißt "nicht angemeldet".
/// let record = RawRecord::new("type=SYSCALL auid=4294967295 uid=0");
/// let fields = parse_record(&record)?;
/// assert_eq!(fields.auid, None);
/// # Ok::<(), harw_dod_netlink::NetlinkError>(())
/// ```
pub fn parse_record(record: &RawRecord) -> Result<AuditFields, NetlinkError> {
    let tokens = tokenize(record.as_str())?;

    let mut type_token: Option<&str> = None;
    let mut auid: Option<u32> = None;
    let mut uid: Option<u32> = None;
    let mut pid: Option<u32> = None;
    let mut success: Option<bool> = None;
    let mut timestamp: Option<jiff::Timestamp> = None;

    for (index, token) in tokens.iter().enumerate() {
        match *token {
            Token::Bare(word) => {
                // Nur das erste Wort kann der Zeitstempel der Netlink-Nutzlast
                // sein; ein späteres `audit(`-Wort ist gewöhnlicher Text.
                if index == 0 && word.starts_with(AUDIT_STAMP_PREFIX) {
                    timestamp = set_timestamp_once(timestamp, parse_audit_timestamp(word)?)?;
                }
            }
            Token::Pair { key, value, quote } => match key {
                "type" => type_token = Some(value),
                "auid" => {
                    let raw = parse_decimal_u32(value)?;
                    auid = (raw != u32::MAX).then_some(raw);
                }
                "uid" => uid = Some(parse_decimal_u32(value)?),
                "pid" => pid = Some(parse_decimal_u32(value)?),
                "success" => success = merge_outcome(success, parse_success(value)?)?,
                "res" => success = merge_outcome(success, parse_res(value)?)?,
                "msg" => match quote {
                    None if value.starts_with(AUDIT_STAMP_PREFIX) => {
                        timestamp = set_timestamp_once(timestamp, parse_audit_timestamp(value)?)?;
                    }
                    // Ein unquotiertes `msg=` ohne Zeitstempel trägt nichts,
                    // was dieses Programm auswertet.
                    None => {}
                    Some(_) => {
                        if let Some(inner) = inner_outcome(value)? {
                            success = merge_outcome(success, inner)?;
                        }
                    }
                },
                _ => {}
            },
        }
    }

    let record_type = match record.msg_type() {
        Some(code) => audit_type_name(code).map_or_else(|| code.to_string(), str::to_owned),
        None => type_token
            .filter(|name| !name.is_empty())
            .ok_or(NetlinkError::MalformedRecord)?
            .to_owned(),
    };

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
    use super::{RawRecord, audit_type_name, parse_record};
    use crate::test_support::{TestError, TestResult};

    /// Kernel-`LOGIN`-Record (1006): `auid` wird gesetzt, Ergebnis numerisch.
    const KERNEL_LOGIN: &str = "audit(1699999999.100:455): pid=4242 uid=0 subj=unconfined old-auid=4294967295 auid=1000 tty=(none) old-ses=4294967295 ses=2 res=1";

    /// `USER_LOGIN` (1112) von sshd über libaudit, erfolgreich.
    const KERNEL_USER_LOGIN: &str = "audit(1699999999.123:456): pid=4242 uid=0 auid=1000 ses=2 subj=unconfined msg='op=login id=1000 exe=\"/usr/sbin/sshd\" hostname=? addr=192.0.2.10 terminal=/dev/pts/0 res=success'";

    /// `USER_AUTH` (1100) von sudo über PAM, gescheitert.
    const KERNEL_USER_AUTH_FAILED: &str = "audit(1699999999.456:457): pid=4300 uid=1000 auid=1000 ses=2 subj=unconfined msg='op=PAM:authentication grantors=? acct=\"root\" exe=\"/usr/bin/sudo\" hostname=? addr=? terminal=/dev/pts/0 res=failed'";

    /// `USER_CMD` (1123) von sudo; `cmd` ist hex-kodiert und bleibt es.
    const KERNEL_USER_CMD: &str = "audit(1699999999.789:458): pid=4400 uid=1000 auid=1000 ses=2 subj=unconfined msg='cwd=\"/home/alice\" cmd=6C73202D6C61 exe=\"/usr/bin/sudo\" terminal=pts/0 res=success'";

    /// Erwartet `Err(MalformedRecord)` und prüft, dass die Meldung keinen
    /// Recordinhalt zitiert.
    fn expect_malformed(record: &RawRecord, forbidden: &[&str]) -> TestResult {
        let Err(err) = parse_record(record) else {
            return Err(TestError::Unexpected(
                "Err(MalformedRecord) erwartet".into(),
            ));
        };
        let message = err.to_string();
        assert_eq!(message, "audit record is malformed");
        for needle in forbidden {
            assert!(!message.contains(needle), "Meldung zitiert {needle:?}");
        }
        Ok(())
    }

    #[test]
    fn test_parse_record_full_record_extracts_all_fields() -> TestResult {
        let record = RawRecord::new(
            "type=SYSCALL msg=audit(1699999999.123:456): auid=1000 uid=0 pid=4242 success=yes",
        );
        let fields = parse_record(&record)?;

        assert_eq!(fields.record_type, "SYSCALL");
        assert_eq!(fields.auid, Some(1000));
        assert_eq!(fields.uid, Some(0));
        assert_eq!(fields.pid, Some(4242));
        assert_eq!(fields.success, Some(true));
        let ts = fields.timestamp.ok_or(TestError::Missing("Zeitstempel"))?;
        assert_eq!(ts.as_second(), 1_699_999_999);
        assert_eq!(ts.subsec_nanosecond(), 123_000_000);
        Ok(())
    }

    #[test]
    fn test_parse_record_auid_sentinel_max_value_is_none() -> TestResult {
        // Der wichtigste Test dieser Crate: 4294967295 (u32::MAX, -1 als u32)
        // heißt "keine Anmelde-UID gesetzt" und muss None ergeben, nicht
        // Some(4294967295).
        let record = RawRecord::new("type=SYSCALL auid=4294967295 uid=0");
        let fields = parse_record(&record)?;
        assert_eq!(fields.auid, None);
        Ok(())
    }

    #[test]
    fn test_parse_record_auid_zero_is_some_zero() -> TestResult {
        // Abgrenzung zum Sentinelwert: auid=0 ist root, der sich tatsächlich
        // angemeldet hat, kein "nicht gesetzt".
        let record = RawRecord::new("type=SYSCALL auid=0 uid=0");
        let fields = parse_record(&record)?;
        assert_eq!(fields.auid, Some(0));
        Ok(())
    }

    #[test]
    fn test_parse_record_quoted_value_is_unpacked() -> TestResult {
        let record = RawRecord::new(r#"type=SYSCALL success="yes" auid=1000"#);
        let fields = parse_record(&record)?;
        assert_eq!(fields.success, Some(true));
        Ok(())
    }

    #[test]
    fn test_parse_record_malformed_record_error_message_has_no_record_content() -> TestResult {
        let record = RawRecord::new("type=SYSCALL auid=not-a-number");
        expect_malformed(&record, &["not-a-number", "SYSCALL"])
    }

    #[test]
    fn test_parse_record_missing_required_type_field_is_detected() -> TestResult {
        let record = RawRecord::new("auid=1000 uid=0 pid=4242");
        expect_malformed(&record, &[])
    }

    #[test]
    fn test_parse_record_unrelated_hex_encoded_field_is_ignored() -> TestResult {
        // exe ist hex-kodiert (siehe Moduldokumentation: bewusst nicht
        // dekodiert), stört aber die Extraktion der bekannten Felder nicht.
        let record = RawRecord::new("type=SYSCALL exe=2F62696E2F62617368 auid=1000");
        let fields = parse_record(&record)?;
        assert_eq!(fields.auid, Some(1000));
        Ok(())
    }

    #[test]
    fn test_raw_record_as_str_roundtrips() {
        let record = RawRecord::new("type=SYSCALL");
        assert_eq!(record.as_str(), "type=SYSCALL");
        assert_eq!(record.msg_type(), None);
    }

    #[test]
    fn test_raw_record_from_netlink_keeps_type_and_payload() {
        let record = RawRecord::from_netlink(1112, "audit(1.000:1): uid=0");
        assert_eq!(record.as_str(), "audit(1.000:1): uid=0");
        assert_eq!(record.msg_type(), Some(1112));
        assert_ne!(record, RawRecord::new("audit(1.000:1): uid=0"));
    }

    #[test]
    fn test_audit_type_name_maps_login_relevant_types() {
        assert_eq!(audit_type_name(1006), Some("LOGIN"));
        assert_eq!(audit_type_name(1100), Some("USER_AUTH"));
        assert_eq!(audit_type_name(1101), Some("USER_ACCT"));
        assert_eq!(audit_type_name(1112), Some("USER_LOGIN"));
        assert_eq!(audit_type_name(1123), Some("USER_CMD"));
        assert_eq!(audit_type_name(1124), Some("USER_TTY"));
        assert_eq!(audit_type_name(1300), Some("SYSCALL"));
        assert_eq!(audit_type_name(1327), Some("PROCTITLE"));
        assert_eq!(audit_type_name(0), None);
        assert_eq!(audit_type_name(u16::MAX), None);
    }

    #[test]
    fn test_parse_record_netlink_user_login_payload() -> TestResult {
        let fields = parse_record(&RawRecord::from_netlink(1112, KERNEL_USER_LOGIN))?;
        assert_eq!(fields.record_type, "USER_LOGIN");
        assert_eq!(fields.pid, Some(4242));
        assert_eq!(fields.uid, Some(0));
        assert_eq!(fields.auid, Some(1000));
        assert_eq!(fields.success, Some(true));
        let ts = fields.timestamp.ok_or(TestError::Missing("Zeitstempel"))?;
        assert_eq!(ts.as_second(), 1_699_999_999);
        assert_eq!(ts.subsec_nanosecond(), 123_000_000);
        Ok(())
    }

    #[test]
    fn test_parse_record_netlink_user_auth_failure_reads_res_inside_msg() -> TestResult {
        let fields = parse_record(&RawRecord::from_netlink(1100, KERNEL_USER_AUTH_FAILED))?;
        assert_eq!(fields.record_type, "USER_AUTH");
        assert_eq!(fields.pid, Some(4300));
        assert_eq!(fields.uid, Some(1000));
        assert_eq!(fields.auid, Some(1000));
        assert_eq!(fields.success, Some(false));
        Ok(())
    }

    #[test]
    fn test_parse_record_netlink_user_cmd_keeps_hex_command_opaque() -> TestResult {
        // sudo meldet die Kommandozeile hex-kodiert (`cmd=…`). Sie wird nicht
        // dekodiert und erscheint in keinem Feld; nur Typ, Ids und Ergebnis.
        let fields = parse_record(&RawRecord::from_netlink(1123, KERNEL_USER_CMD))?;
        assert_eq!(fields.record_type, "USER_CMD");
        assert_eq!(fields.pid, Some(4400));
        assert_eq!(fields.uid, Some(1000));
        assert_eq!(fields.auid, Some(1000));
        assert_eq!(fields.success, Some(true));
        Ok(())
    }

    #[test]
    fn test_parse_record_netlink_kernel_login_numeric_res() -> TestResult {
        let fields = parse_record(&RawRecord::from_netlink(1006, KERNEL_LOGIN))?;
        assert_eq!(fields.record_type, "LOGIN");
        assert_eq!(fields.auid, Some(1000));
        assert_eq!(fields.success, Some(true));
        Ok(())
    }

    #[test]
    fn test_parse_record_netlink_unknown_type_uses_decimal_name() -> TestResult {
        let fields = parse_record(&RawRecord::from_netlink(
            2_999,
            "audit(1699999999.000:1): pid=1 uid=0",
        ))?;
        assert_eq!(fields.record_type, "2999");
        Ok(())
    }

    #[test]
    fn test_parse_record_netlink_type_comes_from_header_not_payload() -> TestResult {
        // Der Kopf setzt der Kernel; ein `type=` in der Nutzlast überstimmt
        // ihn nie.
        let fields = parse_record(&RawRecord::from_netlink(
            1300,
            "audit(1699999999.000:1): type=USER_LOGIN uid=0 success=yes",
        ))?;
        assert_eq!(fields.record_type, "SYSCALL");
        Ok(())
    }

    #[test]
    fn test_parse_record_auditd_text_user_auth_with_second_msg_key() -> TestResult {
        // Die echte auditd-Textform trägt `msg=` zweimal: erst den
        // Zeitstempel, dann den Text des meldenden Programms.
        let record = RawRecord::new(
            "type=USER_AUTH msg=audit(1699999999.456:457): pid=4300 uid=0 auid=1000 ses=2 msg='op=PAM:authentication grantors=? acct=\"root\" exe=\"/usr/bin/sudo\" hostname=? addr=? terminal=/dev/pts/0 res=failed'",
        );
        let fields = parse_record(&record)?;
        assert_eq!(fields.record_type, "USER_AUTH");
        assert_eq!(fields.success, Some(false));
        assert_eq!(fields.auid, Some(1000));
        let ts = fields.timestamp.ok_or(TestError::Missing("Zeitstempel"))?;
        assert_eq!(ts.as_second(), 1_699_999_999);
        Ok(())
    }

    #[test]
    fn test_parse_record_auditd_enriched_group_separator_is_a_separator() -> TestResult {
        // auditd `log_format = ENRICHED` trennt angereicherte Felder mit U+001D.
        let record = RawRecord::new(
            "type=USER_LOGIN msg=audit(1699999999.123:456): pid=4242 uid=0 auid=1000 ses=2 msg='op=login id=1000 res=success'\u{1d}UID=\"root\" AUID=\"alice\"",
        );
        let fields = parse_record(&record)?;
        assert_eq!(fields.record_type, "USER_LOGIN");
        assert_eq!(fields.uid, Some(0));
        assert_eq!(fields.success, Some(true));
        Ok(())
    }

    #[test]
    fn test_parse_record_single_quote_inside_msg_does_not_leak_outer_fields() -> TestResult {
        // Ein `'` im Text des meldenden Programms beendet `msg` nicht: das
        // eingeschmuggelte `auid=0` bleibt innen und wird nie gelesen.
        let record = RawRecord::from_netlink(
            1100,
            "audit(1699999999.456:457): pid=4300 uid=1000 auid=1000 ses=2 msg='op=PAM:authentication acct=\"x' auid=0 '\" res=failed'",
        );
        let fields = parse_record(&record)?;
        assert_eq!(fields.auid, Some(1000));
        assert_eq!(fields.success, Some(false));
        Ok(())
    }

    #[test]
    fn test_parse_record_inner_auid_and_uid_are_never_read() -> TestResult {
        let record = RawRecord::from_netlink(
            1112,
            "audit(1699999999.123:456): pid=4242 uid=0 auid=1000 msg='op=login uid=7 auid=7 pid=7 res=success'",
        );
        let fields = parse_record(&record)?;
        assert_eq!(fields.uid, Some(0));
        assert_eq!(fields.auid, Some(1000));
        assert_eq!(fields.pid, Some(4242));
        Ok(())
    }

    #[test]
    fn test_parse_record_contradicting_outcomes_are_malformed() -> TestResult {
        let record = RawRecord::from_netlink(
            1112,
            "audit(1699999999.123:456): pid=4242 uid=0 auid=1000 res=1 msg='op=login res=failed'",
        );
        expect_malformed(&record, &["login", "failed"])
    }

    #[test]
    fn test_parse_record_agreeing_outcomes_are_accepted() -> TestResult {
        let record = RawRecord::from_netlink(
            1112,
            "audit(1699999999.123:456): pid=4242 uid=0 auid=1000 res=0 msg='op=login res=failed'",
        );
        assert_eq!(parse_record(&record)?.success, Some(false));
        Ok(())
    }

    #[test]
    fn test_parse_record_last_inner_res_is_authoritative() -> TestResult {
        // hostname= ist unkodiert und steht vor dem letzten res= (libaudit):
        // ein eingeschmuggeltes ` res=success` dreht den Fehlschlag nicht um
        // und macht den Record auch nicht fehlerhaft.
        let record = RawRecord::from_netlink(
            1100,
            "audit(1699999999.456:457): pid=4300 uid=0 auid=4294967295 ses=4294967295 msg='op=PAM:authentication grantors=? acct=\"root\" exe=\"/usr/sbin/sshd\" hostname=x res=success addr=192.0.2.7 terminal=ssh res=failed'",
        );
        assert_eq!(parse_record(&record)?.success, Some(false));

        // Umgekehrt: ein eingeschmuggeltes res=failed vor dem echten
        // res=success ist ebenfalls bedeutungslos.
        let record = RawRecord::from_netlink(
            1112,
            "audit(1699999999.123:456): pid=4242 uid=0 auid=1000 msg='op=login id=1000 exe=\"/usr/sbin/sshd\" hostname=x res=failed addr=? terminal=ssh res=success'",
        );
        assert_eq!(parse_record(&record)?.success, Some(true));
        Ok(())
    }

    #[test]
    fn test_parse_record_injected_garbage_before_last_res_is_ignored() -> TestResult {
        // Ein offenes `"` oder ein unbekannter res-Wert in einem vom
        // Aufrufer gesteuerten Feld darf den Record nicht zum Formfehler
        // machen: das letzte Wort ist das maßgebliche res=.
        for inner in [
            "op=login hostname=x\" terminal=ssh res=failed",
            "op=login hostname=x res=maybe terminal=ssh res=failed",
            "op=login terminal=pts/0 res=failed  ",
        ] {
            let record = RawRecord::from_netlink(
                1112,
                format!("audit(1699999999.123:456): pid=4242 uid=0 auid=1000 msg='{inner}'"),
            );
            assert_eq!(parse_record(&record)?.success, Some(false), "{inner}");
        }
        Ok(())
    }

    #[test]
    fn test_parse_record_last_res_token_counts_when_not_the_last_word() -> TestResult {
        // Kein libaudit-Format (res= nicht am Ende): das letzte res=-Token
        // zählt, frühere werden ignoriert.
        let record = RawRecord::from_netlink(
            1112,
            "audit(1699999999.123:456): pid=4242 uid=0 auid=1000 msg='res=success op=login res=failed id=1000'",
        );
        assert_eq!(parse_record(&record)?.success, Some(false));
        Ok(())
    }

    #[test]
    fn test_parse_record_unknown_last_res_value_is_malformed_even_after_a_valid_one() -> TestResult
    {
        let record = RawRecord::from_netlink(
            1112,
            "audit(1699999999.123:456): pid=4242 uid=0 auid=1000 msg='op=login res=success res=maybe'",
        );
        expect_malformed(&record, &["maybe", "login"])
    }

    #[test]
    fn test_parse_record_unknown_res_value_is_malformed() -> TestResult {
        let record = RawRecord::from_netlink(
            1100,
            "audit(1699999999.456:457): pid=4300 uid=1000 auid=1000 msg='op=PAM:authentication res=maybe'",
        );
        expect_malformed(&record, &["maybe", "PAM"])
    }

    #[test]
    fn test_parse_record_non_numeric_timestamp_is_malformed() -> TestResult {
        let record = RawRecord::from_netlink(1100, "audit(notanumber.1:2): pid=1");
        expect_malformed(&record, &["notanumber"])
    }

    #[test]
    fn test_parse_record_non_numeric_serial_is_malformed() -> TestResult {
        let record = RawRecord::from_netlink(1100, "audit(1699999999.456:x7): pid=1");
        expect_malformed(&record, &["x7"])
    }

    #[test]
    fn test_parse_record_unterminated_single_quote_is_malformed() -> TestResult {
        let record = RawRecord::from_netlink(
            1100,
            "audit(1699999999.456:457): pid=4300 uid=0 auid=1000 msg='op=PAM acct=\"x",
        );
        expect_malformed(&record, &["PAM", "acct"])
    }

    #[test]
    fn test_parse_record_unterminated_double_quote_is_malformed() -> TestResult {
        let record = RawRecord::new("type=USER_AUTH acct=\"alice uid=0");
        expect_malformed(&record, &["alice"])
    }

    #[test]
    fn test_parse_record_non_numeric_auid_in_netlink_payload_is_malformed() -> TestResult {
        let record = RawRecord::from_netlink(1112, "audit(1699999999.123:456): pid=1 auid=abc");
        expect_malformed(&record, &["abc"])
    }

    #[test]
    fn test_parse_record_signed_uid_is_malformed() -> TestResult {
        let record = RawRecord::new("type=SYSCALL uid=-1");
        expect_malformed(&record, &["-1"])
    }

    #[test]
    fn test_parse_record_bare_words_without_equals_are_skipped() -> TestResult {
        // AVC-artige Wörter ohne `=` tragen nichts und lassen den Record
        // nicht scheitern.
        let record =
            RawRecord::new("type=AVC msg=audit(1699999999.000:9): avc: denied { read } for pid=7");
        let fields = parse_record(&record)?;
        assert_eq!(fields.record_type, "AVC");
        assert_eq!(fields.pid, Some(7));
        Ok(())
    }

    #[test]
    fn test_parse_record_late_bare_audit_word_is_not_a_timestamp() -> TestResult {
        let record = RawRecord::from_netlink(1112, "pid=1 audit(1699999999.123:456): uid=0");
        let fields = parse_record(&record)?;
        assert_eq!(fields.timestamp, None);
        Ok(())
    }

    #[test]
    fn test_parse_record_two_timestamps_are_malformed() -> TestResult {
        let record = RawRecord::from_netlink(
            1112,
            "audit(1699999999.123:456): pid=1 msg=audit(1700000000.000:1): uid=0",
        );
        expect_malformed(&record, &["1700000000"])
    }

    #[test]
    fn test_parse_record_quoted_msg_starting_with_audit_is_not_a_timestamp() -> TestResult {
        // Ein meldendes Programm kann seinen Text mit `audit(` beginnen; als
        // quotierter Wert ist er nie ein Zeitstempel.
        let record = RawRecord::from_netlink(
            1112,
            "audit(1699999999.123:456): pid=1 uid=0 msg='audit(1.000:1): res=success'",
        );
        let fields = parse_record(&record)?;
        let ts = fields.timestamp.ok_or(TestError::Missing("Zeitstempel"))?;
        assert_eq!(ts.as_second(), 1_699_999_999);
        Ok(())
    }

    #[test]
    fn test_parse_record_netlink_payload_without_timestamp_has_none() -> TestResult {
        let fields = parse_record(&RawRecord::from_netlink(1112, "pid=1 uid=0 res=success"))?;
        assert_eq!(fields.timestamp, None);
        assert_eq!(fields.success, Some(true));
        Ok(())
    }

    #[test]
    fn test_parse_record_empty_and_whitespace_lines_have_no_type() -> TestResult {
        expect_malformed(&RawRecord::new(""), &[])?;
        expect_malformed(&RawRecord::new(" \t \u{1d} "), &[])
    }

    #[test]
    fn test_parse_record_empty_type_value_is_malformed() -> TestResult {
        expect_malformed(&RawRecord::new("type= uid=0"), &[])
    }

    #[test]
    fn test_parse_record_netlink_empty_payload_still_has_type() -> TestResult {
        let fields = parse_record(&RawRecord::from_netlink(1320, ""))?;
        assert_eq!(fields.record_type, "EOE");
        assert_eq!(fields.timestamp, None);
        Ok(())
    }

    #[test]
    fn test_parse_record_fraction_digits_are_scaled_to_nanoseconds() -> TestResult {
        let fields = parse_record(&RawRecord::from_netlink(1112, "audit(10.5:1): uid=0"))?;
        let ts = fields.timestamp.ok_or(TestError::Missing("Zeitstempel"))?;
        assert_eq!(ts.subsec_nanosecond(), 500_000_000);

        let fields = parse_record(&RawRecord::from_netlink(
            1112,
            "audit(10.1234567891:1): uid=0",
        ))?;
        let ts = fields.timestamp.ok_or(TestError::Missing("Zeitstempel"))?;
        assert_eq!(ts.subsec_nanosecond(), 123_456_789);
        Ok(())
    }
}
