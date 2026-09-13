//! Der ehrliche Platzhalter für [`harw_dod_warden::NetworkIsolator`]: kein
//! echtes Netz-Backend, wie im Auftrag verlangt.
//!
//! # Warum kein echtes Backend
//! `harw_dod_warden::executor`-Moduldoku, Abschnitt „Warum `NetworkIsolator`
//! keine echte Implementierung hat", dokumentiert bereits die geprüfte und
//! verworfene Option: `rustables` (die im Plan genannte Kandidaten-Crate)
//! taucht nirgends in `Cargo.lock` auf und würde eine vollständig neue
//! Abhängigkeitskette ziehen, während das Abhängigkeitsbudget dieser
//! Knotenfamilie bereits durch die mandatierte Grundausstattung von
//! `harw-dod-warden-proto` gesprengt ist. Der Auftrag dieses Knotens
//! wiederholt die Entscheidung ausdrücklich: „Nimm **keine** Netz-Crate
//! auf... Der `NetworkIsolator` der Bibliothek ist ein Trait ohne echte
//! Implementierung — bau sie hier nicht nach." Diese Datei tut das nicht.
//!
//! # Warum trotzdem eine Implementierung nötig ist
//! [`harw_dod_warden::Warden::new`] verlangt für jede der vier Aktionen
//! eine konkrete Implementierung — kein `Option<impl NetworkIsolator>`,
//! keine Auslassung. [`UnimplementedNetworkIsolator`] füllt genau diese
//! Lücke, ohne eine Wirkung vorzutäuschen: jeder Aufruf schlägt bewusst
//! fehl, geloggt, statt eine `IsolateNetwork`-Aktion stillschweigend als
//! erfolgreich zu quittieren, obwohl nichts isoliert wurde. Ein still
//! erfolgreicher Fake-Isolator wäre die gefährlichere Wahl — er würde dem
//! Aufrufer (und dem Audit-Protokoll, siehe `crate::audit`) einen Schutz
//! vortäuschen, der nicht existiert.
//!
//! # Wohin die echte Implementierung gehört
//! Voraussichtlich in ein eigenes, künftiges Crate, das
//! [`harw_dod_warden::NetworkIsolator`] gegen ein echtes Backend
//! implementiert (siehe `harw_dod_warden::executor`-Moduldoku: „Ein
//! künftiges echtes Backend implementiert `NetworkIsolator` in einem
//! eigenen Crate ... das ist ein Austausch der Implementierung, keine
//! Umschreibung dieses Traits"). `harw-warden` selbst tauscht dann nur die
//! Wiring-Stelle in `crate::warden_factory` aus.
//!
//! # Vertiefte Recherche (2026-09-01): drei geprüfte Wege, keiner tragfähig
//! Dieser Knoten hatte den Auftrag, `rustables` so hinter den Trait zu
//! schneiden, dass ein späterer Austausch gegen `netlink-packet-*` ein
//! Austausch bleibt, keine Umschreibung — und, falls das Abhängigkeitsbudget
//! es nicht zulässt, wenigstens eine cgroup-basierte Durchsetzung ganz ohne
//! neue Abhängigkeit zu prüfen. Alle drei geprüften Wege scheitern, aus
//! jeweils eigenem Grund:
//!
//! 1. **`rustables` (Fassung 0.8.8, `crates.io`-API abgefragt am
//!    2026-09-01)**: `docs.rs/crate/rustables/0.8.8/source/Cargo.toml`
//!    zeigt `[build-dependencies] bindgen = "0.72"` — `bindgen` erzeugt zur
//!    Bauzeit FFI-Bindungen gegen einen C-Header, hier den von `libnftnl`.
//!    Das verlangt `libnftnl`- und `libmnl`-Entwicklungspakete (C-Bibliotheken
//!    samt Headern) auf jeder Baumaschine — ein waschechter C-Build im
//!    Warden-Teilbaum, nicht nur eine dynamisch gelinkte Laufzeitbibliothek.
//!    Das verletzt sowohl D3 (keine unsichere FFI-Fläche gegen fremden
//!    C-Code in einem Prozess mit erhöhten Rechten) als auch das
//!    projektweite Gate „kein C-Build im Warden-Teilbaum". `nftnl`/`nftnl-sys`
//!    (der Vorgänger, von dem `rustables` abgeleitet ist) binden `libnftnl`
//!    ebenso über C-FFI ein — derselbe Befund, andere Crate.
//! 2. **`netlink-packet-route` (0.33.0) / `rtnetlink` (0.23.0)**, beide über
//!    die `crates.io`-API am 2026-09-01 abgefragt: technisch reines Rust —
//!    `docs.rs/crate/netlink-packet-route/0.33.0/source/Cargo.toml` und
//!    `.../netlink-sys/0.9.0/source/Cargo.toml` zeigen `build = false`,
//!    keinen `links`-Schlüssel, keine C-Bibliothek; nur `libc` für die
//!    rohen Socket-Syscalls, die jedes Netlink-Programm ohnehin braucht.
//!    `rtnetlink` selbst zieht darüber hinaus `netlink-packet-core`,
//!    `netlink-proto`, `netlink-sys`, `futures-channel`, `futures-util`
//!    und optional `tokio`/`async-global-executor` — eine mehrgliedrige
//!    Abhängigkeitskette, nicht ein einzelnes Blatt-Crate. Der
//!    entscheidende Einwand ist aber gar nicht das Budget, sondern die
//!    Architektur: `rtnetlink` manipuliert Links, Adressen, Routen und
//!    Netz-Namensräume — es kennt keine cgroup. Um darüber *eine bestimmte
//!    cgroup* vom Netz zu trennen, bräuchte jeder darin laufende Prozess
//!    bereits einen eigenen, dedizierten Netz-Namensraum mit eigenem
//!    `veth`, den man dann per `rtnetlink` abschaltet. Diese
//!    Namensraum-pro-cgroup-Infrastruktur existiert im Warden heute nicht
//!    (siehe `crate::warden_factory`/`CgroupV2Executor`: reine
//!    Dateisystemschreibzugriffe, kein `unshare`, kein `veth`-Aufbau). Sie
//!    einzuführen wäre keine Implementierung *hinter* diesem Trait mehr,
//!    sondern eine Umschreibung, wie Jobs überhaupt gestartet werden — die
//!    Auflage „Austausch, keine Umschreibung" wäre bereits beim ersten
//!    echten Backend gebrochen.
//! 3. **cgroup-nativ, ganz ohne neue Abhängigkeit** (die vom Auftrag
//!    explizit verlangte Prüfung von `cgroup.kill` oder einer
//!    Netz-cgroup-Steuerung): cgroup v2 hat **keine** Kontrolldatei, die
//!    Netzverkehr einer cgroup blockiert. `cgroup.kill` (das
//!    [`harw_dod_warden::executor::ProcessTreeKiller`] bereits für die
//!    *andere*, nicht verwandte Aktion `KillProcessTree` benutzt) beendet
//!    Prozesse, es trennt kein Netz. `net_cls`/`net_prio` (cgroup v1) taggen
//!    oder priorisieren Pakete, sie blocken nichts. Jede tatsächliche
//!    Durchsetzung „diese cgroup kommt nicht mehr ins Netz" läuft in der
//!    Praxis über eines von zwei Dingen: eine nftables-Regel mit
//!    `cgroup`-Match (braucht wieder eine Netzfilter-Bibliothek, siehe Weg 1
//!    — oder den `nft`-Binärprozess aufrufen, was `harw-warden` heute
//!    generell vermeidet) oder ein `BPF_CGROUP_INET_EGRESS`/`INGRESS`-Hook
//!    per `bpf(2)`-Syscall (roher `unsafe`-Syscall, oder eine Bibliothek wie
//!    `aya`, die zusätzlich einen eigenständigen eBPF-Bau-Schritt
//!    — `bpf-linker`, kompiliertes Bytecode-Objekt — und erhöhte Fähigkeiten
//!    wie `CAP_BPF`/`CAP_SYS_ADMIN` voraussetzt: eine neue Werkzeugkette,
//!    kein einzelner Abhängigkeitseintrag, und wieder `unsafe` an der
//!    Systemgrenze). Beides ist ausdrücklich ausgeschlossen (kein `unsafe`,
//!    kein zusätzlicher C-Build, kein neues Werkzeug). Der Befund aus dem
//!    Auftrag bestätigt sich: **cgroup v2 allein kann `IsolateNetwork`
//!    nicht durchsetzen.**
//!
//! ## Die Budget-Frage aus dem Auftrag
//! Selbst wenn Weg 2 (`rtnetlink`) architektonisch tragfähig wäre: die
//! Idee, die Implementierung stattdessen in `harw-dod-netpolicy` zu bauen
//! und den Warden nur eine Kante dorthin ziehen zu lassen, hilft dem
//! Warden-Budget nicht. `harw-warden/Cargo.toml` zählt heute **12 von 12**
//! direkten `[dependencies]`-Einträgen (`harw-dod-warden`,
//! `harw-dod-warden-proto`, `harw-types`, `harw-macros`, `serde`,
//! `serde_json`, `tracing`, `tracing-subscriber`, `clap`, `rustix`,
//! `landlock`, `sd-listen-fds`) — `harw-dod-netpolicy` steht heute **nicht**
//! in dieser Liste. Jede neue Kante, egal wie dünn die Ziel-Crate selbst
//! aussieht, hebt die Zahl auf **13 von 12** und reißt das Gate. Das gilt
//! unabhängig davon, was hinter `harw-dod-netpolicy::NetBackend` steckt,
//! weil das Gate an der Kante zählt, nicht am transitiven Gewicht dahinter.
//!
//! ## Ergebnis dieser Recherche
//! Keiner der drei Wege ist im aktuellen Zuschnitt (kein `unsafe`, kein
//! C-Build, kein gerissenes Budget, `NetworkIsolator`-Signatur unverändert)
//! umsetzbar. Diese Datei fügt deshalb **keine** Implementierung hinzu —
//! [`UnimplementedNetworkIsolator`] bleibt der ehrliche Platzhalter. Eine
//! Bedingung, unter der ein Weg ginge: das Abhängigkeitsbudget dieser
//! Knotenfamilie wird eigens für einen Netz-Durchsetzer-Knoten neu
//! zugeschnitten (mehr als 12 Einträge erlaubt, oder ein Eintrag daraus
//! wird freigegeben), **und** der Warden bekommt eine
//! Netz-Namensraum-pro-cgroup-Infrastruktur beim Start eines Jobs (nicht
//! nur beim Isolieren) — erst dann wäre `rtnetlink` ein echter Kandidat,
//! ohne dass „Austausch, keine Umschreibung" bricht.
//!
//! # Nebenläufigkeit
//! Zustandslos, `Send + Sync` automatisch.

use harw_dod_warden::{NetworkIsolator, WardenError, WardenResult};
use harw_types::CgroupId;

/// Lehnt jede `IsolateNetwork`-Aktion ab, bis ein echtes Backend existiert.
///
/// # Description
/// Siehe Moduldoku. Berührt nie das Netz, öffnet nie einen Socket, ruft nie
/// `nft`/Netlink auf.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnimplementedNetworkIsolator;

impl UnimplementedNetworkIsolator {
    /// Baut den Platzhalter.
    ///
    /// # Returns
    /// Einen [`UnimplementedNetworkIsolator`].
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl NetworkIsolator for UnimplementedNetworkIsolator {
    /// Lehnt die Isolation immer ab.
    ///
    /// # Description
    /// Protokolliert die abgelehnte cgroup über `tracing::error!`, bevor sie
    /// zurückgegeben wird — dieselbe Sichtbarkeit, die
    /// [`crate::audit::TracingAuditSink`] für den nachfolgenden
    /// [`harw_dod_warden::audit::AuditEvent::ExecutionFailed`]-Eintrag
    /// ohnehin liefert; diese zusätzliche Zeile macht die Ursache (kein
    /// Backend, kein Dateisystemfehler) direkt am Ausführungsort sichtbar.
    ///
    /// # Arguments
    /// - `cgroup` (`&CgroupId`): die angeforderte cgroup (nur geloggt, nie
    ///   verwendet, um irgendetwas zu öffnen).
    ///
    /// # Returns
    /// Nie `Ok`.
    ///
    /// # Errors
    /// Immer [`WardenError::Io`] mit einer festen, inhaltsfreien Meldung.
    fn isolate(&self, cgroup: &CgroupId) -> WardenResult<()> {
        tracing::error!(
            cgroup = %cgroup.as_str(),
            "network isolation requested but no real backend exists yet; refusing"
        );
        Err(WardenError::Io(std::io::Error::other(
            "network isolation backend not implemented",
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::UnimplementedNetworkIsolator;
    use harw_dod_warden::{NetworkIsolator, WardenError};
    use harw_types::CgroupId;

    #[test]
    fn test_isolate_always_fails_without_touching_the_network() {
        let isolator = UnimplementedNetworkIsolator::new();
        let cgroup = CgroupId::try_from_str("cgroup-1").expect("non-empty id");
        let err = isolator.isolate(&cgroup).unwrap_err();
        assert!(matches!(err, WardenError::Io(_)));
    }
}
