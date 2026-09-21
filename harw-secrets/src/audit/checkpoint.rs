//! Signed chain-head checkpoints (spec §4.2–§4.3). Every N events or T minutes,
//! the current chain-head hash is ML-DSA-signed and written to a separate
//! append-only checkpoint file. Checkpoints are themselves chained
//! (`prev_checkpoint_hash`) so the checkpoint file can also be verified for
//! gaps. The signing key is distinct from the KEK.
//!
//! Structural verification (monotonic `event_count`, no checkpoint beyond the
//! log) and ML-DSA signing/verification are implemented with `crypt_guard`.
//!
//! [`load_and_verify_persisted_checkpoints`] ist das Platten-Gegenstück zu
//! [`crate::audit::chain::load_and_verify_persisted_chain`]: es dekodiert
//! exakt das Byte-Layout, das `SecretStore::persist_audit_state` (`store.rs`)
//! nach `checkpoints.log` schreibt, rekonstruiert die Checkpoints und
//! durchläuft das vollständige §4.3-Verifikationsverfahren (innere
//! Verkettung, Monotonie der `event_count`-Werte, keine Reichweite über das
//! Audit-Log hinaus und — optional — jede ML-DSA-Signatur). Es ist der
//! Lesepfad, den `harw doctor`s Audit-Integritäts-Check verwendet; es läuft
//! nie automatisch bei der Konstruktion eines `SecretStore` (aus demselben
//! Grund wie `load_and_verify_persisted_chain`, siehe dessen Moduldoku): eine
//! periodische/explizite Manipulationsprüfung, kein Teil des In-Prozess-
//! create/get/delete-Pfads.

use std::path::Path;

use crypt_guard::sign::{
    SignAlgorithm,
    ml_dsa::{MlDsa65Impl, MlDsaSignature, MlDsaSigningKey, MlDsaVerifyingKey},
};
use jiff::Timestamp;
use secrecy::{ExposeSecret as _, SecretBox};
use sha2::{Digest, Sha256};

use crate::audit::chain::GENESIS_HASH;
use crate::error::{AuditError, AuditResult};

/// Feste 16-Byte-Kennung einer persistierten `checkpoints.log` (§ `store.rs`
/// `persist_audit_state`). Hier zentral gehalten, damit Schreiber
/// (`store.rs`) und dieser Leser eine einzige Literaldefinition teilen statt
/// zweier Kopien, die auseinanderdriften könnten — dasselbe Muster wie
/// [`crate::audit::chain::AUDIT_MAGIC`] für `audit.log`.
pub(crate) const CHECKPOINT_MAGIC: &[u8] = b"HARW-CHECKPOINT\0";

/// Die einzige persistierte Checkpoint-Datei-Formatversion, die dieses Crate
/// derzeit schreibt oder liest.
pub(crate) const CHECKPOINT_FORMAT_VERSION: u32 = 1;

/// One signed checkpoint of the audit chain head.
#[derive(Clone, Debug)]
pub struct Checkpoint {
    /// The audit-log chain head this checkpoint attests to.
    pub chain_head_hash: [u8; 32],
    /// How many events had been recorded when it was signed.
    pub event_count: u64,
    /// When the checkpoint was signed.
    pub signed_at: Timestamp,
    /// ML-DSA signature over the checkpoint's canonical bytes.
    pub signature: Vec<u8>,
    /// SHA-256 of the previous checkpoint's canonical bytes (genesis for first).
    pub prev_checkpoint_hash: [u8; 32],
}

/// Deterministic serialization of a checkpoint for chaining/signing. The
/// `signature` field is intentionally excluded — it is computed *over* these
/// bytes.
#[must_use]
pub fn checkpoint_canonical_bytes(checkpoint: &Checkpoint) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&checkpoint.chain_head_hash);
    out.extend_from_slice(&checkpoint.event_count.to_be_bytes());
    out.extend_from_slice(&checkpoint.signed_at.as_second().to_be_bytes());
    out.extend_from_slice(&checkpoint.signed_at.subsec_nanosecond().to_be_bytes());
    out.extend_from_slice(&checkpoint.prev_checkpoint_hash);
    out
}

/// SHA-256 of a checkpoint's canonical bytes (used for `prev_checkpoint_hash`).
#[must_use]
pub fn checkpoint_hash(checkpoint: &Checkpoint) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(checkpoint_canonical_bytes(checkpoint));
    hasher.finalize().into()
}

/// An in-memory, append-only chain of signed checkpoints.
#[derive(Clone, Debug, Default)]
pub struct CheckpointLog {
    checkpoints: Vec<Checkpoint>,
    head: [u8; 32],
}

impl CheckpointLog {
    /// A fresh checkpoint chain with a genesis head.
    #[must_use]
    pub fn new() -> Self {
        Self {
            checkpoints: Vec::new(),
            head: GENESIS_HASH,
        }
    }

    /// The current checkpoint-chain head.
    #[must_use]
    pub fn chain_head(&self) -> [u8; 32] {
        self.head
    }

    /// Number of checkpoints recorded.
    #[must_use]
    pub fn len(&self) -> usize {
        self.checkpoints.len()
    }

    /// Whether no checkpoint has been recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.checkpoints.is_empty()
    }

    /// The recorded checkpoints in append order.
    #[must_use]
    pub fn checkpoints(&self) -> &[Checkpoint] {
        &self.checkpoints
    }

    /// Sign `chain_head_hash` at `event_count` under `signing_key` and append
    /// the resulting checkpoint.
    pub fn emit(
        &mut self,
        chain_head_hash: [u8; 32],
        event_count: u64,
        signing_key: &SecretBox<[u8]>,
    ) -> AuditResult<&Checkpoint> {
        if let Some(previous) = self.checkpoints.last() {
            if event_count <= previous.event_count {
                return Err(AuditError::NonMonotonicCheckpoint {
                    index: self.checkpoints.len() as u64,
                });
            }
        }

        let mut checkpoint = Checkpoint {
            chain_head_hash,
            event_count,
            signed_at: Timestamp::now(),
            signature: Vec::new(),
            prev_checkpoint_hash: self.head,
        };
        let signing_key = MlDsaSigningKey::from_bytes(signing_key.expose_secret().to_vec());
        let signature = MlDsa65Impl::sign(&signing_key, &checkpoint_canonical_bytes(&checkpoint))
            .map_err(AuditError::CheckpointSigning)?;

        checkpoint.signature = signature.as_ref().to_vec();
        self.head = checkpoint_hash(&checkpoint);
        self.checkpoints.push(checkpoint);

        Ok(self
            .checkpoints
            .last()
            .expect("checkpoint was appended before borrowing it"))
    }

    /// Confirm `event_count` values are strictly increasing across checkpoints
    /// (§4.3 step 3). Reports the first non-increasing index.
    pub fn check_monotonic(&self) -> AuditResult<()> {
        for (i, pair) in self.checkpoints.windows(2).enumerate() {
            if pair[1].event_count <= pair[0].event_count {
                return Err(AuditError::NonMonotonicCheckpoint {
                    index: (i + 1) as u64,
                });
            }
        }
        Ok(())
    }

    /// Confirm no checkpoint references more events than the log actually holds
    /// (§4.3 step 3) — a checkpoint beyond `log_len` indicates post-checkpoint
    /// truncation.
    pub fn check_within_log(&self, log_len: u64) -> AuditResult<()> {
        for checkpoint in &self.checkpoints {
            if checkpoint.event_count > log_len {
                return Err(AuditError::CheckpointBeyondLog {
                    referenced: checkpoint.event_count,
                    actual: log_len,
                });
            }
        }
        Ok(())
    }

    /// Verify the checkpoint chaining is internally consistent: each
    /// `prev_checkpoint_hash` equals the recomputed hash of its predecessor.
    pub fn verify_chain(&self) -> AuditResult<()> {
        let mut prev = GENESIS_HASH;
        for (index, checkpoint) in self.checkpoints.iter().enumerate() {
            if checkpoint.prev_checkpoint_hash != prev {
                return Err(AuditError::ChainBroken {
                    index: index as u64,
                    expected: prev,
                    found: checkpoint.prev_checkpoint_hash,
                });
            }
            prev = checkpoint_hash(checkpoint);
        }
        Ok(())
    }

    /// Verify one checkpoint's ML-DSA signature against `verification_key`
    /// (§4.3 step 2).
    pub fn verify_signature(
        &self,
        checkpoint: &Checkpoint,
        verification_key: &[u8],
    ) -> AuditResult<()> {
        let verification_key = MlDsaVerifyingKey::from_bytes(verification_key.to_vec());
        let signature = MlDsaSignature::from_bytes(checkpoint.signature.clone());

        MlDsa65Impl::verify(
            &verification_key,
            &checkpoint_canonical_bytes(checkpoint),
            &signature,
        )
        .map_err(|_| AuditError::InvalidCheckpointSignature {
            event_count: checkpoint.event_count,
        })
    }
}

/// Ergebnis des Ladens und Verifizierens der persistierten
/// `checkpoints.log`, analog zu
/// [`crate::audit::chain::PersistedChainStatus`]s zweifacher Unterscheidung:
/// „nichts zu prüfen" bleibt von „geprüft, unversehrt" getrennt — ein
/// tatsächlicher Checkpoint-Kettenbruch wird als `Err` gemeldet, nie in
/// diesen Typ eingefaltet (siehe [`load_and_verify_persisted_checkpoints`]s
/// `Errors`-Abschnitt).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PersistedCheckpointStatus {
    /// Es existiert keine Checkpoint-Datei am geprüften Pfad. Kein Bruch:
    /// ein Speicher, der noch nie einen Checkpoint signiert hat, sieht
    /// genauso aus.
    Absent,
    /// Die Datei wurde gelesen, ihre Rahmung geparst, und jede §4.3-
    /// Strukturprüfung (innere Verkettung, Monotonie der `event_count`-
    /// Werte, keine Reichweite über das Audit-Log hinaus) hat bestanden.
    Intact {
        /// Anzahl der Checkpoints in der persistierten Datei.
        checkpoint_count: u64,
        /// Ob jede ML-DSA-Signatur tatsächlich verifiziert wurde. `false`,
        /// wenn [`load_and_verify_persisted_checkpoints`] mit
        /// `verification_key: None` aufgerufen wurde — das ist ein
        /// ausdrückliches „Signaturprüfung wurde übersprungen", nie ein
        /// stillschweigender Erfolg.
        signatures_checked: bool,
    },
}

/// Baut ein generisches, inhaltsfreies [`AuditError::Io`] für einen Rahmen-/
/// Parsefehler der Checkpoint-Datei. `reason` darf niemals Checkpoint-Inhalt
/// oder Schlüsselmaterial enthalten — nur eine feste Beschreibung, welche
/// Strukturprüfung fehlgeschlagen ist.
fn malformed_persisted_checkpoints(reason: &'static str) -> AuditError {
    AuditError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, reason))
}

/// Bounds-geprüfter Leser über die Bytes der persistierten Checkpoint-Datei.
///
/// Bewusst eine private, eigenständige Kopie von
/// [`crate::audit::chain`]s entsprechendem Cursor statt eines gemeinsam
/// genutzten: seine Fehlermeldungen sprechen von „checkpoint file", damit
/// eine abgeschnittene/kaputte `checkpoints.log` nie fälschlich als
/// `audit.log`-Problem gemeldet wird. Jeder Zugriff verweigert sofort, sobald
/// weniger Bytes übrig sind als angefordert, statt außerhalb der Grenzen zu
/// slicen — ein von einem Angreifer kontrolliertes Längenpräfix innerhalb
/// eines Checkpoints kann diesen Parser deshalb nie zum Absturz bringen oder
/// eine Allokation über das hinaus treiben, was die Datei tatsächlich
/// enthält (jede zurückgegebene Slice leiht direkt aus der Eingabe; nichts
/// wird kopiert, bis ein Array fester Größe oder ein eigener `Vec`
/// zusammengebaut wird).
struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    fn take(&mut self, len: usize) -> AuditResult<&'a [u8]> {
        if len > self.remaining() {
            return Err(malformed_persisted_checkpoints(
                "checkpoint file record is truncated",
            ));
        }
        let slice = &self.data[self.pos..self.pos + len];
        self.pos += len;
        Ok(slice)
    }

    fn take_u32_be(&mut self) -> AuditResult<u32> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn take_u64_be(&mut self) -> AuditResult<u64> {
        let b = self.take(8)?;
        Ok(u64::from_be_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    fn take_i32_be(&mut self) -> AuditResult<i32> {
        let b = self.take(4)?;
        Ok(i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn take_i64_be(&mut self) -> AuditResult<i64> {
        let b = self.take(8)?;
        Ok(i64::from_be_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    fn take_hash(&mut self) -> AuditResult<[u8; 32]> {
        let b = self.take(32)?;
        let mut out = [0u8; 32];
        out.copy_from_slice(b);
        Ok(out)
    }

    /// Ein `u64`-längenpräfigiertes Feld. Die Länge wird von `take` gegen die
    /// tatsächlich übrigen Bytes geprüft, sodass eine riesige, von einem
    /// Angreifer vorgegebene Länge einfach schnell fehlschlägt statt zu
    /// allozieren.
    fn take_len_prefixed(&mut self) -> AuditResult<&'a [u8]> {
        let len = self.take_u64_be()?;
        let len = usize::try_from(len).map_err(|_| {
            malformed_persisted_checkpoints("checkpoint file length prefix is not addressable")
        })?;
        self.take(len)
    }
}

/// Lädt `path` (die durable `checkpoints.log`, die
/// `SecretStore::persist_audit_state` schreibt) und verifiziert sie
/// vollständig gegen `audit_event_count` (Spezifikation §4.3, Schritte 2-3).
///
/// # Was in welcher Reihenfolge geprüft wird
/// 1. **Rahmung**: die feste Kennung, eine unterstützte Formatversion, dann
///    so viele Datensätze wie das eigene Zählfeld der Datei angibt.
/// 2. **Innere Verkettung** ([`CheckpointLog::verify_chain`]): jeder
///    `prev_checkpoint_hash` stimmt mit dem neu berechneten Hash seines
///    Vorgängers überein.
/// 3. **Monotonie** ([`CheckpointLog::check_monotonic`]): die `event_count`-
///    Werte steigen über die Checkpoints hinweg strikt an.
/// 4. **Reichweite** ([`CheckpointLog::check_within_log`]): kein Checkpoint
///    referenziert mehr Ereignisse als `audit_event_count` — ein Checkpoint
///    jenseits dieser Grenze würde bedeuten, dass das Audit-Log nach der
///    Signierung des Checkpoints gekürzt wurde.
/// 5. **Signaturen**, nur wenn `verification_key` `Some` ist
///    ([`CheckpointLog::verify_signature`]): jede ML-DSA-Signatur wird gegen
///    diesen Schlüssel geprüft. Mit `verification_key: None` wird dieser
///    Schritt ausdrücklich übersprungen —
///    [`PersistedCheckpointStatus::Intact`]s Feld `signatures_checked`
///    meldet dann `false` statt eines stillschweigenden Erfolgs.
///
/// # Arguments
/// - `path` (`&Path`): Pfad zur persistierten Checkpoint-Datei.
/// - `audit_event_count` (`u64`): die bereits geprüfte Ereigniszahl der
///   zugehörigen Audit-Kette (siehe
///   [`crate::audit::chain::load_and_verify_persisted_chain`]) — die Grenze,
///   gegen die Schritt 4 prüft. Aufrufer, die keine vertrauenswürdige Zahl
///   ermitteln konnten (z. B. weil die Audit-Kette selbst nicht verifizierte),
///   sollten `u64::MAX` übergeben, damit Schritt 4 zu einem No-op wird statt
///   gegen eine erfundene `0` zu vergleichen.
/// - `verification_key` (`Option<&[u8]>`): ML-DSA-Verifikationsschlüssel;
///   `None` überspringt Schritt 5 ausdrücklich, statt ihn stillschweigend als
///   bestanden zu melden.
///
/// # Returns
/// [`PersistedCheckpointStatus::Absent`], wenn keine Datei existiert, sonst
/// [`PersistedCheckpointStatus::Intact`] mit der geprüften Checkpoint-Anzahl
/// und ob Signaturen tatsächlich geprüft wurden.
///
/// # Errors
/// - [`AuditError::Io`]: der Pfad ist keine reguläre Datei, oder seine Bytes
///   dekodieren nicht als dieses Format.
/// - [`AuditError::ChainBroken`]: ein `prev_checkpoint_hash` stimmt nicht mit
///   dem neu berechneten Hash seines Vorgängers überein.
/// - [`AuditError::NonMonotonicCheckpoint`]: `event_count`-Werte steigen
///   nicht strikt an.
/// - [`AuditError::CheckpointBeyondLog`]: ein Checkpoint referenziert mehr
///   Ereignisse als `audit_event_count`.
/// - [`AuditError::InvalidCheckpointSignature`]: die ML-DSA-Signatur eines
///   Checkpoints verifiziert nicht gegen `verification_key` (nur möglich,
///   wenn `verification_key` `Some` ist).
///
/// # Concurrency
/// Reine Berechnung über einmalig per `fs::read` gelesene Bytes; hält keine
/// Locks, startet keine Threads und ist von jedem Thread aus sicher
/// aufrufbar, auch parallel zu einem `SecretStore`, der dieselbe Datei
/// mutiert — der Read liefert eine Momentaufnahme, keine zerrissene Sicht.
///
/// # Examples
/// ```rust,no_run
/// use std::path::Path;
/// use harw_secrets::audit::checkpoint::{
///     PersistedCheckpointStatus, load_and_verify_persisted_checkpoints,
/// };
///
/// match load_and_verify_persisted_checkpoints(
///     Path::new("/var/lib/harw/secrets/checkpoints.log"),
///     42,
///     None,
/// ) {
///     Ok(PersistedCheckpointStatus::Absent) => { /* noch nichts signiert */ }
///     Ok(PersistedCheckpointStatus::Intact { checkpoint_count, .. }) => {
///         println!("{checkpoint_count} Checkpoints unversehrt");
///     }
///     Err(error) => eprintln!("Checkpoint-Verifikation fehlgeschlagen: {error}"),
/// }
/// ```
pub fn load_and_verify_persisted_checkpoints(
    path: &Path,
    audit_event_count: u64,
    verification_key: Option<&[u8]>,
) -> AuditResult<PersistedCheckpointStatus> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => {
            return Err(malformed_persisted_checkpoints(
                "checkpoint file path exists but is not a regular file",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PersistedCheckpointStatus::Absent);
        }
        Err(error) => return Err(AuditError::Io(error)),
    }

    let bytes = std::fs::read(path)?;
    let mut cursor = Cursor::new(&bytes);

    let magic = cursor.take(CHECKPOINT_MAGIC.len())?;
    if magic != CHECKPOINT_MAGIC {
        return Err(malformed_persisted_checkpoints(
            "checkpoint file has an unrecognized header",
        ));
    }
    let version = cursor.take_u32_be()?;
    if version != CHECKPOINT_FORMAT_VERSION {
        return Err(malformed_persisted_checkpoints(
            "checkpoint file has an unsupported format version",
        ));
    }
    let checkpoint_count = cursor.take_u64_be()?;

    let mut checkpoints: Vec<Checkpoint> = Vec::new();
    for _ in 0..checkpoint_count {
        let chain_head_hash = cursor.take_hash()?;
        let event_count = cursor.take_u64_be()?;
        let second = cursor.take_i64_be()?;
        let nanosecond = cursor.take_i32_be()?;
        let signed_at = Timestamp::new(second, nanosecond).map_err(|_| {
            malformed_persisted_checkpoints("checkpoint has an invalid signed_at timestamp")
        })?;
        let signature = cursor.take_len_prefixed()?.to_vec();
        let prev_checkpoint_hash = cursor.take_hash()?;

        checkpoints.push(Checkpoint {
            chain_head_hash,
            event_count,
            signed_at,
            signature,
            prev_checkpoint_hash,
        });
    }

    if cursor.remaining() != 0 {
        return Err(malformed_persisted_checkpoints(
            "checkpoint file has trailing data after its recorded checkpoint count",
        ));
    }

    let head = checkpoints
        .last()
        .map(checkpoint_hash)
        .unwrap_or(GENESIS_HASH);
    // Im selben Modul konstruiert: `CheckpointLog`s Felder sind privat zu
    // dieser Datei, und diese Checkpoints tragen bereits ihre echten
    // Signaturen von der Platte — diese Rekonstruktion signiert nicht neu und
    // validiert auch sonst nichts. Jede Prüfung, die die persistierten
    // Checkpoints noch bestehen müssen, läuft explizit gleich im Anschluss,
    // genau wie `CheckpointLog::emit` seine eigene Monotonieprüfung nie
    // auslässt.
    let log = CheckpointLog { checkpoints, head };

    log.verify_chain()?;
    log.check_monotonic()?;
    log.check_within_log(audit_event_count)?;

    let signatures_checked = match verification_key {
        Some(key) => {
            for checkpoint in log.checkpoints() {
                log.verify_signature(checkpoint, key)?;
            }
            true
        }
        None => false,
    };

    Ok(PersistedCheckpointStatus::Intact {
        checkpoint_count,
        signatures_checked,
    })
}

#[cfg(test)]
mod tests {
    use crypt_guard::{
        kem::backend::OsRng,
        sign::{ml_dsa::MlDsa65Impl, SignAlgorithm},
    };

    use super::*;

    fn signing_keypair() -> (SecretBox<[u8]>, Vec<u8>) {
        let mut rng = OsRng;
        let (signing_key, verification_key) =
            MlDsa65Impl::keypair(&mut rng).expect("generate ML-DSA-65 test keypair");

        (
            SecretBox::new(signing_key.as_bytes().to_vec().into_boxed_slice()),
            verification_key.as_bytes().to_vec(),
        )
    }

    #[test]
    fn emitted_checkpoint_round_trips_through_mldsa_verification() {
        let (signing_seed, verification_key) = signing_keypair();
        let mut checkpoints = CheckpointLog::new();

        let checkpoint = checkpoints
            .emit([0xA5; 32], 4, &signing_seed)
            .expect("emit signed checkpoint")
            .clone();

        checkpoints
            .verify_signature(&checkpoint, &verification_key)
            .expect("verify ML-DSA-65 checkpoint signature");
        checkpoints.verify_chain().expect("verify checkpoint chain");
        assert_eq!(checkpoints.chain_head(), checkpoint_hash(&checkpoint));
    }

    #[test]
    fn tampered_checkpoint_is_rejected_fail_closed() {
        let (signing_seed, verification_key) = signing_keypair();
        let mut checkpoints = CheckpointLog::new();
        let mut checkpoint = checkpoints
            .emit([0x5A; 32], 9, &signing_seed)
            .expect("emit signed checkpoint")
            .clone();
        checkpoint.chain_head_hash[0] ^= 0x01;

        assert!(matches!(
            checkpoints.verify_signature(&checkpoint, &verification_key),
            Err(AuditError::InvalidCheckpointSignature { event_count: 9 })
        ));
    }

    #[test]
    fn emit_rejects_non_monotonic_event_counts_without_advancing_the_chain() {
        let (signing_seed, _) = signing_keypair();
        let mut checkpoints = CheckpointLog::new();
        checkpoints
            .emit([0x11; 32], 3, &signing_seed)
            .expect("emit first checkpoint");
        let head = checkpoints.chain_head();

        assert!(matches!(
            checkpoints.emit([0x22; 32], 3, &signing_seed),
            Err(AuditError::NonMonotonicCheckpoint { index: 1 })
        ));
        assert_eq!(checkpoints.len(), 1);
        assert_eq!(checkpoints.chain_head(), head);
    }

    #[test]
    fn verify_chain_reports_tampered_predecessor_hash() {
        let (signing_seed, _) = signing_keypair();
        let mut checkpoints = CheckpointLog::new();
        checkpoints
            .emit([0x11; 32], 3, &signing_seed)
            .expect("emit first checkpoint");
        checkpoints
            .emit([0x22; 32], 6, &signing_seed)
            .expect("emit second checkpoint");

        let expected = checkpoint_hash(&checkpoints.checkpoints[0]);
        checkpoints.checkpoints[1].prev_checkpoint_hash = [0xA5; 32];

        assert!(matches!(
            checkpoints.verify_chain(),
            Err(AuditError::ChainBroken {
                index: 1,
                expected: actual_expected,
                found: actual_found,
            }) if actual_expected == expected && actual_found == [0xA5; 32]
        ));
    }

    fn temp_path(label: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "harw-secrets-checkpoint-{label}-{}-{unique}",
            std::process::id(),
        ))
    }

    /// Mirrors `SecretStore::persist_audit_state`'s checkpoint-file writer
    /// (`store.rs`) without depending on that module, so this test module can
    /// exercise the loader on synthetic byte buffers directly — same pattern
    /// as `crate::audit::chain`'s `persisted_chain_tests::encode_persisted_log`.
    fn encode_persisted_checkpoints(checkpoints: &[Checkpoint]) -> Vec<u8> {
        let mut out = Vec::from(CHECKPOINT_MAGIC);
        out.extend_from_slice(&CHECKPOINT_FORMAT_VERSION.to_be_bytes());
        out.extend_from_slice(&(checkpoints.len() as u64).to_be_bytes());
        for checkpoint in checkpoints {
            out.extend_from_slice(&checkpoint.chain_head_hash);
            out.extend_from_slice(&checkpoint.event_count.to_be_bytes());
            out.extend_from_slice(&checkpoint.signed_at.as_second().to_be_bytes());
            out.extend_from_slice(&checkpoint.signed_at.subsec_nanosecond().to_be_bytes());
            out.extend_from_slice(&(checkpoint.signature.len() as u64).to_be_bytes());
            out.extend_from_slice(&checkpoint.signature);
            out.extend_from_slice(&checkpoint.prev_checkpoint_hash);
        }
        out
    }

    fn two_checkpoint_log(signing_seed: &SecretBox<[u8]>) -> CheckpointLog {
        let mut log = CheckpointLog::new();
        log.emit([0x11; 32], 3, signing_seed)
            .expect("emit first checkpoint");
        log.emit([0x22; 32], 6, signing_seed)
            .expect("emit second checkpoint");
        log
    }

    #[test]
    fn missing_checkpoint_file_is_reported_as_absent_not_a_break() {
        let path = temp_path("missing");

        assert!(matches!(
            load_and_verify_persisted_checkpoints(&path, u64::MAX, None),
            Ok(PersistedCheckpointStatus::Absent)
        ));
    }

    #[test]
    fn intact_persisted_checkpoints_load_and_verify_without_a_key() {
        let (signing_seed, _verification_key) = signing_keypair();
        let log = two_checkpoint_log(&signing_seed);
        let path = temp_path("intact-no-key");
        std::fs::write(&path, encode_persisted_checkpoints(log.checkpoints()))
            .expect("write test checkpoint file");

        let status = load_and_verify_persisted_checkpoints(&path, 6, None)
            .expect("structurally intact checkpoints load without a key");

        assert_eq!(
            status,
            PersistedCheckpointStatus::Intact {
                checkpoint_count: 2,
                signatures_checked: false,
            }
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn intact_persisted_checkpoints_verify_signatures_with_the_right_key() {
        let (signing_seed, verification_key) = signing_keypair();
        let log = two_checkpoint_log(&signing_seed);
        let path = temp_path("intact-with-key");
        std::fs::write(&path, encode_persisted_checkpoints(log.checkpoints()))
            .expect("write test checkpoint file");

        let status =
            load_and_verify_persisted_checkpoints(&path, 6, Some(&verification_key))
                .expect("intact, correctly signed checkpoints verify");

        assert_eq!(
            status,
            PersistedCheckpointStatus::Intact {
                checkpoint_count: 2,
                signatures_checked: true,
            }
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn wrong_verification_key_is_rejected_fail_closed() {
        let (signing_seed, _matching_key) = signing_keypair();
        let (_other_seed, mismatched_key) = signing_keypair();
        let log = two_checkpoint_log(&signing_seed);
        let path = temp_path("wrong-key");
        std::fs::write(&path, encode_persisted_checkpoints(log.checkpoints()))
            .expect("write test checkpoint file");

        let error = load_and_verify_persisted_checkpoints(&path, 6, Some(&mismatched_key))
            .expect_err("a mismatched verification key must not verify");
        assert!(matches!(
            error,
            AuditError::InvalidCheckpointSignature { event_count: 3 }
        ));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn checkpoint_beyond_the_audit_log_length_is_reported() {
        let (signing_seed, _) = signing_keypair();
        let log = two_checkpoint_log(&signing_seed);
        let path = temp_path("beyond-log");
        std::fs::write(&path, encode_persisted_checkpoints(log.checkpoints()))
            .expect("write test checkpoint file");

        // The second checkpoint claims event_count = 6; an audit log that
        // only reached event 5 means the log was truncated after that
        // checkpoint was signed.
        let error = load_and_verify_persisted_checkpoints(&path, 5, None)
            .expect_err("a checkpoint beyond the log length must not verify");
        assert!(matches!(
            error,
            AuditError::CheckpointBeyondLog {
                referenced: 6,
                actual: 5,
            }
        ));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn non_monotonic_event_counts_are_reported() {
        let (signing_seed, _) = signing_keypair();
        let mut log = CheckpointLog::new();
        log.emit([0x11; 32], 5, &signing_seed)
            .expect("emit first checkpoint");
        let first = log.checkpoints()[0].clone();

        // `CheckpointLog::emit` itself refuses a non-increasing event_count
        // (see `emit_rejects_non_monotonic_event_counts_without_advancing_the_chain`
        // above), so a non-monotonic *persisted* file can only come from a
        // tampered/rebuilt one: a second record, chained correctly onto the
        // first (so chaining alone would pass) but with the same
        // `event_count`, isolating the monotonicity violation from any
        // chaining or signature failure (`verification_key: None` below
        // skips signature checking entirely, so the reused signature bytes
        // never matching the mutated content doesn't matter here).
        let mut second = first.clone();
        second.prev_checkpoint_hash = checkpoint_hash(&first);

        let path = temp_path("non-monotonic");
        std::fs::write(&path, encode_persisted_checkpoints(&[first, second]))
            .expect("write test checkpoint file");

        let error = load_and_verify_persisted_checkpoints(&path, u64::MAX, None)
            .expect_err("non-increasing event_count values must not verify");
        assert!(matches!(
            error,
            AuditError::NonMonotonicCheckpoint { index: 1 }
        ));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn truncated_checkpoint_file_is_a_load_error_not_a_crash_or_intact() {
        let (signing_seed, _) = signing_keypair();
        let log = two_checkpoint_log(&signing_seed);
        let bytes = encode_persisted_checkpoints(log.checkpoints());
        let truncated = &bytes[..bytes.len() / 2];
        let path = temp_path("truncated");
        std::fs::write(&path, truncated).expect("write truncated test checkpoint file");

        assert!(matches!(
            load_and_verify_persisted_checkpoints(&path, u64::MAX, None),
            Err(AuditError::Io(_))
        ));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn bad_checkpoint_header_is_a_load_error() {
        let path = temp_path("bad-header");
        std::fs::write(&path, b"not-a-checkpoint-file-at-all").expect("write garbage");

        assert!(matches!(
            load_and_verify_persisted_checkpoints(&path, u64::MAX, None),
            Err(AuditError::Io(_))
        ));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn trailing_bytes_after_declared_checkpoint_count_are_a_load_error() {
        let (signing_seed, _) = signing_keypair();
        let log = two_checkpoint_log(&signing_seed);
        let mut bytes = encode_persisted_checkpoints(log.checkpoints());
        bytes.extend_from_slice(b"trailing-garbage");
        let path = temp_path("trailing");
        std::fs::write(&path, &bytes).expect("write test checkpoint file with trailing bytes");

        assert!(matches!(
            load_and_verify_persisted_checkpoints(&path, u64::MAX, None),
            Err(AuditError::Io(_))
        ));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn empty_valid_checkpoint_file_is_intact_with_zero_checkpoints() {
        let path = temp_path("empty");
        std::fs::write(&path, encode_persisted_checkpoints(&[]))
            .expect("write empty test checkpoint file");

        assert_eq!(
            load_and_verify_persisted_checkpoints(&path, 0, None)
                .expect("an empty checkpoint file is intact"),
            PersistedCheckpointStatus::Intact {
                checkpoint_count: 0,
                signatures_checked: false,
            }
        );
        let _ = std::fs::remove_file(&path);
    }

    #[cfg(unix)]
    #[test]
    fn a_checkpoint_symlink_is_refused_not_dereferenced() {
        let (signing_seed, _) = signing_keypair();
        let log = two_checkpoint_log(&signing_seed);
        let target = temp_path("symlink-target");
        std::fs::write(&target, encode_persisted_checkpoints(log.checkpoints()))
            .expect("write symlink target");
        let link = temp_path("symlink-link");
        std::os::unix::fs::symlink(&target, &link).expect("create symlink");

        assert!(matches!(
            load_and_verify_persisted_checkpoints(&link, u64::MAX, None),
            Err(AuditError::Io(_))
        ));
        let _ = std::fs::remove_file(&target);
        let _ = std::fs::remove_file(&link);
    }
}
