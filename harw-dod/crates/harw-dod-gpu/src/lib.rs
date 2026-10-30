//! GPU-Zustand aus sysfs: die eine Quelle `/sys/class/drm`, die eine
//! Fähigkeit `Capability::ReadSysfsDrm` (Knoten AW2-12).
//!
//! # Zweck
//! [`GpuSensor`] liest jede sichtbare Grafikkarte unterhalb der Bereichswurzel
//! und meldet, was über sysfs sichtbar ist, als
//! `harw_dod_signals::HostSample`. Genau eine Quelle, genau eine Fähigkeit —
//! siehe `harw_dod_signals::sensor`-Moduldoku für das, was ein Sensor NICHT
//! darf. Diese Crate kennt keine andere Sensor-Crate (Contract-Master §G,
//! Regel C7) und ruft nirgends `std::fs` direkt auf — jeder Zugriff läuft
//! über `harw-dod-readfs` (siehe `sensor`-Moduldoku für die Details).
//!
//! # sysfs-Format
//! Jede Karte ist ein eigenes Verzeichnis unterhalb der Bereichswurzel
//! (real: `/sys/class/drm`), typischerweise `card0`, `card1`, ... Die für
//! diese Crate relevanten Dateien liegen darunter, in `device/`:
//!
//! - `device/gpu_busy_percent` — Auslastung in Prozent (amdgpu).
//! - `device/mem_info_vram_used`, `device/mem_info_vram_total` — belegter
//!   bzw. gesamter Grafikspeicher in Bytes (amdgpu).
//! - `device/hwmon/hwmon*/temp1_input` — Temperatur in **Millidegree
//!   Celsius**, wie bei den Thermalzonen (`harw-dod-thermal`).
//!
//! Jede dieser vier Dateien ist für sich **optional**: welche davon
//! existieren, hängt vom Treiber ab (siehe Abschnitt „Herstellerabhängige
//! Fläche" unten). [`GpuSensor::poll`] meldet für jede Karte genau die
//! Metriken, deren Datei tatsächlich lesbar war, und lässt den Abruf nicht
//! scheitern, nur weil eine oder mehrere dieser Dateien fehlen.
//!
//! # Ein Host ohne GPU ist der Normalfall — nicht `SourceUnavailable`
//! Die meisten Hosts, auf denen der Harness läuft (Server, CI-Läufer,
//! Container), haben **keine** GPU. [`GpuSensor::poll`] meldet das als
//! **leeres, erfolgreiches** `SensorReading` (`Ok(SensorReading { samples:
//! vec![], events: vec![] })`), **nicht** als
//! [`harw_dod_cap::SensorError::SourceUnavailable`] und erst recht nicht als
//! Abbruch. Das ist die zentrale Entscheidung dieser Crate, und sie
//! widerspricht bewusst der Konvention jeder anderen Sensor-Crate in diesem
//! Ausbauprogramm: `harw-dod-thermal` und `harw-dod-cpu` melden eine leere
//! Quelle (keine Thermalzone, kein `cpu`-Zeileneintrag) als
//! `SourceUnavailable` — dort ist eine leere Quelle tatsächlich ein
//! Anzeichen für einen kaputten oder falsch konfigurierten Bereich, weil
//! *jeder* Linux-Host Thermalzonen und `/proc/stat` hat. Für eine GPU gilt
//! das nicht: die Abwesenheit ist die häufigste, gesündeste Beobachtung.
//!
//! **Ein Sensor, der auf jedem GPU-losen Host mit einem Fehler scheitert,
//! wird vom Sentinel nach [`harw_dod_cap::Permanence::Permanent`] abgemeldet
//! — und dann fehlen mit ihm auch alle anderen zehn Sensoren, die
//! unabhängig von ihm hätten weiterlaufen können, sobald ein einzelner
//! fehlschlagender Pflicht-Sensor die gesamte Sentinel-Instanz als degradiert
//! markiert.** Das ist der Fehler, den diese Crate vermeidet.
//!
//! **Aber:** eine Karte, die gefunden wurde (das Verzeichnis existiert), bei
//! der aber ein einzelner Lesevorgang mit einem anderen Fehler als „Datei
//! fehlt" scheitert — etwa Berechtigung verweigert — ist etwas anderes als
//! „keine GPU". [`GpuSensor::poll`] unterscheidet das: eine fehlende Datei
//! (`std::io::ErrorKind::NotFound`) gilt als „diese Metrik exponiert dieser
//! Treiber nicht" und wird ohne Fehler übersprungen; jeder andere
//! E/A-Fehler auf einer gefundenen Kartendatei meldet
//! [`harw_dod_cap::SensorError::Io`] und bricht den Abruf ab, statt ihn wie
//! eine bloß fehlende Datei stillschweigend zu behandeln. Eine Karte mit
//! unlesbaren, aber vorhandenen Dateien ist ein echtes Problem (kaputte
//! Berechtigungen, ein degradierter Treiber) und darf nicht wie „keine GPU"
//! aussehen. Eine Datei, die zwar lesbar ist, aber einen nicht parsbaren
//! Inhalt trägt (leer, nicht-numerisch), ist wiederum ein dritter Fall:
//! [`harw_dod_cap::SensorError::MalformedSource`] — die Quelle existiert,
//! hat aber nicht die erwartete Form, und das ist so unerwartet, dass der
//! gesamte Abruf abbricht statt die defekte Karte stillschweigend
//! auszulassen (siehe [`GpuSensor::poll`] für die genaue Zuordnung).
//!
//! # Herstellerabhängige Fläche — eine ehrliche Grenze, keine stille Lücke
//! Die sysfs-Fläche unter `/sys/class/drm` ist **treiberabhängig**:
//! `amdgpu` exponiert `gpu_busy_percent` und `mem_info_vram_used`/
//! `mem_info_vram_total`; `i915` (Intel) hat andere, teils überlappende
//! Attribute; `nvidia` exponiert über sysfs **fast nichts** — das
//! Interessante liegt dort hinter der proprietären NVML-Bibliothek. Diese
//! Crate zieht **keine** herstellerspezifische Bibliothek (kein NVML, kein
//! ROCm-SMI, kein Subprozess wie `nvidia-smi`/`rocm-smi`) — sie liest sysfs
//! oder gar nichts. **Was über sysfs nicht sichtbar ist, wird nicht
//! gemeldet.** Für eine NVIDIA-Karte kann das im Extremfall bedeuten: die
//! Karte wird über `card*` gefunden, aber keine der vier Dateien ist lesbar
//! — dann meldet [`GpuSensor::poll`] für diese Karte schlicht **keine**
//! Samples, ohne dass der Abruf scheitert (siehe oben: fehlende Dateien sind
//! kein Fehler). Das ist eine bewusste, dokumentierte Grenze dieser Crate,
//! keine übersehene Lücke.
//!
//! # Einheitenumrechnung — Millidegree wie bei `harw-dod-thermal`
//! `temp1_input` steht, exakt wie `harw-dod-thermal`s `temp`-Datei, in
//! **Millidegree Celsius**: `45000` bedeutet 45,0 °C. [`GpuSensor::poll`]
//! übernimmt dieselbe Entscheidung wie `harw-dod-thermal` (siehe dessen
//! `sensor.rs`-Moduldoku, Abschnitt „Einheitenumrechnung", für die volle
//! Begründung) und rechnet vor dem Melden durch Division durch 1000 in Grad
//! Celsius um, unter der Metrik `gpu_temperature_celsius_<karte>`. Ein
//! `HostSample` mit dem unveränderten Millidegree-Rohwert wäre um Faktor
//! tausend falsch — und der Fehler fiele erst auf, wenn jemand einen
//! Schwellwert setzt, der anderswo im selben Contract bereits „Grad Celsius"
//! bedeutet.
//!
//! # Kartenlabel und Kardinalität
//! `harw_dod_signals::HostSample` hat kein eigenes Label-Feld (siehe
//! `harw-dod-thermal`s Moduldoku für die volle Begründung, warum ein
//! Präfix im `metric`-String der einzig verbleibende Ort dafür ist).
//! [`GpuSensor::poll`] hängt deshalb an jeden festen Metrikpräfix
//! (`gpu_busy_percent_`, `gpu_vram_used_bytes_`, `gpu_vram_total_bytes_`,
//! `gpu_temperature_celsius_`) ein sanitisiertes Kartenlabel an — den Namen
//! des gefundenen `card*`-Verzeichnisses (z. B. `card0`), reduziert auf
//! `[a-z0-9_]` und begrenzt auf eine feste Länge.
//!
//! Das ist nur vertretbar, **weil die Zahl der Karten je Host klein und
//! über die Laufzeit stabil ist**: ein Server hat typischerweise null, eine
//! oder wenige Karten, ein GPU-Rechenknoten selten mehr als eine niedrige
//! zweistellige Zahl. `sensor::MAX_CARDS` deklariert diese Obergrenze
//! ausdrücklich (16, wie `harw-dod-thermal`s Zonen-Obergrenze) **und
//! erzwingt sie** bei der Kartensuche (vor F-203 deklariert, aber nirgends
//! durchgesetzt); [`sensor::MAX_CARDINALITY`] multipliziert
//! sie mit der festen Zahl der vier möglichen Metriken je Karte und ist der
//! Wert, den [`harw_dod_fixtures::sensor_suite!`] als `max_cardinality`
//! erhält.
//!
//! **F-203 — Connector-Verzeichnisse ausgeschlossen.** `card*` matcht auf
//! einem Host mit mehreren Bildschirmausgängen nicht nur echte
//! Kartenverzeichnisse (`card0`, `card1`), sondern auch die
//! Connector-Verzeichnisse, die DRM je Anschluss zusätzlich unter
//! `/sys/class/drm` anlegt (`card1-HDMI-A-1`, `card1-DP-1`, …) — jedes mit
//! einem eigenen `device`-Symlink zurück zur Karte. Ohne Filterung würde ein
//! Multi-Monitor-Host dieselbe physische Karte unter mehreren Labels
//! doppelt melden und dabei die deklarierte Kardinalitätsgrenze
//! überschreiten. [`sensor::is_card_root_name`] lässt ausschließlich `card`
//! gefolgt von Ziffern zu.
//!
//! # Warum kein `#[derive(harw_macros::SensorSource)]`
//! `#[derive(harw_macros::SensorSource)]` trägt genau einen Fall: ein
//! einzelner Glob-Treffer wird zu **einem** Skalar unter **einem**, zur
//! Kompilierzeit festen Metriknamen, ohne Umrechnung (siehe dessen
//! Moduldoku und `harw-dod-thermal`s `lib.rs`-Moduldoku, Abschnitt „Warum
//! kein `#[derive(SensorSource)]`", für die vollständige Herleitung derselben
//! drei Lücken). Diese Crate braucht **alle drei** Dinge, die dem Makro
//! fehlen, gleichzeitig:
//!
//! 1. **Mehrere Metriken je Treffer.** Eine Karte liefert bis zu vier
//!    Samples (Auslastung, zwei Speicherwerte, Temperatur), nicht einen
//!    Skalar.
//! 2. **Ein Label je Treffer.** Mehrere Karten brauchen unterscheidbare
//!    Metriknamen (siehe oben) — das Makro kennt nur einen einzigen,
//!    für alle Treffer identischen Metriknamen.
//! 3. **Eine Umrechnung.** Der Millidegree-Rohwert von `temp1_input` muss
//!    durch 1000 geteilt werden, bevor er als `value` erscheint — das Makro
//!    übernimmt den geparsten Rohwert unverändert.
//!
//! [`GpuSensor`] implementiert `Sensor` deshalb von Hand (siehe [`sensor`]).
//! Die dritte, unabhängige Beobachtung aus `harw-dod-thermal`s Moduldoku —
//! ein fest verdrahtetes Glob-Muster fände in der Fixture-Prüfung nichts,
//! weil `harw_dod_readfs::glob::glob` immer ab `/` sucht — gilt auch hier:
//! [`sensor`] baut jedes Glob-Muster **zur Laufzeit** aus der tatsächlichen
//! Bereichswurzel (`ReadScope::roots`) statt es fest zu verdrahten.
//!
//! # Leerer Bereich: warum diese Crate eine ausdrückliche Angabe macht
//! [`harw_dod_fixtures::sensor_suite!`] erzeugt eine Prüfung auf einen
//! `ReadScope` über ein frisches, tatsächlich leeres Verzeichnis. Sie forderte
//! ursprünglich **ausschließlich** `Err(SensorError::SourceUnavailable)` — als
//! Beleg dafür, dass ein Sensor nicht am `ReadScope` vorbei den echten Host
//! liest.
//!
//! Für diese Crate ist das falsch: ein leeres Verzeichnis ist von „keine
//! `card*`-Verzeichnisse gefunden" **nicht unterscheidbar**, und ein Host ohne
//! GPU liefert nach dem Zuschnitt dieses Knotens legitim
//! `Ok(SensorReading::default())`. Dieser Knoten hat den Widerspruch beim Bauen
//! gemeldet statt ihn durch ein falsches `Err` zu übertünchen.
//!
//! **Die Meldung wurde aufgegriffen.** `harw-dod-fixtures` trennt die beiden
//! Behauptungen inzwischen: *Bereichsdichtheit* („liest der Sensor außerhalb
//! seiner Wurzel?") wird für jeden Sensor unbedingt geprüft, und zwar gegen
//! einen Bereich, der als leeres Unterverzeichnis **inmitten** eines Baums mit
//! echten Quelldateien liegt — findet der Sensor sie trotzdem, hat er
//! außerhalb gelesen. *Was ein leerer Bereich bedeutet* ist davon getrennt und
//! wird über `empty_scope:` erklärt. Diese Crate gibt dort
//! `EmptyScopeExpectation::EmptySourceIsNormal` an, und der Harness prüft
//! daraufhin, dass tatsächlich `Ok(leer)` herauskommt — weder ein Fehler noch
//! erfundene Werte. **In beiden Ausprägungen wird geprüft; es gibt keinen Weg,
//! die Prüfung abzuschalten.**
//!
//! # Nebenläufigkeit
//! [`GpuSensor`] hält ausschließlich einen unveränderlichen
//! `harw_dod_cap::SensorHandle`: `Send + Sync` ohne inneres Locking.
//! [`GpuSensor::poll`] öffnet und schließt seine eigenen Dateien je Aufruf;
//! parallele Aufrufe auf derselben Instanz stören sich nicht.
//!
//! # Fehler
//! `harw_dod_cap::SensorError` — inhaltsfrei, kein Feldwert, kein Pfad,
//! keine gelesene Zeile erscheint in einer Fehlermeldung. Siehe
//! `harw_dod_signals::Sensor::poll` für die vollständige Fehlerbedeutung und
//! [`GpuSensor::poll`] für die drei hier tatsächlich auftretenden Varianten.
//!
//! # Examples
//! ```rust,no_run
//! use harw_dod_cap::scope::AliasRoot;
//! use harw_dod_cap::{Capability, ReadScope, SensorHandle};
//! use harw_dod_gpu::GpuSensor;
//! use harw_dod_signals::Sensor;
//! use harw_types::SensorId;
//! use std::path::PathBuf;
//!
//! // `/sys/class/drm/*`-Einträge sind Symlinks nach `/sys/devices/...`
//! // (F-005); `AliasRoot::sysfs_class` baut den Bereich, der das zulässt.
//! let alias =
//!     AliasRoot::sysfs_class(PathBuf::from("/sys/class/drm")).expect("gültige sysfs-Klassenwurzel");
//! let scope = ReadScope::from_roots_and_aliases(Vec::new(), [alias]);
//! let handle = SensorHandle::new(SensorId::from_str("gpu-0"), Capability::ReadSysfsDrm)
//!     .bind(scope);
//! let sensor = GpuSensor::from(handle);
//! let reading = sensor.poll(jiff::Timestamp::UNIX_EPOCH)?;
//! for sample in &reading.samples {
//!     println!("{}: {}", sample.metric, sample.value);
//! }
//! # Ok::<(), harw_dod_cap::SensorError>(())
//! ```

// `pub`, nicht privat wie bei `harw-dod-thermal`: `sensor::MAX_CARDINALITY`
// muss von außerhalb dieser Crate erreichbar sein (Doctest unten sowie ein
// etwaiger externer Integrationstest), analog zu `harw-dod-cpu`s
// `pub mod sensor;`.
pub mod sensor;

pub use sensor::GpuSensor;

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;
