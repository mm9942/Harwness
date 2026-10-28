//! Durabler Marker für nicht verbuchte Job-Usage (Review P1 zu #151).
//!
//! # Description
//! Der Job-Worker persistiert die verbrauchte Usage eines laufenden Jobs über
//! `JobStore::record_usage`. Schlägt dieser Schreibzugriff aus einem anderen
//! Grund als einem Lease-Verlust fehl (Platte voll, Dateisystemfehler, …), ist
//! der Verbrauch real, aber nicht im Job-Datensatz: Stirbt der Worker danach,
//! lädt der Reclaim die ältere `job.usage` und gäbe dasselbe Budget ein
//! zweites Mal frei (`INTENT_CYCLE_MAX_ATTEMPTS > 1`).
//!
//! Dieses Modul legt für genau diesen Fall einen eigenen, kleinen Marker neben
//! dem Job-Store ab (`<job-root>/usage-unresolved/<work-id>.json`). Der
//! nächste Claim verrechnet den Marker zuerst mit der gespeicherten Usage und
//! entfernt ihn erst danach; lässt er sich nicht lesen oder verrechnen, endet
//! der Job fehlgeschlagen statt mit frischem Budget weiterzulaufen.
//!
//! # Rundenreservierung (Review B2 zu #151)
//! Der Marker oben hilft nur, wenn er sich schreiben lässt. Scheitern
//! `record_usage` UND der Marker (volles oder schreibgeschütztes
//! Job-Dateisystem) und stirbt der Worker danach, sähe ein Reclaim nur die
//! ältere Usage. Deshalb legt der Worker VOR jeder Modell-Runde eine durable
//! Reservierung ab ([`InFlightUsage`], `<job-root>/usage-inflight/<work-id>.json`)
//! mit dem Stand, von dem die Runde ausgeht (`usage_before`). Lässt sich die
//! Reservierung nicht schreiben, startet die Runde nicht — es wurde nichts
//! ausgegeben. Nach dem Festschreiben der Usage wird sie entfernt; laufen
//! mehrere Runden zugleich, bleibt sie liegen, bis keine mehr offen ist, und
//! wird vor jedem Festschreiben auf den festzuschreibenden Stand angehoben.
//! Eine Runde, deren gemeldete Usage der Ledger nicht verbuchen durfte
//! (Budget überschritten, Ledger geschlossen), bleibt reserviert.
//!
//! Konservative Regel beim Reclaim (kein Reclaim, solange die Buchhaltung
//! nicht bewiesen ist): Findet der nächste Claim (höhere Epoche) eine
//! Reservierung, gilt deren Runde nur dann als verbucht, wenn die — nach dem
//! Marker oben verrechnete — gespeicherte Usage in Tokens oder
//! Werkzeugaufrufen echt über `usage_before` liegt
//! ([`InFlightUsage::superseded_by`]); dann wird die Reservierung entfernt.
//! Sonst ist der Verbrauch der Runde unbewiesen, und der Job endet
//! fehlgeschlagen ohne Modellaufruf, statt dasselbe Budget ein zweites Mal
//! auszugeben. Eine Reservierung derselben (oder einer jüngeren) Epoche gehört
//! nicht zu einem früheren Versuch und bleibt unberührt. Werkzeugaufrufe
//! brauchen keine Reservierung: sie werden festgeschrieben, bevor sie laufen.
//!
//! Unbewiesener Verbrauch (Review P1 4394111159, 4394114784): Hat der Worker
//! den Provider einer Runde schon aufgerufen, beweist ein Fehler, Timeout oder
//! Abbruch danach nicht, dass nichts ausgegeben wurde (ein Stream kann Teile
//! geliefert haben, bevor er abriss). Die Reservierung trägt dann
//! [`InFlightUsage::unproven_spend`]; eine solche Reservierung gilt beim
//! Reclaim nie als verbucht, auch wenn die gespeicherte Usage inzwischen
//! darüber liegt, und der Worker entfernt sie nie.
//!
//! # Concurrency
//! Rundenreservierungen werden nur unter der Lease des Schreibers angelegt,
//! angehoben und entfernt: der Job-Worker hält dabei die Job-Sperre
//! (`JobStore::hold_live_lease` bzw. den Lease-Halt eines laufenden
//! Werkzeugaufrufs), dieselbe Sperre, die Claim, Reclaim und Ablauf nehmen
//! (Review P1 4394111166, P2 4394114795). Ein veralteter Schreiber wird so
//! abgewiesen, bevor er die Reservierung einer jüngeren Epoche überschreiben
//! oder entfernen kann; die Epochen-Prüfungen hier bleiben als zweite Linie.
//! Innerhalb eines Prozesses serialisiert zusätzlich der Flush-Gate des
//! Ledgers. Marker für nicht verbuchte Usage werden dagegen ohne Job-Sperre
//! geschrieben (sie entstehen gerade dann, wenn der Job-Store nicht
//! beschreibbar ist); sie werden nur feldweise vereinigt, nie verkleinert,
//! und nur entfernt, wenn eine festgeschriebene Usage sie abdeckt
//! ([`UsageMarkerStore::remove_if_covered`]). Jede einzelne Datei wird atomar
//! ersetzt (temp + rename + fsync des Verzeichnisses), ein Absturz
//! hinterlässt also nie eine halbe Marker-Datei.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use harw_job_runtime::BudgetUsage;
use serde::{Deserialize, Serialize};

/// Verzeichnisname unterhalb der Job-Store-Wurzel (`JobStore::root()`).
pub(crate) const USAGE_UNRESOLVED_DIR: &str = "usage-unresolved";

/// Verzeichnisname der Rundenreservierungen unterhalb der Job-Store-Wurzel
/// (Review B2 zu #151).
pub(crate) const USAGE_INFLIGHT_DIR: &str = "usage-inflight";

/// Aktuelle Schemaversion der Marker-Datei.
const SCHEMA_VERSION: u32 = 1;

/// Obergrenze für [`UnresolvedUsage::reason`] in Zeichen.
pub(crate) const MAX_REASON_CHARS: usize = 200;

/// Ersatztext, wenn nach dem Begrenzen kein Grund übrig bleibt.
const UNSPECIFIED_REASON: &str = "unspecified";

/// Endung einer Marker-Datei.
const MARKER_EXTENSION: &str = "json";

/// Endung einer unfertigen Temp-Datei.
const TEMP_SUFFIX: &str = ".tmp";

/// Dateimodus neuer Marker-Dateien (nur Eigentümer), wie der Job-Store.
#[cfg(unix)]
const FILE_MODE: u32 = 0o600;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Verbrauchte, aber nicht im Job-Datensatz festgeschriebene Usage eines Jobs.
///
/// # Description
/// `usage` ist ein absoluter Stand (Gesamtverbrauch des Jobs), kein Delta; so
/// lässt er sich mit der gespeicherten Usage per Feld-Maximum verrechnen
/// ([`UsageMarkerStore::merged`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UnresolvedUsage {
    /// Schemaversion der Datei; derzeit `1`.
    pub schema_version: u32,
    /// Job, zu dem der Verbrauch gehört.
    pub work_id: String,
    /// Lease-Epoche des Workers, der den Verbrauch gemeldet hat.
    pub epoch: u64,
    /// Absoluter Verbrauchsstand zum Zeitpunkt des fehlgeschlagenen Schreibens.
    pub usage: BudgetUsage,
    /// Zeitpunkt, zu dem der Marker geschrieben wurde.
    pub recorded_at: jiff::Timestamp,
    /// Kurzer Grund des fehlgeschlagenen Schreibens (begrenzt, keine Secrets).
    pub reason: String,
}

impl UnresolvedUsage {
    /// Baut einen Marker mit aktueller Schemaversion und Zeitstempel.
    ///
    /// # Description
    /// `reason` wird auf die erste Zeile ohne Steuerzeichen und höchstens
    /// [`MAX_REASON_CHARS`] Zeichen begrenzt; ein leerer Rest wird zu
    /// `"unspecified"`.
    pub(crate) fn new(work_id: &str, epoch: u64, usage: BudgetUsage, reason: &str) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            work_id: work_id.to_owned(),
            epoch,
            usage,
            recorded_at: jiff::Timestamp::now(),
            reason: bounded_reason(reason),
        }
    }
}

/// Durable Reservierung einer Modell-Runde, bevor sie etwas ausgibt (Review B2
/// zu #151).
///
/// # Description
/// `usage_before` ist der absolute Stand (Gesamtverbrauch des Jobs), von dem
/// die Runde ausgeht, kein Delta. Ein späterer Claim gilt die Runde nur dann
/// als verbucht, wenn die gespeicherte Usage darüber liegt
/// ([`Self::superseded_by`]); siehe Moduldoku, "Rundenreservierung".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InFlightUsage {
    /// Schemaversion der Datei; derzeit `1`.
    pub schema_version: u32,
    /// Job, zu dem die Runde gehört.
    pub work_id: String,
    /// Lease-Epoche des Workers, der die Runde reserviert hat.
    pub epoch: u64,
    /// Absoluter Verbrauchsstand, von dem die Runde ausgeht.
    pub usage_before: BudgetUsage,
    /// Zeitpunkt der Reservierung.
    pub started_at: jiff::Timestamp,
    /// `true`, sobald eine Runde dieses Versuchs nach dem Aufruf des
    /// Providers ohne verbuchte Usage endete (Fehler, Timeout, Abbruch oder
    /// nicht verbuchbare Usage): ihr Verbrauch ist unbewiesen, die
    /// Reservierung gilt nie als verbucht ([`Self::superseded_by`]). Fehlt in
    /// Dateien älterer Worker und gilt dann als `false`.
    #[serde(default)]
    pub unproven_spend: bool,
}

impl InFlightUsage {
    /// Baut eine Reservierung mit aktueller Schemaversion und Zeitstempel;
    /// ohne unbewiesenen Verbrauch.
    pub(crate) fn new(work_id: &str, epoch: u64, usage_before: BudgetUsage) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            work_id: work_id.to_owned(),
            epoch,
            usage_before,
            started_at: jiff::Timestamp::now(),
            unproven_spend: false,
        }
    }

    /// Dieselbe Reservierung mit gesetztem bzw. gelöschtem
    /// [`Self::unproven_spend`].
    #[must_use]
    pub(crate) fn with_unproven_spend(mut self, unproven_spend: bool) -> Self {
        self.unproven_spend = unproven_spend;
        self
    }

    /// `true`, wenn `stored` in Tokens oder Werkzeugaufrufen echt über
    /// `usage_before` liegt und die Reservierung keinen unbewiesenen
    /// Verbrauch trägt: dann wurde nach der Reservierung verbucht.
    ///
    /// # Description
    /// Die Wanduhr zählt bewusst nicht — sie wächst auch ohne Runde (der
    /// periodische Flush schreibt sie fort) und bewiese nichts. Eine Runde,
    /// die weder Tokens noch Werkzeugaufrufe meldet, gilt danach nie als
    /// bewiesen (fail closed). Mit [`Self::unproven_spend`] beweist keine
    /// gespeicherte Usage etwas: die unverbuchte Runde steckt in keinem Stand.
    #[must_use]
    pub(crate) fn superseded_by(&self, stored: &BudgetUsage) -> bool {
        !self.unproven_spend
            && (stored.tokens > self.usage_before.tokens
                || stored.tool_calls > self.usage_before.tool_calls)
    }
}

/// Dateibasierter Speicher der Marker unter `<job-root>/usage-unresolved/`
/// und der Rundenreservierungen unter `<job-root>/usage-inflight/`.
#[derive(Debug, Clone)]
pub(crate) struct UsageMarkerStore {
    dir: PathBuf,
    in_flight_dir: PathBuf,
}

impl UsageMarkerStore {
    /// Marker-Speicher unterhalb der Job-Store-Wurzel `job_root`
    /// (`JobStore::root()`); legt noch nichts auf der Platte an.
    pub(crate) fn in_job_root(job_root: &Path) -> Self {
        Self {
            dir: job_root.join(USAGE_UNRESOLVED_DIR),
            in_flight_dir: job_root.join(USAGE_INFLIGHT_DIR),
        }
    }

    /// Schreibt den Marker atomar (temp + rename + fsync des Verzeichnisses);
    /// überschreibt nur mit größerer oder gleicher Usage (max pro Feld).
    ///
    /// # Description
    /// Existiert bereits ein Marker für dieselbe Arbeits-ID, wird dessen
    /// Usage feldweise mit der neuen vereinigt ([`Self::merged`]); die
    /// gespeicherte Usage wird also nie kleiner. Trägt der vorhandene Marker
    /// eine höhere Epoche, behält er Epoche, Grund und Zeitstempel (ein
    /// verspäteter Schreiber einer alten Lease darf die Herkunft nicht
    /// zurückdrehen). Der Grund wird vor dem Schreiben begrenzt.
    ///
    /// # Errors
    /// Unsichere Arbeits-ID, unbekannte Schemaversion, ein vorhandener, aber
    /// unlesbarer Marker (er wird nicht überschrieben — der nächste Claim
    /// schlägt an ihm ohnehin geschlossen fehl) oder jeder I/O-Fehler.
    pub(crate) fn write(&self, marker: &UnresolvedUsage) -> Result<(), String> {
        let path = self.path_for(&marker.work_id)?;
        if marker.schema_version != SCHEMA_VERSION {
            return Err(format!(
                "usage marker: unsupported schema version {} (expected {SCHEMA_VERSION})",
                marker.schema_version
            ));
        }
        let mut next = marker.clone();
        next.reason = bounded_reason(&marker.reason);
        if let Some(existing) = self.read(&marker.work_id)? {
            next.usage = Self::merged(&existing.usage, &next.usage);
            if existing.epoch > next.epoch {
                next.epoch = existing.epoch;
                next.reason = existing.reason;
                next.recorded_at = existing.recorded_at;
            }
        }
        let bytes = serde_json::to_vec(&next)
            .map_err(|error| format!("usage marker: encoding failed: {error}"))?;
        std::fs::create_dir_all(&self.dir).map_err(|error| {
            format!(
                "usage marker: cannot create '{}': {error}",
                self.dir.display()
            )
        })?;
        // Das frisch angelegte Verzeichnis selbst muss im Elternverzeichnis
        // dauerhaft sein, sonst kann ein Absturz den ganzen Marker verlieren.
        if let Some(parent) = self.dir.parent() {
            sync_dir(parent)?;
        }
        write_atomic(&self.dir, &path, &marker.work_id, &bytes)
    }

    /// Liest den Marker zu `work_id`.
    ///
    /// # Returns
    /// `Ok(None)`, wenn kein Marker existiert; sonst der Marker.
    ///
    /// # Errors
    /// Fail-closed: unsichere Arbeits-ID, ein Symlink oder Nicht-Datei an der
    /// Stelle des Markers, nicht lesbare Datei, ungültiges JSON, unbekannte
    /// Felder oder Schemaversion, oder eine Marker-Datei, die zu einer anderen
    /// Arbeits-ID gehört. Ein kaputter Marker darf nie wie "kein Marker"
    /// aussehen, sonst bekäme der Job frisches Budget.
    pub(crate) fn read(&self, work_id: &str) -> Result<Option<UnresolvedUsage>, String> {
        let path = self.path_for(work_id)?;
        let Some(bytes) = read_regular_file(&path, MARKER_LABEL, work_id)? else {
            return Ok(None);
        };
        let marker: UnresolvedUsage = serde_json::from_slice(&bytes)
            .map_err(|error| format!("usage marker for '{work_id}' is malformed: {error}"))?;
        if marker.schema_version != SCHEMA_VERSION {
            return Err(format!(
                "usage marker for '{work_id}' has unsupported schema version {}",
                marker.schema_version
            ));
        }
        if marker.work_id != work_id {
            return Err(format!(
                "usage marker for '{work_id}' belongs to another job"
            ));
        }
        Ok(Some(marker))
    }

    /// Entfernt den Marker zu `work_id`; idempotent (fehlender Marker ist `Ok`).
    ///
    /// # Errors
    /// Unsichere Arbeits-ID oder ein I/O-Fehler beim Löschen bzw. beim
    /// fsync des Verzeichnisses. Ein verbliebener Marker ist unschädlich: er
    /// wird beim nächsten Claim erneut per Maximum verrechnet.
    pub(crate) fn remove(&self, work_id: &str) -> Result<(), String> {
        let path = self.path_for(work_id)?;
        match std::fs::remove_file(&path) {
            Ok(()) => sync_dir(&self.dir),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!(
                "usage marker for '{work_id}' cannot be removed: {error}"
            )),
        }
    }

    /// Entfernt den Marker zu `work_id`, sofern `covered_by` ihn feldweise
    /// abdeckt (Tokens, Wanduhr und Werkzeugaufrufe jeweils höchstens so groß).
    ///
    /// # Description
    /// Für Aufrufer, die eben `covered_by` festgeschrieben haben: ein Marker,
    /// der seither (ohne Job-Sperre, siehe Moduldoku) um weiteren Verbrauch
    /// gewachsen ist, bleibt liegen, statt mit dem Löschen verloren zu gehen.
    ///
    /// # Returns
    /// `Ok(None)`, wenn danach kein Marker mehr liegt (entfernt oder nie
    /// vorhanden); `Ok(Some(usage))` mit dessen Usage, wenn ein nicht
    /// abgedeckter Marker liegen bleibt.
    ///
    /// # Errors
    /// Wie [`Self::read`] und [`Self::remove`]; ein unlesbarer Marker bleibt
    /// liegen.
    pub(crate) fn remove_if_covered(
        &self,
        work_id: &str,
        covered_by: &BudgetUsage,
    ) -> Result<Option<BudgetUsage>, String> {
        let Some(marker) = self.read(work_id)? else {
            return Ok(None);
        };
        if Self::merged(&marker.usage, covered_by) != *covered_by {
            return Ok(Some(marker.usage));
        }
        self.remove(work_id)?;
        Ok(None)
    }

    /// Schreibt die Rundenreservierung atomar (temp + rename + fsync des
    /// Verzeichnisses); ersetzt eine vorhandene derselben oder einer älteren
    /// Epoche.
    ///
    /// # Errors
    /// Unsichere Arbeits-ID, unbekannte Schemaversion, eine vorhandene, aber
    /// unlesbare Reservierung (sie wird nicht überschrieben — der nächste
    /// Claim schlägt an ihr ohnehin geschlossen fehl), eine vorhandene
    /// Reservierung einer jüngeren Epoche (ein veralteter Lease-Halter darf
    /// die Reservierung des aktuellen nicht ersetzen) oder jeder I/O-Fehler.
    /// Der Aufrufer startet die Runde dann nicht.
    pub(crate) fn write_in_flight(&self, reservation: &InFlightUsage) -> Result<(), String> {
        let path = self.in_flight_path_for(&reservation.work_id)?;
        if reservation.schema_version != SCHEMA_VERSION {
            return Err(format!(
                "{IN_FLIGHT_LABEL}: unsupported schema version {} (expected {SCHEMA_VERSION})",
                reservation.schema_version
            ));
        }
        if let Some(existing) = self.read_in_flight(&reservation.work_id)? {
            if existing.epoch > reservation.epoch {
                return Err(format!(
                    "{IN_FLIGHT_LABEL} for '{}' is held by the newer epoch {} (writer epoch {})",
                    reservation.work_id, existing.epoch, reservation.epoch
                ));
            }
        }
        let bytes = serde_json::to_vec(reservation)
            .map_err(|error| format!("{IN_FLIGHT_LABEL}: encoding failed: {error}"))?;
        // Das Verzeichnis entsteht einmal je Job-Store; nur dann muss sein
        // Eintrag im Elternverzeichnis dauerhaft gemacht werden.
        let existed = self.in_flight_dir.is_dir();
        std::fs::create_dir_all(&self.in_flight_dir).map_err(|error| {
            format!(
                "{IN_FLIGHT_LABEL}: cannot create '{}': {error}",
                self.in_flight_dir.display()
            )
        })?;
        if !existed {
            if let Some(parent) = self.in_flight_dir.parent() {
                sync_dir(parent)?;
            }
        }
        write_atomic(&self.in_flight_dir, &path, &reservation.work_id, &bytes)
    }

    /// Liest die Rundenreservierung zu `work_id`.
    ///
    /// # Returns
    /// `Ok(None)`, wenn keine Reservierung existiert; sonst die Reservierung.
    ///
    /// # Errors
    /// Fail-closed wie [`Self::read`]: unsichere Arbeits-ID, Symlink oder
    /// Nicht-Datei, nicht lesbare Datei, ungültiges JSON, unbekannte Felder
    /// oder Schemaversion, oder eine Reservierung einer anderen Arbeits-ID.
    /// Eine kaputte Reservierung darf nie wie "keine Reservierung" aussehen.
    pub(crate) fn read_in_flight(&self, work_id: &str) -> Result<Option<InFlightUsage>, String> {
        let path = self.in_flight_path_for(work_id)?;
        let Some(bytes) = read_regular_file(&path, IN_FLIGHT_LABEL, work_id)? else {
            return Ok(None);
        };
        let reservation: InFlightUsage = serde_json::from_slice(&bytes)
            .map_err(|error| format!("{IN_FLIGHT_LABEL} for '{work_id}' is malformed: {error}"))?;
        if reservation.schema_version != SCHEMA_VERSION {
            return Err(format!(
                "{IN_FLIGHT_LABEL} for '{work_id}' has unsupported schema version {}",
                reservation.schema_version
            ));
        }
        if reservation.work_id != work_id {
            return Err(format!(
                "{IN_FLIGHT_LABEL} for '{work_id}' belongs to another job"
            ));
        }
        Ok(Some(reservation))
    }

    /// Entfernt die Rundenreservierung zu `work_id`, sofern sie aus Epoche
    /// `epoch` oder einer älteren stammt; idempotent.
    ///
    /// # Description
    /// Eine Reservierung einer jüngeren Epoche gehört dem aktuellen
    /// Lease-Halter und bleibt liegen (`Ok`). Kein eigener Dateilock: der
    /// Aufrufer hält die Job-Sperre unter seiner Lease (siehe Moduldoku) —
    /// der Lease-Halter, der eben festgeschrieben hat, oder der Claim, der die
    /// Reservierung eines früheren Versuchs als verbucht bewiesen hat. Die
    /// Epochen-Prüfung hier ist die zweite Linie.
    ///
    /// # Errors
    /// Unsichere Arbeits-ID, eine unlesbare Reservierung (sie bleibt liegen;
    /// der nächste Claim schlägt an ihr geschlossen fehl) oder ein I/O-Fehler
    /// beim Löschen bzw. beim fsync des Verzeichnisses.
    pub(crate) fn remove_in_flight(&self, work_id: &str, epoch: u64) -> Result<(), String> {
        let path = self.in_flight_path_for(work_id)?;
        match self.read_in_flight(work_id)? {
            None => return Ok(()),
            Some(existing) if existing.epoch > epoch => return Ok(()),
            Some(_) => {}
        }
        match std::fs::remove_file(&path) {
            Ok(()) => sync_dir(&self.in_flight_dir),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!(
                "{IN_FLIGHT_LABEL} for '{work_id}' cannot be removed: {error}"
            )),
        }
    }

    /// Feldweises Maximum zweier Usages.
    #[must_use]
    pub(crate) fn merged(a: &BudgetUsage, b: &BudgetUsage) -> BudgetUsage {
        BudgetUsage {
            tokens: a.tokens.max(b.tokens),
            wall: a.wall.max(b.wall),
            tool_calls: a.tool_calls.max(b.tool_calls),
        }
    }

    // Pfad der Marker-Datei; die Arbeits-ID muss ein sicherer einzelner
    // Pfadbestandteil sein (dieselbe Regel wie die Datensätze des Job-Stores).
    fn path_for(&self, work_id: &str) -> Result<PathBuf, String> {
        harw_job_store::validate_id(work_id).map_err(|error| format!("usage marker: {error}"))?;
        Ok(self.dir.join(format!("{work_id}.{MARKER_EXTENSION}")))
    }

    // Pfad der Reservierungsdatei; dieselbe Regel für die Arbeits-ID.
    fn in_flight_path_for(&self, work_id: &str) -> Result<PathBuf, String> {
        harw_job_store::validate_id(work_id)
            .map_err(|error| format!("{IN_FLIGHT_LABEL}: {error}"))?;
        Ok(self
            .in_flight_dir
            .join(format!("{work_id}.{MARKER_EXTENSION}")))
    }
}

/// Bezeichnung des Markers in Fehlertexten.
const MARKER_LABEL: &str = "usage marker";

/// Bezeichnung der Rundenreservierung in Fehlertexten.
const IN_FLIGHT_LABEL: &str = "in-flight usage reservation";

// Liest die Datei unter `path` fail-closed: `Ok(None)` nur, wenn sie fehlt;
// ein Symlink, eine Nicht-Datei oder ein Lesefehler ist ein Fehler. `label`
// benennt die Dateiart im Fehlertext.
fn read_regular_file(path: &Path, label: &str, work_id: &str) -> Result<Option<Vec<u8>>, String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(format!(
                "{label} for '{work_id}' must not be a symbolic link"
            ));
        }
        Ok(metadata) if !metadata.is_file() => {
            return Err(format!("{label} for '{work_id}' is not a regular file"));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "{label} for '{work_id}' is not accessible: {error}"
            ));
        }
    }
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("{label} for '{work_id}' cannot be read: {error}")),
    }
}

// temp + fsync + rename + fsync des Verzeichnisses `dir`; die Temp-Datei wird
// bei jedem Fehler wieder entfernt.
fn write_atomic(dir: &Path, path: &Path, work_id: &str, bytes: &[u8]) -> Result<(), String> {
    let temp = dir.join(temp_name(work_id));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    restrict_mode(&mut options);
    let mut file = options.open(&temp).map_err(|error| {
        format!(
            "usage marker: cannot create temp file '{}': {error}",
            temp.display()
        )
    })?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = written {
        remove_quietly(&temp);
        return Err(format!("usage marker: cannot write temp file: {error}"));
    }
    if let Err(error) = std::fs::rename(&temp, path) {
        remove_quietly(&temp);
        return Err(format!("usage marker: cannot publish marker: {error}"));
    }
    sync_dir(dir)
}

// Erste Zeile, ohne Steuerzeichen, höchstens `MAX_REASON_CHARS` Zeichen.
fn bounded_reason(reason: &str) -> String {
    let first_line = reason.lines().next().unwrap_or("");
    let bounded: String = first_line
        .chars()
        .filter(|character| !character.is_control())
        .take(MAX_REASON_CHARS)
        .collect();
    if bounded.trim().is_empty() {
        UNSPECIFIED_REASON.to_owned()
    } else {
        bounded
    }
}

// `.<id>.<pid>-<nanos>-<zähler>.tmp`: beginnt mit einem Punkt und endet nicht
// auf `.json`, ist also nie ein lesbarer Marker.
fn temp_name(work_id: &str) -> String {
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let stamp = jiff::Timestamp::now().as_nanosecond();
    format!(
        ".{work_id}.{}-{stamp}-{counter}{TEMP_SUFFIX}",
        std::process::id()
    )
}

#[cfg(unix)]
fn restrict_mode(options: &mut std::fs::OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(FILE_MODE);
}

#[cfg(not(unix))]
fn restrict_mode(_options: &mut std::fs::OpenOptions) {}

fn remove_quietly(path: &Path) {
    if let Err(error) = std::fs::remove_file(path) {
        tracing::debug!(path = %path.display(), error = %error, "usage marker temp file not removed");
    }
}

// fsync des Verzeichnisses, damit Anlegen, Umbenennen und Löschen eines
// Eintrags einen Absturz überstehen. Windows kennt kein fsync auf
// Verzeichnisse; dort bleibt es bei der atomaren Ersetzung.
#[cfg(unix)]
fn sync_dir(dir: &Path) -> Result<(), String> {
    std::fs::File::open(dir)
        .and_then(|handle| handle.sync_all())
        .map_err(|error| {
            format!(
                "usage marker: cannot sync directory '{}': {error}",
                dir.display()
            )
        })
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx, some_or};
    use jiff::SignedDuration;

    fn temp_dir() -> TestResult<tempfile::TempDir> {
        tempfile::tempdir().map_err(ctx("temp directory"))
    }

    fn usage(tokens: u64, tool_calls: u32) -> BudgetUsage {
        BudgetUsage {
            tokens,
            wall: SignedDuration::from_secs(5),
            tool_calls,
        }
    }

    fn marker(work_id: &str, epoch: u64, tokens: u64, tool_calls: u32) -> UnresolvedUsage {
        UnresolvedUsage::new(
            work_id,
            epoch,
            usage(tokens, tool_calls),
            "store: disk full",
        )
    }

    fn marker_file(root: &Path, work_id: &str) -> PathBuf {
        root.join(USAGE_UNRESOLVED_DIR)
            .join(format!("{work_id}.json"))
    }

    fn reservation(work_id: &str, epoch: u64, tokens: u64, tool_calls: u32) -> InFlightUsage {
        InFlightUsage::new(work_id, epoch, usage(tokens, tool_calls))
    }

    fn reservation_file(root: &Path, work_id: &str) -> PathBuf {
        root.join(USAGE_INFLIGHT_DIR)
            .join(format!("{work_id}.json"))
    }

    fn read_reservation(store: &UsageMarkerStore, work_id: &str) -> TestResult<InFlightUsage> {
        some_or(
            store
                .read_in_flight(work_id)
                .map_err(TestError::Unexpected)?,
            "reservation present",
        )
    }

    fn expect_err<T: std::fmt::Debug>(result: Result<T, String>, what: &str) -> TestResult<String> {
        match result {
            Err(message) => Ok(message),
            Ok(value) => Err(TestError::Unexpected(format!(
                "{what}: expected an error, got {value:?}"
            ))),
        }
    }

    #[test]
    fn test_write_and_read_round_trip() -> TestResult {
        let root = temp_dir()?;
        let store = UsageMarkerStore::in_job_root(root.path());
        let written = marker("job-1", 3, 1_234, 2);
        store.write(&written).map_err(TestError::Unexpected)?;
        let read = some_or(
            store.read("job-1").map_err(TestError::Unexpected)?,
            "marker present after write",
        )?;
        if read != written {
            return Err(TestError::Unexpected(format!(
                "round trip changed the marker: {written:?} -> {read:?}"
            )));
        }
        // Nur die Marker-Datei liegt im Verzeichnis, keine Temp-Reste.
        let names: Vec<String> = std::fs::read_dir(root.path().join(USAGE_UNRESOLVED_DIR))
            .map_err(ctx("read marker dir"))?
            .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
            .collect::<Result<Vec<String>, std::io::Error>>()
            .map_err(ctx("marker dir entry"))?;
        if names != ["job-1.json".to_owned()] {
            return Err(TestError::Unexpected(format!(
                "unexpected directory content: {names:?}"
            )));
        }
        Ok(())
    }

    #[test]
    fn test_read_of_a_missing_marker_is_none() -> TestResult {
        let root = temp_dir()?;
        let store = UsageMarkerStore::in_job_root(root.path());
        // Weder Verzeichnis noch Datei vorhanden.
        if store
            .read("job-1")
            .map_err(TestError::Unexpected)?
            .is_some()
        {
            return Err(TestError::Unexpected(
                "a marker appeared out of nowhere".to_owned(),
            ));
        }
        // Verzeichnis vorhanden, Datei nicht.
        store
            .write(&marker("job-2", 1, 1, 0))
            .map_err(TestError::Unexpected)?;
        if store
            .read("job-1")
            .map_err(TestError::Unexpected)?
            .is_some()
        {
            return Err(TestError::Unexpected(
                "a marker of another job was returned".to_owned(),
            ));
        }
        Ok(())
    }

    #[test]
    fn test_remove_is_idempotent() -> TestResult {
        let root = temp_dir()?;
        let store = UsageMarkerStore::in_job_root(root.path());
        // Ohne Verzeichnis und ohne Datei.
        store.remove("job-1").map_err(TestError::Unexpected)?;
        store
            .write(&marker("job-1", 1, 10, 0))
            .map_err(TestError::Unexpected)?;
        store.remove("job-1").map_err(TestError::Unexpected)?;
        store.remove("job-1").map_err(TestError::Unexpected)?;
        if store
            .read("job-1")
            .map_err(TestError::Unexpected)?
            .is_some()
        {
            return Err(TestError::Unexpected(
                "the marker survived its removal".to_owned(),
            ));
        }
        Ok(())
    }

    #[test]
    fn test_malformed_json_fails_closed() -> TestResult {
        let root = temp_dir()?;
        let store = UsageMarkerStore::in_job_root(root.path());
        std::fs::create_dir_all(root.path().join(USAGE_UNRESOLVED_DIR))
            .map_err(ctx("create marker dir"))?;
        let path = marker_file(root.path(), "job-1");
        std::fs::write(&path, b"{ not json").map_err(ctx("write garbage"))?;
        expect_err(store.read("job-1"), "garbage marker")?;
        // Ein Schreibversuch überschreibt die kaputte Datei nicht.
        expect_err(
            store.write(&marker("job-1", 1, 5, 0)),
            "write over a garbage marker",
        )?;
        let after = std::fs::read(&path).map_err(ctx("re-read garbage"))?;
        if after != b"{ not json" {
            return Err(TestError::Unexpected(
                "the unreadable marker was overwritten".to_owned(),
            ));
        }
        Ok(())
    }

    #[test]
    fn test_unknown_fields_schema_and_foreign_job_fail_closed() -> TestResult {
        let root = temp_dir()?;
        let store = UsageMarkerStore::in_job_root(root.path());
        store
            .write(&marker("job-1", 1, 5, 0))
            .map_err(TestError::Unexpected)?;
        let path = marker_file(root.path(), "job-1");
        let text = std::fs::read_to_string(&path).map_err(ctx("read marker"))?;

        // Unbekanntes Feld (deny_unknown_fields).
        let with_extra = text.replacen('{', "{\"surprise\":true,", 1);
        std::fs::write(&path, with_extra).map_err(ctx("write extra field"))?;
        expect_err(store.read("job-1"), "unknown field")?;

        // Unbekannte Schemaversion.
        let future_schema = text.replacen("\"schema_version\":1", "\"schema_version\":2", 1);
        std::fs::write(&path, future_schema).map_err(ctx("write future schema"))?;
        expect_err(store.read("job-1"), "future schema")?;

        // Marker einer anderen Arbeits-ID unter diesem Dateinamen.
        let foreign = text.replacen("\"work_id\":\"job-1\"", "\"work_id\":\"job-2\"", 1);
        std::fs::write(&path, foreign).map_err(ctx("write foreign marker"))?;
        expect_err(store.read("job-1"), "foreign marker")?;
        Ok(())
    }

    #[test]
    fn test_write_keeps_the_larger_usage_per_field() -> TestResult {
        let root = temp_dir()?;
        let store = UsageMarkerStore::in_job_root(root.path());
        store
            .write(&marker("job-1", 1, 100, 1))
            .map_err(TestError::Unexpected)?;
        // Weniger Tokens, mehr Tool-Aufrufe: jedes Feld behält sein Maximum.
        store
            .write(&marker("job-1", 1, 50, 5))
            .map_err(TestError::Unexpected)?;
        let read = some_or(
            store.read("job-1").map_err(TestError::Unexpected)?,
            "marker present",
        )?;
        if read.usage.tokens != 100 || read.usage.tool_calls != 5 {
            return Err(TestError::Unexpected(format!(
                "field-wise maximum expected (100 tokens, 5 tool calls), got {:?}",
                read.usage
            )));
        }
        // Größere Usage ersetzt die kleinere.
        store
            .write(&marker("job-1", 1, 400, 5))
            .map_err(TestError::Unexpected)?;
        let read = some_or(
            store.read("job-1").map_err(TestError::Unexpected)?,
            "marker present",
        )?;
        if read.usage.tokens != 400 {
            return Err(TestError::Unexpected(format!(
                "larger usage was not kept: {:?}",
                read.usage
            )));
        }
        Ok(())
    }

    #[test]
    fn test_older_epoch_does_not_rewind_the_origin() -> TestResult {
        let root = temp_dir()?;
        let store = UsageMarkerStore::in_job_root(root.path());
        store
            .write(&marker("job-1", 7, 10, 0))
            .map_err(TestError::Unexpected)?;
        store
            .write(&marker("job-1", 6, 99, 0))
            .map_err(TestError::Unexpected)?;
        let read = some_or(
            store.read("job-1").map_err(TestError::Unexpected)?,
            "marker present",
        )?;
        if read.epoch != 7 || read.usage.tokens != 99 {
            return Err(TestError::Unexpected(format!(
                "expected epoch 7 with the merged 99 tokens, got {read:?}"
            )));
        }
        Ok(())
    }

    #[test]
    fn test_merged_is_the_field_wise_maximum() {
        let merged = UsageMarkerStore::merged(
            &BudgetUsage {
                tokens: 10,
                wall: SignedDuration::from_secs(9),
                tool_calls: 1,
            },
            &BudgetUsage {
                tokens: 3,
                wall: SignedDuration::from_secs(20),
                tool_calls: 4,
            },
        );
        assert_eq!(
            merged,
            BudgetUsage {
                tokens: 10,
                wall: SignedDuration::from_secs(20),
                tool_calls: 4,
            }
        );
    }

    #[test]
    fn test_reason_is_bounded_to_one_short_line() -> TestResult {
        let root = temp_dir()?;
        let store = UsageMarkerStore::in_job_root(root.path());
        let long = format!(
            "{}\nsecond line with details",
            "x".repeat(MAX_REASON_CHARS * 3)
        );
        let mut raw = marker("job-1", 1, 1, 0);
        // Direkt gebaut, am Konstruktor vorbei: `write` begrenzt trotzdem.
        raw.reason = long;
        store.write(&raw).map_err(TestError::Unexpected)?;
        let read = some_or(
            store.read("job-1").map_err(TestError::Unexpected)?,
            "marker present",
        )?;
        if read.reason.chars().count() != MAX_REASON_CHARS || read.reason.contains('\n') {
            return Err(TestError::Unexpected(format!(
                "reason not bounded: {} chars",
                read.reason.chars().count()
            )));
        }
        if UnresolvedUsage::new("job-1", 1, usage(1, 0), "\n\t ").reason != UNSPECIFIED_REASON {
            return Err(TestError::Unexpected(
                "an empty reason must become 'unspecified'".to_owned(),
            ));
        }
        Ok(())
    }

    #[test]
    fn test_unsafe_work_ids_are_rejected() -> TestResult {
        let root = temp_dir()?;
        let store = UsageMarkerStore::in_job_root(root.path());
        for id in ["", "../escape", "a/b", "a\\b", "a.b", "job\n1", "job 1"] {
            expect_err(store.read(id), "read with unsafe id")?;
            expect_err(store.remove(id), "remove with unsafe id")?;
            expect_err(store.write(&marker(id, 1, 1, 0)), "write with unsafe id")?;
        }
        // Nichts ist außerhalb des Marker-Verzeichnisses entstanden.
        let entries = std::fs::read_dir(root.path())
            .map_err(ctx("read job root"))?
            .count();
        if entries != 0 {
            return Err(TestError::Unexpected(format!(
                "{entries} entries were created for rejected ids"
            )));
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn test_symlinked_marker_fails_closed() -> TestResult {
        let root = temp_dir()?;
        let store = UsageMarkerStore::in_job_root(root.path());
        store
            .write(&marker("job-2", 1, 5, 0))
            .map_err(TestError::Unexpected)?;
        std::os::unix::fs::symlink(
            marker_file(root.path(), "job-2"),
            marker_file(root.path(), "job-1"),
        )
        .map_err(ctx("create symlink"))?;
        expect_err(store.read("job-1"), "symlinked marker")?;
        Ok(())
    }

    // ── Rundenreservierung (Review B2 zu #151) ────────────────────────────

    #[test]
    fn test_in_flight_round_trip_and_epoch_guarded_remove() -> TestResult {
        let root = temp_dir()?;
        let store = UsageMarkerStore::in_job_root(root.path());
        if store
            .read_in_flight("job-1")
            .map_err(TestError::Unexpected)?
            .is_some()
        {
            return Err(TestError::Unexpected(
                "a reservation appeared out of nowhere".to_owned(),
            ));
        }
        let written = reservation("job-1", 4, 70, 1);
        store
            .write_in_flight(&written)
            .map_err(TestError::Unexpected)?;
        let read = read_reservation(&store, "job-1")?;
        if read != written {
            return Err(TestError::Unexpected(format!(
                "round trip changed the reservation: {written:?} -> {read:?}"
            )));
        }
        // Reservierung und Marker liegen getrennt.
        if store
            .read("job-1")
            .map_err(TestError::Unexpected)?
            .is_some()
        {
            return Err(TestError::Unexpected(
                "a reservation must not look like an unresolved-usage marker".to_owned(),
            ));
        }

        // Eine ältere Epoche entfernt die Reservierung einer jüngeren nicht.
        store
            .remove_in_flight("job-1", 3)
            .map_err(TestError::Unexpected)?;
        read_reservation(&store, "job-1")?;
        store
            .remove_in_flight("job-1", 4)
            .map_err(TestError::Unexpected)?;
        store
            .remove_in_flight("job-1", 4)
            .map_err(TestError::Unexpected)?;
        if store
            .read_in_flight("job-1")
            .map_err(TestError::Unexpected)?
            .is_some()
        {
            return Err(TestError::Unexpected(
                "the reservation survived its removal".to_owned(),
            ));
        }
        Ok(())
    }

    #[test]
    fn test_in_flight_write_never_replaces_a_newer_epoch() -> TestResult {
        let root = temp_dir()?;
        let store = UsageMarkerStore::in_job_root(root.path());
        store
            .write_in_flight(&reservation("job-1", 5, 10, 0))
            .map_err(TestError::Unexpected)?;
        expect_err(
            store.write_in_flight(&reservation("job-1", 4, 99, 0)),
            "stale writer",
        )?;
        let read = read_reservation(&store, "job-1")?;
        assert_eq!((read.epoch, read.usage_before.tokens), (5, 10));

        // Dieselbe oder eine jüngere Epoche ersetzt sie (Anheben, Reclaim).
        store
            .write_in_flight(&reservation("job-1", 5, 20, 0))
            .map_err(TestError::Unexpected)?;
        store
            .write_in_flight(&reservation("job-1", 6, 30, 1))
            .map_err(TestError::Unexpected)?;
        let read = read_reservation(&store, "job-1")?;
        assert_eq!((read.epoch, read.usage_before.tokens), (6, 30));
        Ok(())
    }

    #[test]
    fn test_in_flight_malformed_unknown_or_foreign_fails_closed() -> TestResult {
        let root = temp_dir()?;
        let store = UsageMarkerStore::in_job_root(root.path());
        std::fs::create_dir_all(root.path().join(USAGE_INFLIGHT_DIR))
            .map_err(ctx("create reservation dir"))?;
        let path = reservation_file(root.path(), "job-1");
        std::fs::write(&path, b"{ not json").map_err(ctx("write garbage"))?;
        expect_err(store.read_in_flight("job-1"), "garbage reservation")?;
        expect_err(
            store.write_in_flight(&reservation("job-1", 1, 5, 0)),
            "write over a garbage reservation",
        )?;
        expect_err(
            store.remove_in_flight("job-1", 9),
            "remove of a garbage reservation",
        )?;
        let after = std::fs::read(&path).map_err(ctx("re-read garbage"))?;
        if after != b"{ not json" {
            return Err(TestError::Unexpected(
                "the unreadable reservation was touched".to_owned(),
            ));
        }

        std::fs::remove_file(&path).map_err(ctx("remove garbage"))?;
        store
            .write_in_flight(&reservation("job-1", 1, 5, 0))
            .map_err(TestError::Unexpected)?;
        let text = std::fs::read_to_string(&path).map_err(ctx("read reservation"))?;
        let with_extra = text.replacen('{', "{\"surprise\":true,", 1);
        std::fs::write(&path, with_extra).map_err(ctx("write extra field"))?;
        expect_err(store.read_in_flight("job-1"), "unknown field")?;
        let future_schema = text.replacen("\"schema_version\":1", "\"schema_version\":2", 1);
        std::fs::write(&path, future_schema).map_err(ctx("write future schema"))?;
        expect_err(store.read_in_flight("job-1"), "future schema")?;
        let foreign = text.replacen("\"work_id\":\"job-1\"", "\"work_id\":\"job-2\"", 1);
        std::fs::write(&path, foreign).map_err(ctx("write foreign reservation"))?;
        expect_err(store.read_in_flight("job-1"), "foreign reservation")?;
        Ok(())
    }

    #[test]
    fn test_in_flight_unsafe_work_ids_are_rejected() -> TestResult {
        let root = temp_dir()?;
        let store = UsageMarkerStore::in_job_root(root.path());
        for id in ["", "../escape", "a/b", "a\\b", "a.b", "job\n1", "job 1"] {
            expect_err(store.read_in_flight(id), "read with unsafe id")?;
            expect_err(store.remove_in_flight(id, 1), "remove with unsafe id")?;
            expect_err(
                store.write_in_flight(&reservation(id, 1, 1, 0)),
                "write with unsafe id",
            )?;
        }
        let entries = std::fs::read_dir(root.path())
            .map_err(ctx("read job root"))?
            .count();
        if entries != 0 {
            return Err(TestError::Unexpected(format!(
                "{entries} entries were created for rejected ids"
            )));
        }
        Ok(())
    }

    #[test]
    fn test_superseded_by_needs_more_tokens_or_tool_calls() {
        let reserved = reservation("job-1", 1, 10, 2);
        assert!(
            !reserved.superseded_by(&usage(10, 2)),
            "equal usage proves nothing"
        );
        assert!(!reserved.superseded_by(&usage(9, 2)));
        assert!(
            !reserved.superseded_by(&BudgetUsage {
                tokens: 10,
                wall: SignedDuration::from_secs(500),
                tool_calls: 2,
            }),
            "wall time alone proves nothing"
        );
        assert!(reserved.superseded_by(&usage(11, 2)));
        assert!(reserved.superseded_by(&usage(10, 3)));
    }

    // ── Unbewiesener Verbrauch (Review P1 4394111159) ─────────────────────

    #[test]
    fn test_unproven_spend_is_never_superseded_and_survives_a_round_trip() -> TestResult {
        let unproven = reservation("job-1", 2, 10, 2).with_unproven_spend(true);
        assert!(
            !unproven.superseded_by(&usage(1_000, 50)),
            "no stored usage proves a round whose spend is unproven"
        );

        let root = temp_dir()?;
        let store = UsageMarkerStore::in_job_root(root.path());
        store
            .write_in_flight(&unproven)
            .map_err(TestError::Unexpected)?;
        let read = read_reservation(&store, "job-1")?;
        assert!(read.unproven_spend, "{read:?}");
        assert_eq!(read, unproven);
        Ok(())
    }

    #[test]
    fn test_reservation_of_an_older_worker_reads_without_unproven_spend() -> TestResult {
        let root = temp_dir()?;
        let store = UsageMarkerStore::in_job_root(root.path());
        store
            .write_in_flight(&reservation("job-1", 1, 5, 0))
            .map_err(TestError::Unexpected)?;
        let path = reservation_file(root.path(), "job-1");
        let text = std::fs::read_to_string(&path).map_err(ctx("read reservation"))?;
        let legacy = text.replacen(",\"unproven_spend\":false", "", 1);
        if legacy == text {
            return Err(TestError::Unexpected(format!(
                "the field must be serialized: {text}"
            )));
        }
        std::fs::write(&path, legacy).map_err(ctx("write legacy reservation"))?;
        let read = read_reservation(&store, "job-1")?;
        assert!(!read.unproven_spend, "{read:?}");
        Ok(())
    }

    #[test]
    fn test_remove_if_covered_keeps_a_marker_that_grew() -> TestResult {
        let root = temp_dir()?;
        let store = UsageMarkerStore::in_job_root(root.path());
        // Ohne Marker bleibt nichts liegen.
        assert_eq!(
            store
                .remove_if_covered("job-1", &usage(1, 0))
                .map_err(TestError::Unexpected)?,
            None
        );
        store
            .write(&marker("job-1", 1, 40, 3))
            .map_err(TestError::Unexpected)?;
        // Mehr Werkzeugaufrufe als festgeschrieben: der Marker bleibt.
        let kept = store
            .remove_if_covered("job-1", &usage(40, 2))
            .map_err(TestError::Unexpected)?;
        assert_eq!(kept, Some(usage(40, 3)));
        some_or(
            store.read("job-1").map_err(TestError::Unexpected)?,
            "an uncovered marker stays",
        )?;
        // Ein abgedeckter Marker wird entfernt.
        assert_eq!(
            store
                .remove_if_covered("job-1", &usage(41, 3))
                .map_err(TestError::Unexpected)?,
            None
        );
        if store
            .read("job-1")
            .map_err(TestError::Unexpected)?
            .is_some()
        {
            return Err(TestError::Unexpected(
                "a covered marker must be removed".to_owned(),
            ));
        }
        Ok(())
    }
}
