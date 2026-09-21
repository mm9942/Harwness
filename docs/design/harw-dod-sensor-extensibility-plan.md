# Sensorik erweitern: Vertrag, Harness und Kochbuch

**Status:** Entwurf zur Diskussion, noch nicht normativ
**Zweck:** Sicherstellen, dass der fünfzehnte Sensor genauso billig ist wie
der dritte
**Verwandt:** `harw-dod-crate-decomposition.md` (C1 bis C8),
`harw-dod-integration-and-dependencies.md`

---

## 0. Was Erweiterbarkeit hier bedeutet

Die Zerlegung in eine Crate pro Quelle liefert Erweiterbarkeit auf dem
Papier: eine neue Quelle ist eine neue Crate, und keine bestehende wird
angefasst. Damit das in der Praxis stimmt, müssen vier Dinge zutreffen, und
drei davon fallen aus der bisherigen Architektur:

**Der Trait ist schmal.** `Sensor` verlangt Identität, Berechtigung, Takt,
plus `poll` oder `subscribe`. Mehr nicht.

**Die Fehler komponieren nicht nach oben.** Jede Crate hat ihren eigenen
reichen Fehlertyp und verdichtet erst an der Trait-Grenze auf `SensorError`.
Ein neuer Sensor berührt damit kein fremdes Enum.

**Ausfall ist ein Zustand.** Ein neuer Sensor darf scheitern, ohne etwas
mitzureißen. Man kann einen experimentellen Sensor in Produktion mitlaufen
lassen; bindet er auf einem Kernel nicht, ist das eine Metrik und kein
Vorfall.

**Die Verarbeitungskette wird geerbt.** Weil beide Ströme typisiert sind,
steht ein neuer Sensor sofort der Regelschicht zur Verfügung, erscheint in
der Telemetrie, ist über Selektoren im Triage-Kontext adressierbar und kann
über den Ringpuffer Evidenz liefern. Einmal `HostSample` und
`SecurityEvent` festgelegt, erbt jede neue Quelle alles Weitere.

Das vierte Ding fällt nicht von selbst und ist der eigentliche Inhalt dieses
Plans: **die Fixture-Last.** Sie ist es, die Erweiterbarkeit langfristig
auffrisst, nicht die Crate-Zahl.

---

## 1. Die Regel: offene Menge, geschlossenes Vokabular

**Offen:** die Menge der Sensoren. Eine neue Quelle kostet eine Crate und
niemanden sonst.

**Geschlossen:** `Capability`, `EventKind`, `SampleScope`, `Hardness`. Eine
Quelle mit einer wirklich neuen Privilegienklasse oder einer wirklich neuen
Ereignisform berührt das Vokabular, und dann zeigt der Compiler jede Regel,
jeden Renderer und jede Zuordnung, die die neue Variante noch nicht
behandelt.

Das ist dieselbe Trennung wie bei Rollen und Spezialisierungen, eine Ebene
tiefer: neue Quellen kosten nichts, neue Autoritätsklassen kosten eine
bewusste Entscheidung. Wenn ein Vorschlag für einen neuen Sensor eine neue
`Capability`-Variante verlangt, ist das das Signal, innezuhalten. Meistens
ist die richtige Antwort dann ein zweiter Prozess, keine zwölfte
Berechtigung.

**Verfahren bei Vokabularänderung.** Kein stilles Hinzufügen. Eine neue
`EventKind`-Variante durchläuft: Eintrag in dieses Dokument mit Begründung,
Prüfung, ob eine bestehende Variante genügt, Erweiterung der
Zulässigkeitsmatrix in der Leiter, und ein Testfall, der beweist, dass die
Regelschicht sie nicht stillschweigend als `Informational` durchreicht.

---

## 2. Der Erweiterungsvertrag

Was eine neue Sensor-Crate liefern muss. Sechs Punkte, jeder prüfbar.

1. **Genau eine Quelle, genau eine `Capability`.** Braucht sie zwei, sind es
   zwei Backends hinter einem Trait (Präzedenz `harw-dod-authlog`) oder zwei
   Crates.
2. **Konstruktor nimmt einen `ReadScope`.** Kein Sensor öffnet je einen Pfad
   an ihm vorbei. Kein `std::fs::read` im Crate, durchgesetzt per Lint.
3. **Eigener Fehlertyp plus `From` auf `SensorError`.** Die Abbildung ist
   inhaltsfrei: Offsets und Längen, keine geparsten Werte.
4. **Kein Abhängigkeit auf eine andere Sensor-Crate** (C7). Geprüft in CI
   über `harw-code-graph`.
5. **Ein Fixture-Verzeichnis** nach der Konvention aus §3, mit mindestens
   drei Kernelständen und einem Fehlerfall.
6. **Ein Eintrag in der Rechtematrix** des Zerlegungsplans: was sie liest,
   in welchem Prozess sie läuft, was sie ausdrücklich nicht darf.

Punkt 6 ist der, der am ehesten vergessen wird und am meisten wert ist. Die
Zeile "darf nicht" ist oft aufschlussreicher als die Zeile "liefert".

---

## 3. Der Fixture-Harness

Bei vierzehn Sensoren mal drei Kernelständen sind das zweiundvierzig
Verzeichnisse. Ohne gemeinsamen Harness baut jeder Sensor seine eigene
Testmechanik nach, und ab dem achten macht das niemand mehr sorgfältig.

**Ein Crate, `harw-dod-fixtures`, dev-dependency für alle Sensoren.**

### 3.1 Verzeichniskonvention

```
fixtures/
  <sensor-id>/
    <kernel>/            z. B. 6.11-fedora, 6.6-lts, 5.14-rhel9
      tree/              Auszug der Quelle, pfadgetreu
        proc/stat
        sys/class/hwmon/hwmon0/temp1_input
      expect.json        erwartete Samples oder Events, normalisiert
    malformed/
      tree/              abgeschnitten, leer, unerwartete Spalten
      expect.json        erwarteter SensorError, ohne Inhalt
    adversarial/
      tree/              Werte, die wie Anweisungen aussehen
      expect.json        muss als Data-Fragment enden, nie als Instruktion
```

Der `tree`-Ordner ist pfadgetreu, weil der Sensor über einen `ReadScope` auf
genau dieses Verzeichnis zeigt. Damit läuft derselbe Codepfad wie in
Produktion, inklusive Scope-Prüfung und Symlink-Auflösung.

### 3.2 Der Tabellentest

```rust
harw_dod_fixtures::sensor_suite! {
    sensor: harw_dod_thermal::Thermal,
    id: "thermal",
    // findet alle Kernelstände automatisch, führt drei Klassen aus:
    // parse (tree gegen expect), malformed (Fehler ohne Inhalt),
    // adversarial (Vertrauensklasse Data, Redaktion greift)
}
```

Ein Makroaufruf, kein Testcode. Der Harness prüft zusätzlich, ohne dass es
jemand hinschreibt:

- **Determinismus:** zweimal auf demselben Baum liefert dasselbe.
- **Inhaltsfreiheit:** kein Byte aus `tree` erscheint in einem
  `SensorError` (C5). Property-Test über den Fehlerpfad.
- **Scope-Dichtheit:** ein Symlink aus `tree` heraus liefert
  `OutsideScope`, nicht die Zieldatei.
- **Redaktion:** jedes Feld des erzeugten Samples oder Events geht durch
  `Redact` und erzeugt keinen `Plain`-Wert, der nicht deklariert ist.
- **Kardinalität:** die erzeugten Labels liegen unter der deklarierten
  Grenze.

Damit erbt ein neuer Sensor sechs Prüfungen, für die er null Zeilen
schreibt. Genau das hält den fünfzehnten so billig wie den dritten.

### 3.3 Herkunft und Pflege der Fixtures

Auszüge werden erzeugt, nicht getippt: ein kleines Werkzeug
`harw-dod-fixtures --capture <sensor> --label 6.11-fedora` kopiert die vom
Sensor deklarierten Pfade aus dem laufenden System in ein Verzeichnis und
redigiert dabei, was redigiert werden muss (Hostnamen, Seriennummern,
Benutzernamen).

Pflegeregel: drei Stände dauerhaft, nämlich der aktuelle Fedora-Kernel, ein
LTS-Stand und die älteste unterstützte Enterprise-Linie. Ein vierter kommt
nur dazu, wenn ein Format tatsächlich abweicht.

### 3.4 Formatdrift bemerken, bevor sie weh tut

Kernel ändern Formate. Der Sensor merkt das als Parse-Fehler, aber ein
einzelner Parse-Fehler ist im Rauschen unsichtbar. Deshalb ist die
Fehlerrate pro Sensor eine Metrik mit Baseline:
`sensor_parse_errors_total` je `SensorId`. Ein sprunghafter Anstieg nach
einem Systemupdate ist ein Befund über den `ScanReport`-Weg, kein stiller
Datenverlust. Blindheit ist ein Befund, und Formatdrift ist die häufigste
Ursache von Blindheit.

---

## 4. Vier Archetypen und ihr Aufwand

Fast jeder Sensor fällt in eine dieser vier Formen. Der Aufwand ist deshalb
gut schätzbar.

| Archetyp | Beispiel | Mechanik | Aufwand | Fixtures |
|---|---|---|---|---|
| **A: sysfs-Einzelwert** | thermal, gpu | Glob über Pfade, Zahl je Datei, Skalierung | 50 bis 100 Zeilen, meist ganz per `#[derive(SensorSource)]` | trivial |
| **B: procfs-Tabelle** | cpu, blockio, netcounters | zeilenweise Tabelle mit fester Spaltenordnung, Delta gegen Vorlauf | 100 bis 200 Zeilen | mittel, Spalten variieren nach Version |
| **C: Netlink-Abo** | authlog | Socket, Rahmen, Recordtypen, Feldzerlegung | 200 bis 300 Zeilen plus Recordfixtures | aufwendig |
| **D: eBPF-Map** | procmon, flow | Programm laden, Map oder Ringpuffer lesen, Ereignisse formen | 150 Zeilen Rust plus BPF-Seite | aufwendig, braucht VM |

**Archetyp A ist praktisch kostenlos.** Das ist Absicht: die meisten
Erweiterungswünsche für Hostgesundheit fallen dort hinein, und das Derive
macht daraus eine Deklaration.

```rust
#[derive(SensorSource)]
#[sensor(id = "thermal", capability = SysfsRead, cadence = "5s")]
struct Thermal {
    #[read(glob = "class/hwmon/*/temp*_input", scale = 0.001)]
    #[metric(TEMP_CELSIUS, scope = Host)]
    temperature: Vec<f64>,
}
```

Der Glob ist relativ zum `ReadScope`, nicht absolut. Das Derive erzeugt
`poll`, die Metrikemission, die `Capability`-Deklaration und die
`From`-Abbildung auf `SensorError`. Was bleibt, ist die Deklaration selbst.

---

## 5. Durchgerechnetes Beispiel: `harw-dod-usb`

Ein realistischer neuer Sensor, um den Aufwand konkret zu machen. USB-Geräte
sind ein echtes Sicherheitssignal: ein neu angestecktes Massenspeichergerät
oder ein Gerät, das sich als Tastatur ausgibt, ist genau die Art von
Ereignis, die man sehen will.

**Vertrag.** Quelle `/sys/bus/usb/devices`, Capability `SysfsRead`, Prozess
`harw-sentinel`, Archetyp A mit einem Ereignisanteil.

**Liefert.** Beim Poll die aktuelle Geräteliste mit Vendor, Produkt,
Geräteklasse und Portpfad. Als Ereignis die Differenz gegen den vorigen
Poll.

**Darf nicht.** Geräteinhalte lesen, Mounts auflösen, Prozesse zuordnen.

**Vokabularfrage.** Braucht es eine neue `EventKind`-Variante? Ja, und
genau deshalb ist das ein gutes Beispiel: `DeviceAttached` und
`DeviceDetached` sind neu. Nach §1 heißt das: Eintrag mit Begründung,
Prüfung ob `StructureDrift` genügt (tut es nicht, das ist Workspace), und
ein Testfall gegen stillschweigendes `Informational`.

**Regelanbindung ohne neue Mechanik.** Eine Baseline vom Typ
`Expectation::AllowedSet` über die bekannten Vendor-Produkt-Paare. Ein
Gerät außerhalb der Menge ist `Anomaly`, weil die Baseline
`Provisional` sein kann. Wird die Menge per Review nach `Established`
befördert, wird dasselbe Ereignis `RuleTriggered`. Ein
Massenspeichergerät oder ein HID-Gerät außerhalb der Menge kann per Regel
höhere `Severity` bekommen.

**Aufwand insgesamt.** Rund achtzig Zeilen Rust, ein Fixture-Verzeichnis mit
drei Ständen plus einem Adversarial-Fall (ein Gerät, dessen Produktname wie
eine Anweisung aussieht, denn Produktstrings sind angreiferkontrolliert),
zwei Zeilen Vokabular, eine Zeile Rechtematrix, eine Baseline-Definition.
Ein Nachmittag, ohne dass eine bestehende Crate angefasst wird.

Der Adversarial-Fall ist hier kein Formalismus: ein USB-Produktstring ist
frei wählbar und landet im Triage-Kontext. Ohne die Vertrauensklasse `Data`
und den Zwei-Block-Renderer wäre das ein Injection-Vektor über Hardware.

---

## 6. Sensoren von außen

Kann ein Plugin einen Sensor mitbringen? Ja, mit einer harten Grenze.

**Die Grenze:** ein von außen registrierter Sensor kann keine `Capability`
erlangen, die der Prozess nicht ohnehin hat. Er läuft im Sentinel, also
unprivilegiert, mit einem `ReadScope`, den die Composition-Root vergibt und
der nur schrumpfen kann. Ein Plugin-Sensor mit `FanotifyMark` ist nicht
ausdrückbar, weil der Sentinel diese Berechtigung nicht besitzt und `bind()`
den echten Probe-Zugriff nicht bestanden bekommt.

Das ist dieselbe Regel wie bei den Agenten: Plugins dürfen Programme
mitbringen, keine Privilegien.

**Was ein externer Sensor zusätzlich erfüllen muss:** denselben
Erweiterungsvertrag aus §2, plus Registrierung mit deklarierter
`Capability`, die die Registry gegen die Prozessberechtigung prüft und bei
Überschreitung ablehnt. Eine höhere Behauptung ist ein
Registrierungsfehler, kein Laufzeitfehler.

---

## 7. Prüfungen

- **Vertragsprüfung.** Ein CI-Schritt, der für jede Crate mit Präfix
  `harw-dod-` und einer `Sensor`-Implementierung die sechs Punkte aus §2
  prüft, soweit maschinell möglich: eine `Capability`, keine
  Sensor-Geschwister-Abhängigkeit, Fixture-Verzeichnis vorhanden, kein
  direkter `std::fs`-Aufruf.
- **Harness-Vollständigkeit.** Jeder Sensor ruft `sensor_suite!` auf; ein
  fehlender Aufruf bricht den Build.
- **Vokabular-Gate.** Eine Änderung an `Capability` oder `EventKind` ohne
  begleitenden Eintrag in diesem Dokument scheitert im Review-Checkliste;
  maschinell prüfbar über einen Doc-Test, der die Variantenzahl gegen eine
  Konstante hält.
- **Driftmetrik.** `sensor_parse_errors_total` hat eine Baseline und ist an
  die Regelschicht angebunden.

---

## 8. Offene Punkte

1. **Capture-Werkzeug und Redaktion.** Welche Felder beim Erfassen
   redigiert werden, muss pro Archetyp festgelegt werden. Hostnamen und
   Seriennummern sind offensichtlich, Gerätepfade weniger.
2. **Fixture-Größe.** Vollständige `/proc`-Auszüge können groß werden.
   Vorschlag: nur die vom Sensor deklarierten Pfade erfassen, was das
   Werkzeug ohnehin weiß.
3. **Archetyp D ohne VM.** Für eBPF-Sensoren gibt es keinen billigen
   Fixture-Weg; die Map-Inhalte lassen sich synthetisch erzeugen, das
   Laden nicht. Vorschlag: Trennung in einen testbaren Formungsteil und
   einen ungetesteten Ladeteil, der in der VM-Suite läuft.
4. **Wer entscheidet über neue `EventKind`-Varianten.** Vorschlag: dieselbe
   Schwelle wie bei einer neuen Warden-Aktion, also eine bewusste
   Entscheidung mit Eintrag, nicht ein Pull Request nebenbei.
