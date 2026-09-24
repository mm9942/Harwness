//! Live-Übernahme von Modellwechseln in die laufende Sitzung.
//!
//! # Verantwortungsbereich
//! Die Wechsel-Operationen (`/model switch`, `/uia-model switch`,
//! `/models set|reset`) schreiben ihre Wahl in den
//! [`harw_operations::SharedSessionController`] bzw. in die Profil-Config.
//! Was die **laufende** Montage dafür zusätzlich tun muss — einen beim
//! Start nicht baubaren Provider-Client jetzt bauen, die Rollenwahl für neu
//! gestartete Kind-Agenten übernehmen —, kann `harw-ops` nicht selbst: die
//! Montage liegt in `harw-runtime`, das von diesem Crate abhängt. Dieser
//! Dienst ist die Naht dafür; die Runtime legt ihre Implementierung als
//! [`SharedLiveModelControl`] in die Slash-`ServiceMap`.
//!
//! Ohne registrierten Dienst (Tests, fremde Kompositionen) verhalten sich
//! alle Operationen wie bisher.
//!
//! # Nebenläufigkeit
//! Implementierungen müssen `Send + Sync` sein; Aufrufe erfolgen synchron
//! aus der Operation heraus, nie aus einem laufenden Turn.

use std::sync::Arc;

use harw_config::{InternalModelChoice, InternalModelPoint};
use harw_operations::{OpContext, OpError};

/// Laufzeit-Seite eines Modellwechsels (siehe Moduldoku).
pub trait LiveModelControl: Send + Sync {
    /// Stellt sicher, dass der Provider-Client von `provider` nutzbar ist.
    ///
    /// # Beschreibung
    /// Ein beim Start gescheiterter Client (z. B. fehlende Zugangsdaten)
    /// wird über den regulären Provider-Bau neu gebaut und eingesetzt.
    ///
    /// # Fehler
    /// `Err(grund)` ohne Geheimnisinhalt, wenn der Client auch jetzt nicht
    /// gebaut werden kann — der Aufrufer muss den Wechsel dann ablehnen.
    fn ensure_provider_ready(&self, provider: &str) -> Result<(), String>;

    /// Übernimmt die Wahl einer internen Modellstelle für künftig
    /// gestartete Kind-Agenten (`None` = zurück auf den Standard der Stelle).
    ///
    /// # Rückgabe
    /// `true`, wenn die Stelle in dieser Sitzung live wirkt; `false`, wenn
    /// sie erst ab der nächsten Sitzung gilt (z. B. Sitzungstitel).
    fn set_internal_model(
        &self,
        point: InternalModelPoint,
        choice: Option<InternalModelChoice>,
    ) -> bool;
}

/// Geteilter Handle, unter dem die Runtime den Dienst registriert.
pub type SharedLiveModelControl = Arc<dyn LiveModelControl>;

/// Prüft über den registrierten Dienst, ob `provider` jetzt nutzbar ist.
///
/// # Fehler
/// [`OpError::InvalidArguments`] mit Provider und Grund; ohne Dienst `Ok`.
pub(crate) fn ensure_provider_ready(ctx: &OpContext, provider: &str) -> Result<(), OpError> {
    let Some(live) = ctx.service::<SharedLiveModelControl>() else {
        return Ok(());
    };
    live.ensure_provider_ready(provider).map_err(|reason| {
        OpError::InvalidArguments(format!(
            "provider {provider}: client could not be built ({reason}); \
             the previous model stays active"
        ))
    })
}

/// Meldet eine Rollenwahl an den registrierten Dienst.
///
/// # Rückgabe
/// `true`, wenn sie live wirkt; ohne Dienst `false`.
pub(crate) fn apply_internal_model(
    ctx: &OpContext,
    point: InternalModelPoint,
    choice: Option<InternalModelChoice>,
) -> bool {
    ctx.service::<SharedLiveModelControl>()
        .is_some_and(|live| live.set_internal_model(point, choice))
}
