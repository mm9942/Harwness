//! Reale eBPF-Ladeschicht für diese Sonde — jetzt verdrahtet.
//!
//! # Der frühere Stand, und was sich geändert hat
//! Diese Datei lieferte zuvor immer einen Fehler „kein realer Ladeteil":
//! `harw-dod-bpf` (Knoten AW7-01a) hatte damals bewusst **keine**
//! `aya`-Abhängigkeit gezogen, mit zwei Begründungen — `SocketFilter::attach`
//! bräuchte einen bereits offenen, vom Aufrufer besessenen Socket, den eine
//! generische Ladeschicht nicht besitzen darf, und ein neuer, in dieser
//! Aufgabenform nicht per `cargo` verifizierbarer Abhängigkeitszuwachs sei im
//! Binary, das später mit `CAP_BPF` läuft, ein zu großes Risiko.
//!
//! Beide Punkte sind inzwischen bearbeitet, an der Quelle, nicht hier:
//! `harw-dod-procmon` und `harw-dod-flow` hängen beide an einem Tracepoint
//! (`sched:sched_process_exec` bzw. `sock:inet_sock_set_state`), keiner
//! braucht mehr `SocketFilter` — das erste Hindernis ist gegenstandslos.
//! `harw-dod-bpf` selbst trägt jetzt [`harw_dod_bpf::real::RealBpfLoader`],
//! eine vollständige, `aya`-gestützte [`harw_dod_bpf::BpfLoader`]-
//! Implementierung — siehe deren Moduldoku (`harw-dod-bpf/src/real.rs`) für
//! die vollständige Recherche (Fassung, `unsafe`-Fläche, `build.rs`-Frage,
//! transitive Crate-Zahl, jeweils mit Quelle) und die Begründung, warum sie
//! zwar vollständig, aber **ungefüttert** ist: sie lädt ein bereits
//! übersetztes eBPF-ELF-Objekt, erzeugt aber keines — die Objekterzeugung
//! selbst (ein `aya-ebpf`/`nightly`/`bpfel-unknown-none`-Übersetzungslauf)
//! bleibt ein eigener, künftiger Baustein.
//!
//! # Was [`build_real_loader`] heute tut
//! Baut und liefert einen [`harw_dod_bpf::real::RealBpfLoader`] — das
//! Konstruieren selbst führt **keinen** Kernel- oder Berechtigungszugriff
//! aus (siehe dessen Moduldoku, `RealBpfLoader::new`) und schlägt deshalb nie
//! fehl. Ob ein späterer `loader.load(&spec)`-Aufruf gelingt, hängt von drei
//! Dingen ab, die diese Funktion nicht beeinflusst: `CAP_BPF` auf dem Host,
//! ein tatsächlich erreichbares `BpfProgramSource` (siehe oben, „Was sich
//! geändert hat“), und ein Objekt, das dem in `RealBpfLoader`s Moduldoku
//! dokumentierten Vertrag folgt (genau ein Programm, eine Ringpuffer-Map
//! namens `"EVENTS"`).
//!
//! [`crate::sensors::build_procmon_sensor`] und
//! [`crate::sensors::build_flow_sensor`] nehmen bereits einen
//! `Box<dyn harw_dod_bpf::BpfLoader>` entgegen, unabhängig davon, ob er von
//! hier oder von einem Test-Fixture stammt — kein Aufrufer dieser Crate
//! musste sich ändern, als diese Funktion von einem dokumentierten
//! Platzhalter zu einer echten Verdrahtung wurde.
//!
//! # Exportierte Typen
//! Keine — nur die Funktion [`build_real_loader`].
//!
//! # Nebenläufigkeit
//! Zustandslos; das zurückgegebene `Box<dyn harw_dod_bpf::BpfLoader>` ist
//! `Send + Sync` (Trait-Anforderung), siehe
//! `harw_dod_bpf::real::RealBpfLoader`-Moduldoku für die Nebenläufigkeits-
//! annahme über `aya`s eigene Typen.
//!
//! # Fehler
//! Keine — [`build_real_loader`] selbst ist total. Fehler entstehen erst bei
//! einem späteren `load`/`read_events`-Aufruf auf dem zurückgegebenen Lader
//! (siehe `harw_dod_bpf::error::BpfError`).
//!
//! # Examples
//! ```rust,ignore
//! use crate::real_loader::build_real_loader;
//!
//! let loader = build_real_loader().expect("building the loader never fails");
//! ```

use harw_dod_bpf::BpfLoader;
use harw_dod_bpf::real::RealBpfLoader;

use crate::error::ProbeError;

/// Baut die reale eBPF-Ladeschicht.
///
/// # Description
/// Siehe Moduldoku für den Stand dieser Funktion und was sich seit ihrem
/// früheren, immer fehlschlagenden Platzhalter geändert hat.
///
/// # Returns
/// Einen `Box<dyn harw_dod_bpf::BpfLoader>`, gestützt auf
/// [`harw_dod_bpf::real::RealBpfLoader`].
///
/// # Errors
/// Keine — diese Funktion ist total; siehe Moduldoku.
pub fn build_real_loader() -> Result<Box<dyn BpfLoader>, ProbeError> {
    Ok(Box::new(RealBpfLoader::new()))
}

#[cfg(test)]
mod tests {
    use super::build_real_loader;

    #[test]
    fn test_build_real_loader_succeeds_and_holds_no_programs_yet() {
        // Kein Kernel-, kein Berechtigungszugriff: das Bauen selbst schlägt
        // nie fehl. Siehe Aufgabenregel „Lade in keinem Test ein echtes
        // eBPF-Programm" — dieser Test ruft `load` bewusst nicht auf.
        let loader = build_real_loader();
        assert!(loader.is_ok());
    }
}
