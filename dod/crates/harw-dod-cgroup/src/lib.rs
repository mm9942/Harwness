//! cgroup-v2-Zähler unterhalb der Bereichswurzel — die eine Quelle
//! `/sys/fs/cgroup`, die eine Fähigkeit
//! `harw_dod_cap::Capability::ReadCgroupV2` (Knoten AW2-13, siebter und
//! letzter Strom-A-Sensor).
//!
//! # Zweck
//! [`CgroupSensor`] liest jede sichtbare cgroup **direkt unterhalb** der
//! Bereichswurzel und meldet vier Zähler je cgroup als
//! `harw_dod_signals::HostSample`. Genau eine Quelle, genau eine Fähigkeit —
//! siehe `harw_dod_signals::sensor`-Moduldoku für das, was ein Sensor NICHT
//! darf. Diese Crate kennt keine andere Sensor-Crate (Contract-Master §G,
//! Regel C7) und ruft nirgends `std::fs` direkt auf — jeder Zugriff läuft
//! über `harw-dod-readfs`.
//!
//! # Welche Ebene: nur die direkten Kinder der Bereichswurzel
//! cgroup-v2 ist ein beliebig tief verschachtelter Baum
//! (`system.slice/docker-<id>.scope/init` etwa drei Ebenen tief). Der einzige
//! Musterabgleich, der dieser Crate zur Verfügung steht
//! (`harw_dod_readfs::glob::glob`), kennt laut eigener Moduldoku **kein**
//! `**` — nur `*`/`?` auf einer einzelnen Pfadkomponente. Ein Sensor, der
//! beliebige Tiefe erschließen wollte, müsste selbst rekursiv
//! Verzeichnisse auflisten — die eine dokumentierte Ausnahme von „kein
//! `std::fs` außerhalb des Bereichs" gehört `glob::glob` allein (siehe
//! dessen Moduldoku), keiner Sensor-Crate.
//!
//! **Entscheidung:** Dieser Sensor liest deshalb ausschließlich die cgroups,
//! die **direkte Kinder** der Bereichswurzel sind — ein Aufrufer, der tiefer
//! verschachtelte Kind-cgroups überwachen will, richtet die Bereichswurzel
//! entsprechend tiefer ein (z. B. auf `/sys/fs/cgroup/system.slice`, um
//! dessen Kinder zu sehen), statt dass dieser Sensor selbst rekursiert. Das
//! ist dieselbe Form wie bei `harw-dod-thermal` (ein Glob-Suffix,
//! `thermal_zone*`, genau eine Ebene) und `harw-dod-netcounters`
//! (`/proc/net/dev`-Zeilen, keine Rekursion) — kein Sensor in diesem
//! Workspace läuft rekursiv über einen Verzeichnisbaum.
//!
//! Der Root-cgroup der Bereichswurzel selbst (ihre eigenen Kontrolldateien,
//! z. B. `<wurzel>/memory.current`) wird **nicht** gemeldet — nur ihre
//! Kind-Verzeichnisse. `*` matcht auf derselben Ebene gleichermaßen echte
//! Kind-Verzeichnisse **und** die Kontrolldateien der Wurzel selbst
//! (`cgroup.controllers`, `cpu.stat`, `memory.stat`, …) — beide liegen
//! direkt unterhalb der Bereichswurzel.
//!
//! **F-095 (behoben):** eine frühere Fassung verließ sich darauf, dass ein
//! solcher Kandidat beim späteren Lesen für alle vier Kontrolldateien
//! `ENOTDIR` und damit `None` liefert, also implizit keinen `HostSample`
//! erzeugt — filterte dabei aber **nach** der Kürzung auf
//! [`sensor::MAX_CGROUPS`]. Die Kontrolldateien der Wurzel sortieren
//! alphabetisch früh (`cgroup.*`, `cpu.*`, `memory.*`, …) und belegten so
//! auf einem `systemd`-Host praktisch alle 16 Plätze, bevor `system.slice`/
//! `user.slice` je an die Reihe kamen — die reale Quelle blieb dauerhaft
//! unsichtbar, ohne dass ein Fehler das anzeigte. [`sensor`] filtert
//! deshalb jetzt explizit **vor** der Kürzung: ein Kandidat gilt als echte
//! cgroup, wenn unter ihm `cgroup.controllers` lesbar ist — ein Attribut,
//! das laut cgroup-v2-Kontrakt jede cgroup (Wurzel wie Kind, unabhängig von
//! aktivierten Controllern) trägt, eine Kontrolldatei der Wurzel dagegen
//! nie (siehe `sensor::is_child_cgroup_dir` für die Details).
//!
//! # Welche Kontrolldateien, und warum genau diese vier
//! Von den vielen Kontrolldateien einer cgroup-v2-Hierarchie liest dieser
//! Sensor **vier**, jede mit einer eigenen Sicherheitsaussage — *läuft hier
//! etwas aus dem Ruder, und seit wann?* Alles andere (`cgroup.procs`,
//! `memory.stat`, `io.stat`, …) bleibt bewusst außen vor, analog zu
//! `harw-dod-gpu`, das sich ausdrücklich auf vier Werte beschränkt hat,
//! statt spekulativ zu verbreitern:
//!
//! - **`memory.current`** — aktueller Speicherverbrauch in Byte. Die
//!   unmittelbarste „läuft etwas aus dem Ruder"-Frage: ein Wert, der über
//!   mehrere Polls hinweg wächst, ist ein Speicherleck oder ein
//!   außer Kontrolle geratener Prozess in dieser cgroup.
//! - **`memory.max`** — das konfigurierte Speicherlimit dieser cgroup, in
//!   Byte. Ohne dieses Limit ist `memory.current` allein nicht zu
//!   interpretieren: 500 MiB sind harmlos bei einem 2-GiB-Limit, aber ein
//!   akutes Problem bei einem 512-MiB-Limit. Siehe unten zur Behandlung von
//!   `max` als gültigem Wert.
//! - **`pids.current`** — Zahl der Prozesse/Threads in dieser cgroup. Die
//!   klassische Fork-Bomb-Kennzahl: eine cgroup, deren `pids.current`
//!   sprunghaft wächst, zeigt eine unkontrollierte Prozessvermehrung an,
//!   unabhängig davon, wie viel Speicher oder CPU die einzelnen Prozesse
//!   verbrauchen.
//! - **`cpu.stat`**, Feld `usage_usec` — kumulierte CPU-Zeit dieser cgroup in
//!   Mikrosekunden seit ihrer Erzeugung. Analog zu `harw-dod-cpu`s
//!   `/proc/stat`-Zählern: ein monoton wachsender, kumulativer Zähler, kein
//!   Prozentsatz (siehe Entscheidung unten).
//!
//! **Bewusst nicht gelesen:** `cgroup.procs` (Prozess-IDs — dieser Sensor
//! meldet Zähler, nicht was in einer cgroup läuft, siehe Abschnitt
//! „Redaktion" unten), `memory.stat` (viel feinkörniger als hier gebraucht),
//! `io.stat` (eigene Sicherheitsaussage, gehört eher zu einem
//! Block-E/A-Sensor wie `harw-dod-blockio`).
//!
//! # Entscheidung 1 — kumulative Zähler, keine Rate
//! Wie `harw-dod-cpu`s `/proc/stat`-Zähler sind `memory.current`,
//! `pids.current` und `cpu.stat`s `usage_usec` **Momentaufnahmen bzw.
//! kumulative Zähler**, keine Raten. [`harw_dod_signals::Sensor::poll`]
//! nimmt `&self`, nicht `&mut self` — ein Sensor, der eine Rate bilden
//! wollte (Differenz zweier Polls durch die verstrichene Zeit), bräuchte
//! innere Veränderlichkeit und wäre gegen die Fixture-Harness'
//! Determinismus-Prüfung (zwei Polls mit demselben `now` müssen exakt
//! gleich sein) nicht mehr prüfbar. Die Bildung einer Rate ist deshalb
//! bewusst Sache des Verbrauchers (`harw-dod-rules`), der ohnehin schon
//! Zustand über mehrere Beobachtungen hinweg hält.
//!
//! # Entscheidung 2 — `max` ist ein gültiger Wert, kein Fehler
//! `memory.max` trägt entweder eine Ganzzahl oder das wörtliche Literal
//! `max`, wenn für diese cgroup **kein** Speicherlimit gesetzt ist — beides
//! sind in cgroup-v2 gleichermaßen gültige Inhalte derselben Datei. Ein
//! `harw_dod_readfs::parse_u64` auf `max` ergäbe fälschlich
//! `SensorError::MalformedSource`, obwohl die Datei genau das enthält, was
//! ein Kernel ohne konfiguriertes Limit dort hinterlegt — kein
//! Formatfehler, sondern die dokumentierte Kernel-Kodierung für „kein
//! Limit".
//!
//! **Entscheidung:** [`sensor::CgroupSensor::poll`] erkennt den getrimmten
//! Inhalt `"max"` ausdrücklich und meldet ihn als `u64::MAX` (als `f64`:
//! `18446744073709551616.0` — der nächstliegende `f64`-Wert, da `u64::MAX`
//! selbst nicht exakt in 53 Bit Mantisse passt). Die Alternative — den Wert
//! als IEEE-754-`+Infinity` zu melden — wäre semantisch treffender, scheitert
//! aber praktisch: `harw_dod_signals::HostSample` (und mit ihm
//! `harw_dod_fixtures`s `expect.json`) wird über `serde_json` serialisiert,
//! und JSON kennt keine Unendlichkeit — `serde_json` kodiert einen
//! nicht-endlichen `f64` als `null`, was das `expect.json`-Format dieser
//! Crate ohne Sonderbehandlung nicht rund-trip-fähig machen würde. Ein
//! `u64::MAX`-Stellvertreter ist zudem kein Novum: cgroup-v1s
//! `memory.limit_in_bytes` kodierte „kein Limit" historisch bereits als eine
//! sehr große, aber endliche Zahl (`9223372036854771712`), nicht als
//! Unendlichkeit. Ein Verbraucher erkennt „kein Limit" an einem Wert nahe
//! `u64::MAX`, dokumentiert hier als die eine, stabile Konvention dieses
//! Sensors.
//!
//! # Entscheidung 3 — fehlende Kontrolldatei vs. vorhandene, aber
//! fehlerhafte Kontrolldatei
//! cgroup-v2-Controller sind **pro Teilbaum opt-in**
//! (`cgroup.subtree_control`): eine cgroup, für die der `pids`- oder
//! `memory`-Controller nicht aktiviert ist, hat schlicht keine
//! `pids.current`- bzw. `memory.current`-Datei — das ist der gesunde
//! Normalfall, keine fehlerhafte Quelle. Anders als `harw-dod-thermal`
//! (jede sichtbare `thermal_zone*` hat garantiert eine `temp`-Datei) kann
//! dieser Sensor sich also **nicht** darauf verlassen, dass eine gefundene
//! cgroup alle vier Kontrolldateien trägt.
//!
//! **Entscheidung, asymmetrisch:**
//! - **Fehlt** eine Kontrolldatei vollständig, wird **nur diese eine
//!   Metrik** für diese eine cgroup ausgelassen — kein Fehler, keine
//!   Auswirkung auf andere Metriken derselben oder anderer cgroups.
//! - **Existiert** eine Kontrolldatei, ihr Inhalt ist aber nicht die
//!   erwartete Form (leer, nicht numerisch, kein `usage_usec`-Feld in
//!   `cpu.stat`), wird ebenfalls **nur diese eine Metrik** ausgelassen — der
//!   Fehler wird gesammelt (F-095, „Teilergebnisse statt Totalausfall"),
//!   aber nicht sofort propagiert: andere cgroups und andere Metriken
//!   derselben cgroup werden weiter gelesen. Erst wenn am Ende **keine
//!   einzige** Probe zustande kam, meldet [`sensor::CgroupSensor::poll`]
//!   einen der gesammelten Fehler (typischerweise
//!   `SensorError::MalformedSource`) statt eines leeren `Ok`. Anders als in
//!   einer früheren Fassung dieser Crate bricht eine einzelne fehlerhaft
//!   geformte Kontrolldatei damit nicht mehr den gesamten Abruf für alle
//!   anderen, gesunden cgroups ab — ein Host mit hunderten cgroups soll
//!   nicht wegen einer einzigen defekten Quelle blind werden.
//!
//! # Kardinalität — eine harte Obergrenze, alphabetisch sortiert vor dem
//! Kürzen
//! cgroups sind der Fall mit dem größten Explosionspotenzial im gesamten
//! Sensorsatz: ein Host mit vielen Containern hat potenziell hunderte
//! Kind-cgroups, und ihre Namen sind **angreiferkontrolliert** — ein
//! Containername kommt von dem, der den Container startet.
//! [`sensor::MAX_CGROUPS`] begrenzt die je Poll berücksichtigten cgroups auf
//! **16** (dieselbe Größenordnung wie `harw-dod-netcounters`s
//! `MAX_INTERFACES` und `harw-dod-thermal`s `max_cardinality`) — zuerst auf
//! echte Kind-cgroups gefiltert (F-095, siehe oben), erst danach nach Namen
//! sortiert **vor** dem Kürzen, damit die Auswahl deterministisch ist und
//! ein Sortierfehler sofort sichtbar würde (siehe
//! [`sensor::cap_and_sort_cgroups`]). Mit vier Metriken je cgroup ergibt das
//! eine feste Obergrenze von [`sensor::MAX_CARDINALITY`] `= 64`
//! verschiedenen Labelkombinationen je Poll, unabhängig davon, wie viele
//! Container tatsächlich auf dem Host laufen.
//!
//! # cgroup-Namen werden bereinigt
//! Ein cgroup-Name landet unmittelbar in einem Metriknamen (analog zu
//! `harw-dod-thermal`s Zonenlabel und `harw-dod-netcounters`s
//! Schnittstellenlabel) — und ist, wie oben festgehalten, vollständig
//! angreiferkontrolliert. [`sensor::sanitize_label`] reduziert ihn auf
//! `[a-z0-9_]`, gekürzt auf 64 Zeichen: jedes andere Zeichen (Punkte,
//! Semikola, Anführungszeichen, Leerzeichen — alles, was ein
//! Linux-Verzeichnisname außer `/` und `NUL` tragen darf) wird durch `_`
//! ersetzt. Getestet mit einem absichtlich feindlichen, adressartigen Namen
//! (`fixtures/hostile-name`) — siehe `harw-dod-netcounters`s
//! `sanitize_label`-Test mit einer gepunkteten IPv4-Adresse als Vorbild für
//! dieses Muster.
//!
//! # Warum kein `#[derive(harw_macros::SensorSource)]`
//! Das Makro kennt genau **einen** Glob, **einen** Skalar je Treffer, einen
//! zur Kompilierzeit festen Metriknamen, keine Umrechnung und kein Label je
//! Treffer. Dieser Sensor braucht **vier** verschiedene Kontrolldateien je
//! Treffer, ein Label je Treffer (der bereinigte cgroup-Name) und eine
//! Sonderbehandlung für den Wert `max` — drei Anforderungen, die das Makro
//! nicht abdeckt, dieselbe Lücke, die bereits `harw-dod-thermal` (Label,
//! Umrechnung) und `harw-dod-netcounters` (mehrere Metriken, Label) zur
//! handgeschriebenen `Sensor`-Implementierung gezwungen haben. [`sensor`]
//! implementiert `Sensor` deshalb von Hand.
//!
//! # Nebenläufigkeit
//! [`CgroupSensor`] hält keinen veränderlichen Zustand: `Send + Sync`
//! automatisch, `poll` nimmt `&self` und ist aus mehreren Threads
//! gleichzeitig sicher aufrufbar (siehe Entscheidung 1 oben).
//!
//! # Fehler
//! [`harw_dod_cap::SensorError`] — inhaltsfrei. Diese Crate definiert
//! keinen eigenen Fehlertyp: jeder Fehlerpfad in
//! [`sensor::CgroupSensor::poll`] mündet in eine der vier bestehenden
//! `SensorError`-Varianten.
//!
//! # Examples
//! ```rust,no_run
//! use harw_dod_cap::{Capability, ReadScope, SensorHandle};
//! use harw_dod_cgroup::CgroupSensor;
//! use harw_dod_signals::Sensor;
//! use harw_types::SensorId;
//! use std::path::PathBuf;
//!
//! let scope = ReadScope::from_roots([PathBuf::from("/sys/fs/cgroup")]);
//! let handle = SensorHandle::new(SensorId::from_str("cgroup-0"), Capability::ReadCgroupV2)
//!     .bind(scope);
//! let sensor = CgroupSensor::from(handle);
//!
//! let reading = sensor.poll(jiff::Timestamp::now())?;
//! for sample in &reading.samples {
//!     println!("{}: {}", sample.metric, sample.value);
//! }
//! # Ok::<(), harw_dod_cap::SensorError>(())
//! ```

pub mod sensor;

pub use sensor::CgroupSensor;

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;
