//! Der Hold-Timer-Thread: [`spawn_hold_sweeper`] (PL-100 WP 3.1,
//! Entscheidung O3).
//!
//! # Zweck
//! Ein Freeze mit signierter Haltedauer wird bei Ablauf aufgetaut — und nur
//! aufgetaut. Die Logik liegt vollständig in
//! [`harw_dod_warden::Warden::expire_holds`] (ruft ausschließlich den
//! `CgroupThawer`, nie den Isolator oder Killer, und prüft das Ziel vor dem
//! Auftauen erneut). Dieses Modul liefert nur den Takt: ein eigener Thread,
//! der sofort nach dem Start (überfällige Holds aus der Ausfallzeit) und
//! danach alle [`HOLD_SWEEP_INTERVAL`] einen Durchlauf startet und die Uhr
//! liest. Ein blockierter Timer (Sperre oder Hold-Speicher) wird nur beim
//! Zustandswechsel geloggt, nicht jede Sekunde.
//!
//! # Nebenläufigkeit
//! Der Thread teilt den `Arc<Warden>` mit den Verbindungs-Threads; der
//! Warden serialisiert Eingriffe und Holds über seine Sperre. Der Thread
//! wird nach `restrict_self()` und nach dem seccomp-Filter gestartet und
//! erbt beide (`crate::seccomp` erlaubt dafür `clone3`/`clone`,
//! `prctl(PR_SET_NAME)` für den Thread-Namen und
//! `clock_nanosleep`/`nanosleep`).
//!
//! # Fehler
//! [`WardenBinError::HoldTimerSpawn`], wenn der Thread nicht startet.

use std::sync::Arc;
use std::time::Duration;

use harw_dod_warden::{HoldSweep, Warden};

use crate::error::WardenBinError;

/// Abstand zwischen zwei Durchläufen des Hold-Timers.
pub const HOLD_SWEEP_INTERVAL: Duration = Duration::from_secs(1);

/// Starts the hold timer thread (first sweep immediately).
///
/// # Arguments
/// - `warden` (`Arc<Warden>`): der produktive Warden.
///
/// # Errors
/// - [`WardenBinError::HoldTimerSpawn`]: der Thread ließ sich nicht starten.
pub fn spawn_hold_sweeper(warden: Arc<Warden>) -> Result<(), WardenBinError> {
    std::thread::Builder::new()
        .name("hold-timer".to_owned())
        .spawn(move || sweep_forever(&warden))
        .map_err(WardenBinError::HoldTimerSpawn)?;
    Ok(())
}

// Endlosschleife des Hold-Timers: Durchlauf, dann warten. Merkt sich, ob
// der vorige Durchlauf blockiert war (Log nur bei Zustandswechsel).
fn sweep_forever(warden: &Warden) {
    let mut blocked = false;
    loop {
        blocked = log_sweep(warden.expire_holds(jiff::Timestamp::now()), blocked);
        std::thread::sleep(HOLD_SWEEP_INTERVAL);
    }
}

// Meldet einen Durchlauf, wenn er etwas getan hat. Ein blockierter Timer
// wird nur beim Wechsel gemeldet (einmal `error`, beim Ende einmal
// `info`), nicht jede Sekunde. Gibt zurück, ob dieser Durchlauf blockiert
// war.
fn log_sweep(sweep: HoldSweep, previously_blocked: bool) -> bool {
    if sweep.blocked && !previously_blocked {
        tracing::error!("hold timer could not run (enforcement lock or hold store unavailable)");
    }
    if !sweep.blocked && previously_blocked {
        tracing::info!("hold timer runs again");
    }
    if sweep.abandoned > 0 {
        tracing::error!(
            abandoned = sweep.abandoned,
            "hold timer gave up thawing; the cgroups stay frozen until a signed thaw"
        );
    }
    if sweep.thawed > 0 || sweep.dropped > 0 || sweep.retried > 0 {
        tracing::info!(
            thawed = sweep.thawed,
            dropped = sweep.dropped,
            retried = sweep.retried,
            "hold timer sweep finished"
        );
    }
    sweep.blocked
}

#[cfg(test)]
mod tests {
    use super::{HOLD_SWEEP_INTERVAL, log_sweep};
    use harw_dod_warden::HoldSweep;

    #[test]
    fn test_sweep_interval_is_short_enough_for_second_granular_holds() {
        assert!(HOLD_SWEEP_INTERVAL <= std::time::Duration::from_secs(1));
    }

    #[test]
    fn test_log_sweep_accepts_every_shape_and_tracks_the_blocked_state() {
        assert!(!log_sweep(HoldSweep::default(), false));
        let busy = HoldSweep {
            thawed: 1,
            dropped: 1,
            retried: 1,
            abandoned: 1,
            blocked: true,
        };
        assert!(log_sweep(busy, false));
        // Weiter blockiert: kein neues Log, Zustand bleibt.
        assert!(log_sweep(busy, true));
        // Wieder frei.
        assert!(!log_sweep(HoldSweep::default(), true));
    }
}
