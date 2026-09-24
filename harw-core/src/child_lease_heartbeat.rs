//! Lease-Herzschlag laufender Kinder (Runde 5, Teil O).
//!
//! # Verantwortungsbereich
//! Die Lease eines Kindes ([`crate::child_controller::ChildLimits::lease_seconds`],
//! Vorgabe 15 min) ist ein **Lebenszeichen**, kein Zeitbudget: sie soll nur
//! ablaufen, wenn niemand mehr an dem Kind arbeitet (Prozess weg, Turn
//! verloren). Verlängert wird sie ereignisgetrieben über den
//! [`crate::guard::ProgressObserver`] — nach jeder Modellrunde und jedem
//! Werkzeugergebnis des Kindes, seit Runde 5 Teil M auch für alle
//! admittierten Vorfahren. Das deckt zwei Fälle nicht ab:
//!
//! - Ein einzelnes langes Werkzeug (Build, Testlauf, Warten auf einen
//!   langsamen Provider) erzeugt bis zu seinem Ende **kein**
//!   Fortschrittsereignis; nach 15 min räumte der Reaper
//!   (`ManagedAgentSpawner::reap` → `reap_expired`) das noch arbeitende Kind
//!   ab, brach es mit `CancelReason::LeaseLost` ab und markierte seine
//!   Sitzung als gescheitert.
//! - Der durable Lease-Store wird vom Fortschrittsbeobachter nicht
//!   verlängert (nur die In-Memory-Registry).
//!
//! Dieses Modul ergänzt deshalb — kein zweiter Weg für dieselben
//! Ereignisse, sondern ein Takt für die Zeit **zwischen** ihnen — einen
//! Herzschlag: solange das Future eines Kind-Laufs lebt
//! ([`with_lease_heartbeat`]), wird die Lease des Kindes und aller
//! admittierten Vorfahren in festem Takt ([`heartbeat_interval`], ein Drittel
//! der Lease-Dauer) über [`ManagedAgentSpawner::renew_lease`] verlängert —
//! damit auch durabel.
//!
//! # Abgrenzung zum Zeitbudget
//! Die Wanduhrgrenze eines Kindes (`[spawn.budget] max_wall_secs` →
//! `AgentBudget::max_wall_time_ms`) bleibt davon völlig unberührt: sie wird
//! weiterhin von `enforce_child_budget` mit `tokio::time::timeout`
//! durchgesetzt. Die Lease läuft also nie mehr mitten in aktiver Arbeit ab,
//! ein festhängendes Kind endet trotzdem an seinem Zeitbudget.
//!
//! # Nebenläufigkeit
//! Der Herzschlag läuft im selben Task wie das Kind-Future (`tokio::select!`),
//! hält keine Sperre über einen `await` und endet mit dem Future.

use std::future::Future;
use std::time::Duration;

use harw_types::SessionId;
use jiff::Timestamp;

use crate::child_controller::ManagedAgentSpawner;

/// Kürzester Herzschlag-Abstand, auch bei sehr kurzen Test-Leases.
pub const LEASE_HEARTBEAT_MIN_INTERVAL: Duration = Duration::from_secs(1);

/// Obergrenze der Vorfahren-Kette, die ein Herzschlag verlängert.
///
/// Schutz gegen einen (nie erwarteten) Zyklus in den Admission-Records; die
/// echte Tiefe liegt weit darunter (`ChildLimits::max_depth`).
const MAX_ANCESTOR_HOPS: usize = 64;

/// Abstand zwischen zwei Herzschlägen für eine Lease von `lease_seconds`.
///
/// # Beschreibung
/// Ein Drittel der Lease-Dauer: selbst wenn ein Herzschlag verspätet
/// eintrifft (voll ausgelasteter Executor), bleiben zwei weitere, bevor die
/// Lease abläuft. Nie kürzer als [`LEASE_HEARTBEAT_MIN_INTERVAL`].
///
/// # Argumente
/// - `lease_seconds` (`i64`): die konfigurierte Lease-Dauer; `<= 0` ergibt
///   den Mindestabstand.
///
/// # Rückgabe
/// Der Takt als [`Duration`].
#[must_use]
pub fn heartbeat_interval(lease_seconds: i64) -> Duration {
    let third = u64::try_from(lease_seconds).unwrap_or(0) / 3;
    Duration::from_secs(third).max(LEASE_HEARTBEAT_MIN_INTERVAL)
}

/// Verlängert die Lease von `child` und aller admittierten Vorfahren.
///
/// # Beschreibung
/// Läuft die Elternkette über die Admission-Records hoch, bis ein Elternteil
/// kein admittiertes Kind mehr ist (die Wurzel). Bereits abgelaufene Leases
/// werden dabei nicht wiederbelebt (Regel von
/// [`ManagedAgentSpawner::renew_lease`]).
///
/// # Argumente
/// - `spawner` (`&ManagedAgentSpawner`): der Spawner des Laufs.
/// - `child` (`&SessionId`): das arbeitende Kind.
/// - `now` (`Timestamp`): Referenzzeitpunkt.
///
/// # Rückgabe
/// Anzahl der verlängerten Sitzungen (Kind plus Vorfahren).
pub fn renew_lease_chain(
    spawner: &ManagedAgentSpawner,
    child: &SessionId,
    now: Timestamp,
) -> usize {
    let mut renewed = 0_usize;
    let mut cursor = child.clone();
    for _ in 0..MAX_ANCESTOR_HOPS {
        let Some(record) = spawner.child_record(&cursor) else {
            break;
        };
        if let Err(error) = spawner.renew_lease(&cursor, now) {
            tracing::warn!(child = %cursor, error = %error, "child_lease_heartbeat.renew_failed");
        }
        renewed = renewed.saturating_add(1);
        cursor = record.parent;
    }
    renewed
}

/// Treibt `future` und verlängert währenddessen die Lease von `child`.
///
/// # Beschreibung
/// Der erste Herzschlag fällt einen [`heartbeat_interval`] nach dem Start
/// (direkt nach der Admission ist die Lease ohnehin frisch). Endet das
/// Future, endet der Herzschlag im selben Moment.
///
/// # Argumente
/// - `spawner` (`&ManagedAgentSpawner`): der Spawner des Laufs.
/// - `child` (`&SessionId`): das laufende Kind.
/// - `future` (`F`): der eigentliche Kind-Lauf.
///
/// # Rückgabe
/// Das Ergebnis von `future`, unverändert.
pub async fn with_lease_heartbeat<F>(
    spawner: &ManagedAgentSpawner,
    child: &SessionId,
    future: F,
) -> F::Output
where
    F: Future,
{
    let interval = heartbeat_interval(spawner.limits().lease_seconds);
    let mut ticker = tokio::time::interval_at(tokio::time::Instant::now() + interval, interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tokio::pin!(future);
    loop {
        tokio::select! {
            biased;
            output = &mut future => return output,
            _ = ticker.tick() => {
                let renewed = renew_lease_chain(spawner, child, Timestamp::now());
                tracing::trace!(child = %child, renewed, "child_lease_heartbeat.tick");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_heartbeat_beats_three_times_per_lease() {
        assert_eq!(heartbeat_interval(15 * 60), Duration::from_secs(300));
        assert_eq!(heartbeat_interval(90), Duration::from_secs(30));
    }

    #[test]
    fn the_heartbeat_never_beats_faster_than_the_minimum() {
        assert_eq!(heartbeat_interval(0), LEASE_HEARTBEAT_MIN_INTERVAL);
        assert_eq!(heartbeat_interval(-5), LEASE_HEARTBEAT_MIN_INTERVAL);
        assert_eq!(heartbeat_interval(2), LEASE_HEARTBEAT_MIN_INTERVAL);
    }
}
