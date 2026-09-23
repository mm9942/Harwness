//! Blockgeräte-Zähler aus sysfs: die eine Quelle `<block-wurzel>/<gerät>/stat`,
//! die eine Fähigkeit `Capability::ReadSysfsBlock` (Knoten AW2-10).
//!
//! # Zweck
//! [`sensor::BlockioSensor`] liest die kumulativen Ein-/Ausgabezähler jedes
//! sichtbaren, nicht ausgeschlossenen Blockgeräts unterhalb der
//! Bereichswurzel und meldet sie als `harw_dod_signals::HostSample`. Genau
//! eine Quelle, genau eine Fähigkeit — siehe `harw_dod_signals::sensor`-
//! Moduldoku für das, was ein Sensor NICHT darf. Diese Crate kennt keine
//! andere Sensor-Crate (Contract-Master §G, Regel C7) und ruft nirgends
//! `std::fs` direkt auf — jeder Zugriff läuft über `harw-dod-readfs`
//! ([`harw_dod_readfs::glob::glob`], [`harw_dod_readfs::read_line_fields`]).
//!
//! # Welche Quelle — `<gerät>/stat` unter der Bereichswurzel, nicht `/proc/diskstats`
//! Zwei reale Quellen tragen dieselben Zähler in derselben Reihenfolge:
//!
//! - `/proc/diskstats` — **eine** Datei, **eine Zeile je Gerät**, mit drei
//!   vorangestellten Feldern (Major, Minor, Gerätename) vor den Zählern:
//!   ```text
//!      8       0 sda 123456 789 9876543 45678 234567 890 8765432 56789 0 34567 102467
//!    259       0 nvme0n1 98765 12 7654321 3456 87654 34 6543210 4567 0 7890 8023
//!   ```
//! - `/sys/block/<gerät>/stat` — **eine Datei je Gerät**, **eine Zeile**, nur
//!   die Zähler, ohne Major/Minor/Namensfelder (der Gerätename steht bereits
//!   im Verzeichnispfad):
//!   ```text
//!   123456 789 9876543 45678 234567 890 8765432 56789 0 34567 102467
//!   ```
//!
//! Beide sind „spaltenförmig" im Sinne von
//! [`harw_dod_readfs::read_line_fields`] — dessen eigene Moduldoku nennt
//! `/proc/diskstats` sogar wörtlich als Beispiel. Trotzdem fällt die Wahl
//! hier auf die **zweite** Form, aus einem Grund, der nichts mit dem
//! Dateiformat zu tun hat: [`harw_dod_cap::Capability`] ist ein
//! geschlossenes Enum (Vertrag Abschnitt F) ohne eine `ReadProcDiskstats`-
//! Variante — die einzige für Blockgeräte vorgesehene Fähigkeit ist
//! [`harw_dod_cap::Capability::ReadSysfsBlock`], deren eigene Dokumentation
//! sie als „Blockgeräte unter `/sys/block`" beschreibt und deren
//! [`harw_dod_cap::Capability::probe`] `"/sys/block"` liefert. Diese Crate
//! darf das Enum nicht erweitern (Regel C7: kein Schreibzugriff außerhalb
//! von `harw-dod-blockio/`), und `probe()` ist zwar laut dessen eigener
//! Dokumentation rein informativ und für keine Zugriffsentscheidung
//! bindend — aber eine Fähigkeit zu beanspruchen, deren einzige Dokumentation
//! `/sys/block` nennt, und dann tatsächlich `/proc` zu lesen, wäre eine
//! Diskrepanz zwischen deklarierter und tatsächlicher Ressource, die niemand
//! bräuchte, wenn die naheliegende Quelle (`/sys/block/<gerät>/stat`)
//! dieselben Daten trägt. Die Produktions-Bereichswurzel dieses Sensors ist
//! deshalb `/sys/block`; jedes direkte Unterverzeichnis ist ein Gerät mit
//! einer eigenen `stat`-Datei.
//!
//! Strukturell ist das außerdem die Form, die die drei anderen
//! `ReadSysfs*`-Fähigkeiten bereits etabliert haben:
//! [`harw_dod_cap::Capability::ReadSysfsThermal`] (`harw-dod-thermal`, Knoten
//! AW2-07) hat exakt dieselbe Form — eine Wurzel mit vielen
//! Geräte-/Zonenverzeichnissen, je eines mit einer eigenen Attributdatei —
//! und wird deshalb auch hier als Vorbild für die Glob-basierte
//! Geräteerkennung übernommen (siehe unten, „Warum kein
//! `#[derive(SensorSource)]`").
//!
//! # Das Zählerformat — elf Felder auf älteren Kerneln, siebzehn auf neueren
//! Beide Formen oben tragen ab dem Gerätenamen (bzw. für `<gerät>/stat`: ab
//! dem ersten Feld) dieselben Zähler in derselben Reihenfolge:
//! `read_ios read_merges read_sectors read_ticks_ms write_ios write_merges
//! write_sectors write_ticks_ms io_in_flight io_ticks_ms time_in_queue_ms`
//! — **elf** Felder seit den frühesten Kernel-Versionen, die diese Statistik
//! überhaupt führen. Kernel ab 4.18 hängen vier weitere Felder für
//! Discard-Statistik an (`discard_ios discard_merges discard_sectors
//! discard_ticks_ms`, macht **fünfzehn**), Kernel ab 5.5 zwei weitere für
//! Flush-Statistik (`flush_ios flush_ticks_ms`, macht **siebzehn**). **Das ist
//! der wichtigste Robustheitsfall dieses Sensors:** ein Parser, der auf genau
//! elf Feldern besteht, scheitert auf jedem Kernel ab 4.18 — an einer Datei,
//! die vollkommen in Ordnung ist. [`harw_dod_readfs::read_line_fields`] prüft
//! die Feldzahl bewusst **nicht**; dieser Sensor nimmt die ersten bis zu
//! siebzehn Felder, die er kennt (siehe [`sensor::FIELD_METRICS`]), verlangt
//! nur eine **Mindestzahl** von elf ([`sensor::MIN_STAT_FIELDS`]) und
//! ignoriert stillschweigend jedes Feld darüber hinaus, dessen Bedeutung er
//! nicht kennt (ein noch neuerer Kernel mit weiteren Spalten).
//!
//! # Entscheidung 1 — kumulative Zähler, keine Rate
//! Alle Zähler einer `stat`-Datei wachsen **monoton** seit dem
//! Geräteanschluss (nicht notwendig seit dem Systemstart — ein Gerät kann
//! nach dem Boot hinzukommen); sie sind keine Durchsatzangabe. Einen
//! Durchsatz (Bytes/Sekunde, IOPS) zu melden würde zwei Abrufe
//! vergleichen — also Zustand über einen Poll hinaus halten. Das verbietet
//! sich aus denselben zwei Gründen, die bereits `harw-dod-cpu` für
//! `/proc/stat` dokumentiert:
//!
//! 1. [`harw_dod_signals::Sensor::poll`] nimmt `&self`, nicht `&mut self`.
//!    Ein Sensor mit Zustand bräuchte innere Veränderlichkeit
//!    (`Mutex`/`Cell`) und verletzte damit die in der `Sensor`-Moduldoku
//!    festgehaltene Erwartung, dass gleichzeitige `poll`-Aufrufe sicher und
//!    deterministisch sind.
//! 2. Die Fixture-Harness ([`harw_dod_fixtures::sensor_suite!`]) prüft jeden
//!    Sensor gegen einen **einzelnen**, eingefrorenen `tree/`-Zustand. Ein
//!    Sensor mit Zustand über den Abruf hinaus ist gegen ein solches
//!    Fixture nicht prüfbar — die wichtigste der sechs Harness-Prüfungen,
//!    Determinismus, setzt genau das voraus.
//!
//! **Diese Crate meldet deshalb die rohen kumulativen Zähler, unverändert,
//! je gemeldetem Gerät.** Eine Rate zu bilden (Differenz durch Zeitspanne
//! zwischen zwei Beobachtungen) ist Sache des Verbrauchers (z. B.
//! `harw-dod-rules`), der ohnehin schon Zustand über mehrere Beobachtungen
//! hinweg hält.
//!
//! # Entscheidung 2 — welche Geräte: Rauschen ausschließen, Partitionen ausschließen, Anzahl begrenzen
//! `harw_dod_signals::HostSample` hat kein eigenes Label-Feld — die einzige
//! Stelle für eine Geräteunterscheidung ist der `metric`-String selbst
//! (siehe `harw-dod-thermal`, das dasselbe Problem für Zonen löst): jeder
//! gemeldete Zähler trägt den Gerätenamen als Suffix, z. B.
//! `read_ios_sda`, `write_sectors_nvme0n1` (siehe [`sensor::FIELD_METRICS`]
//! und [`sensor::sanitize_device_label`]). Ein Label je Gerät ist auf einem
//! gewöhnlichen Host mit einer Handvoll Datenträgern harmlos — aber auf
//! einem Host mit vielen Loop- oder Device-Mapper-Geräten **unbegrenzt**,
//! genau die Kardinalitätsexplosion, gegen die die `Cardinality`-Deklaration
//! (`harw-observe`, Contract-Master §A.2) existiert. Drei Teilentscheidungen:
//!
//! 1. **Virtuelle Geräte ausgeschlossen.** `loop*`, `ram*`, `zram*`, `dm-*`
//!    und `sr*` tauchen nicht, weil physische Hardware es tut, sondern weil
//!    Konfiguration es so will: ein Container-Host kann beliebig viele Loop-
//!    Geräte anlegen, ein Kernel mit `CONFIG_ZRAM` typischerweise ein
//!    Zram-Gerät je CPU-Kern (dieselbe Kardinalitätsfalle, die
//!    `harw-dod-cpu` dazu bewegt hat, nur die `cpu`-Aggregatzeile statt
//!    jeder `cpuN`-Zeile zu lesen), Device-Mapper-Geräte (LVM, LUKS) je
//!    konfiguriertem Volume. Diese Zähler sagen nichts über physische
//!    Datenträgerlast aus, die dieser Sensor beobachten soll — siehe
//!    [`sensor::is_noise_device`].
//! 2. **Keine Partitionen — strukturell, nicht per Heuristik (F-204).**
//!    Eine frühere Fassung ging davon aus, `/sys/block` liste `sda` und
//!    `sda1` (bzw. `nvme0n1` und `nvme0n1p1`) gleichermaßen als Geschwister
//!    und filterte Partitionen deshalb über eine Namens-Koexistenz-Heuristik
//!    heraus. Diese Annahme ist falsch: `/sys/block` listet ausschließlich
//!    Ganzgeräte; eine Partition erscheint immer eine Ebene *unterhalb*
//!    ihres Ganzgeräts (`/sys/block/sda/sda1`, nicht `/sys/block/sda1`). Die
//!    Heuristik erzeugte deshalb nur Fehlklassifikationen, ohne je eine
//!    echte Partition zu treffen — zum Beispiel wurde das eigenständige,
//!    physische Gerät `nvme0n10` fälschlich als „Partition 0" von `nvme0n1`
//!    ausgeschlossen. Das Glob-Muster für Geräteverzeichnisse (`*/stat`,
//!    genau eine Ebene) trifft echte Partitionen aus demselben Grund nie — dieser
//!    Sensor meldet also allein durch die Form seines Glob-Musters nur
//!    Ganzgeräte, ohne eine gesonderte Partitionsfilterung zu brauchen.
//! 3. **Anzahl hart begrenzt.** Die ersten beiden Filter schließen bekanntes
//!    Rauschen aus, verhindern aber nicht, dass ein Host mit ungewöhnlich
//!    vielen *physischen* Datenträgern (ein großes Storage-Array) die
//!    Kardinalität dennoch wachsen lässt. Diese Crate begrenzt deshalb hart
//!    auf [`sensor::MAX_DEVICES`] Geräte, alphabetisch nach Gerätename
//!    ausgewählt (deterministisch, siehe [`harw_dod_readfs::glob::glob`]s
//!    eigene Sortierzusage) — analog zu `harw-dod-cpu`s Entscheidung, nur
//!    die `cpu`-Aggregatzeile zu lesen: eine feste, dokumentierte Obergrenze
//!    statt einer bloßen Erwartung über „normale" Hosts. Die deklarierte
//!    [`sensor::MAX_CARDINALITY`] ist entsprechend
//!    `MAX_DEVICES × FIELD_METRICS.len()`.
//!
//! # Warum kein `#[derive(harw_macros::SensorSource)]`
//! `#[derive(harw_macros::SensorSource)]` deckt laut eigener Moduldoku genau
//! einen Fall ab: ein Glob-Treffer erzeugt genau **einen** Skalar unter
//! einem zur Kompilierzeit **festen, für alle Treffer identischen**
//! Metriknamen. Zwei Anforderungen dieses Knotens fehlen dort:
//!
//! 1. **Kein Label pro Treffer.** `metric` ist beim Makro ein einzelnes,
//!    für alle Treffer identisches `LitStr` — es gibt keinen Weg, aus dem
//!    Gerätenamen eines Treffers (dem Verzeichnisnamen) einen je Treffer
//!    unterschiedlichen Metriknamen abzuleiten, wie ihn Entscheidung 2 oben
//!    verlangt.
//! 2. **Kein Mehrwert-je-Treffer.** Das Makro liest je Treffer genau einen
//!    Skalar über `parse_i64`/`parse_u64` (erste Zeile einer Datei als
//!    Ganzes). `<gerät>/stat` trägt bis zu siebzehn Werte in einer Zeile —
//!    das Makro hat kein Attribut, das eine Spaltenaufteilung einer Zeile in
//!    mehrere benannte Metriken ausdrücken könnte.
//!
//! Eine dritte, unabhängige Falle (dokumentiert in `harw-dod-thermal`s
//! Moduldoku, dort ausführlich hergeleitet): das Makro reicht sein
//! `#[source(glob = "...")]`-Muster als zur Kompilierzeit **festen Text**
//! unverändert an [`harw_dod_readfs::glob::glob`] weiter, das seine Suche
//! laut eigener Moduldoku **immer bei `/`** beginnt, unabhängig vom
//! `ReadScope` — der Bereich wirkt nur als nachträglicher Filter. Ein fest
//! verdrahtetes Muster wie `"sys/block/*/stat"` fände deshalb in Produktion
//! etwas (Bereichswurzel zufällig `/sys/block`), aber in der Fixture-Prüfung
//! (Bereichswurzel = ein beliebiges `fixtures/<fall>/tree`) **nichts** — ein
//! still leeres `Ok(SensorReading { samples: vec![], events: vec![] })`,
//! nicht ein Fehler. [`sensor::BlockioSensor::poll`] umgeht das, indem es
//! sein Glob-Muster **zur Laufzeit** aus der ersten tatsächlichen
//! Bereichswurzel (`ReadScope::roots`) plus dem festen, privaten Suffix
//! `DEVICE_STAT_GLOB_SUFFIX` baut, statt den vollen Pfad fest zu
//! verdrahten — siehe die private Funktion `relative_pattern` in
//! `sensor.rs` (identisch zu der in `harw-dod-thermal`, unabhängig
//! kopiert, da C7 keine Abhängigkeit zwischen Sensor-Crates erlaubt).
//!
//! Diese Crate implementiert [`harw_dod_signals::Sensor`] deshalb **von
//! Hand** (siehe [`sensor`]).
//!
//! # Nebenläufigkeit
//! [`sensor::BlockioSensor`] hält ausschließlich einen unveränderlichen
//! `harw_dod_cap::SensorHandle`: `Send + Sync` ohne inneres Locking.
//! [`sensor::BlockioSensor::poll`] öffnet und schließt seine eigenen
//! Dateien je Aufruf; parallele Aufrufe auf derselben Instanz stören sich
//! nicht (siehe Entscheidung 1 oben).
//!
//! # Fehler
//! `harw_dod_cap::SensorError` — inhaltsfrei, kein Feldwert, kein Pfad,
//! keine gelesene Zeile erscheint in einer Fehlermeldung. Siehe
//! [`sensor::BlockioSensor::poll`] für die vollständige Fehlerbedeutung.
//!
//! # Examples
//! ```rust,no_run
//! use harw_dod_cap::scope::AliasRoot;
//! use harw_dod_cap::{Capability, ReadScope, SensorHandle};
//! use harw_dod_blockio::BlockioSensor;
//! use harw_dod_signals::Sensor;
//! use harw_types::SensorId;
//! use std::path::PathBuf;
//!
//! // `/sys/block/*`-Einträge sind Symlinks nach `/sys/devices/...` (F-005);
//! // `AliasRoot::sysfs_class` baut den Bereich, der das zulässt.
//! let alias =
//!     AliasRoot::sysfs_class(PathBuf::from("/sys/block")).expect("gültige sysfs-Klassenwurzel");
//! let scope = ReadScope::from_roots_and_aliases(Vec::new(), [alias]);
//! let handle = SensorHandle::new(SensorId::from_str("blockio-0"), Capability::ReadSysfsBlock)
//!     .bind(scope);
//! let sensor = BlockioSensor::from(handle);
//!
//! let reading = sensor.poll(jiff::Timestamp::now())?;
//! for sample in &reading.samples {
//!     println!("{}: {}", sample.metric, sample.value);
//! }
//! # Ok::<(), harw_dod_cap::SensorError>(())
//! ```

pub mod sensor;

pub use sensor::BlockioSensor;

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;
