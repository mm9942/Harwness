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
//! # Lebenszyklus der Provider-Angebote
//! [`enforce_lifecycle`] prüft eine **explizite** Modellwahl gegen die
//! eingebettete Lebenszyklus-Tabelle, **bevor** eine Wechsel-Operation den
//! Controller mutiert oder etwas persistiert: ein abgeschaltetes (retired)
//! Modell wird abgelehnt und lässt Controller, Wurzelsitzung, Kind-Route und
//! Profil-Config unverändert; ein abgekündigtes (deprecated) Modell bleibt
//! wählbar und liefert den Warntext für die Bestätigung. Die Prüfung braucht
//! keinen registrierten Dienst — Provider und Modell-ID stehen der Operation
//! bereits kanonisch aufgelöst vor.
//!
//! Abgedeckt sind alle Wechsel-/Setz-Operationen, die eine Modellwahl
//! persistieren oder in den Controller schreiben: `/model switch`,
//! `/uia-model switch`, `/models set <rolle>` (auch `uia`, `uia-worker`),
//! `/models worker` und `/uia-worker-model switch`.
//!
//! **Nicht abgedeckt** (keine Wechsel-Operationen, sie schreiben das Modell
//! unmittelbar in eine Sitzung): die Einstellung `Setting::Model` der
//! Session-Driver-Bridge (`harw-session-driver`) und die direkten Setzer des
//! SDK (`harwness-sdk`). Wer dort ein abgeschaltetes Modell setzt, wird von
//! dieser Prüfung nicht aufgehalten.
//!
//! # Nebenläufigkeit
//! Implementierungen müssen `Send + Sync` sein; Aufrufe erfolgen synchron
//! aus der Operation heraus, nie aus einem laufenden Turn.

use std::sync::Arc;

use harw_config::{InternalModelChoice, InternalModelPoint};
use harw_model_catalog::lifecycle;
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

/// Erzwingt den Lebenszyklus einer **expliziten** Modellwahl.
///
/// # Beschreibung
/// Prüft `provider`/`model` (kanonischer Provider, aufgelöste Modell-ID des
/// Katalogs) mit [`lifecycle::enforce_explicit`] gegen die eingebettete
/// Tabelle zur aktuellen Uhrzeit. Muss vor jeder Controller-Mutation und vor
/// jeder Persistenz laufen; ein Modell wird nie still ersetzt.
///
/// # Rückgabe
/// `Ok(Some(warnung))` für ein abgekündigtes (deprecated) Modell — die Wahl
/// gilt, der Aufrufer hängt die Warnung an die Bestätigung —, sonst
/// `Ok(None)`.
///
/// # Fehler
/// [`OpError::InvalidArguments`] mit Provider, Modell und den vom Provider
/// empfohlenen Ersatzmodellen, wenn das Angebot abgeschaltet (retired) ist;
/// die bisherige Auswahl bleibt aktiv.
pub(crate) fn enforce_lifecycle(provider: &str, model: &str) -> Result<Option<String>, OpError> {
    lifecycle::enforce_explicit(provider, model, lifecycle::OffsetDateTime::now_utc()).map_err(
        |error| OpError::InvalidArguments(format!("{error}; the previous model stays active")),
    )
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
