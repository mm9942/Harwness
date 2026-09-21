# Telemetrie-Export: OTLP, Prometheus und die Sink-Grenze

**Status:** Entwurf zur Diskussion, noch nicht normativ
**Zweck:** Wie Telemetrie das System verlässt, welche Abhängigkeiten das
kostet, und warum keine davon im Kern liegt
**Recherchestand:** August 2026
**Verwandt:** `harw-context-plan.md` (Metrikfamilie Kontext),
`harw-security-observability-plan.md` §6,
`harw-dod-integration-and-dependencies.md` (Doktrin D1 bis D9)

---

## 0. Die Grundentscheidung

`harw-observe` definiert `TelemetrySink` und kennt kein Backend. Das ist
keine Vorsicht, sondern eine Recherche-Entscheidung: OpenTelemetry-Rust ist
Mitte 2026 auf der 0.32-Linie, Logs und Metriken sind stabil, Traces noch
Beta, sämtliche Erstanbieter-Crates weiterhin pre-1.0, mit Brüchen in
Minor-Releases und Versionierung im Gleichschritt über die ganze Familie.
Bemerkenswert dabei: die Reihenfolge ist umgekehrt zu Go und Java, wo Traces
zuerst stabil wurden. Wer aus anderen Sprachen Erwartungen mitbringt, liegt
hier falsch.

Eine solche Abhängigkeit im Kern eines Systems, das auf Jahre gebaut wird,
wäre ein wiederkehrender Umbau. Hinter einem eigenen Trait ist sie ein
Adapter, den man austauscht.

**Regel:** Kein Typ aus einer Backend-Crate erscheint jemals in einer
öffentlichen Signatur von `harw-observe`, `harw-dod` oder `harw`. Das ist
Doktrin D5, und hier ist ihr wichtigster Anwendungsfall.

---

## 1. Drei Sinks, drei Crates

| Crate | Transport | Abhängigkeiten | Zweck |
|---|---|---|---|
| `harw-observe-file` | JSONL auf Platte | keine über `serde_json` hinaus | Standard. Immer verfügbar, airgap-tauglich, forensisch nutzbar |
| `harw-observe-prom` | Pull, Text-Exposition | eigenes Rendering, minimaler HTTP-Server | Klassische Betriebsintegration ohne Collector |
| `harw-observe-otlp` | Push, OTLP | OpenTelemetry-Familie | Wenn ein Collector existiert und Traces gewünscht sind |

Alle drei implementieren `TelemetrySink`. Keine kennt die andere. Der Kern
kennt keine.

### 1.1 `harw-observe-file` ist der Standard, nicht der Notnagel

Ein zeilenweises JSONL mit Zeitstempel, Metrikschlüssel, Labels und Wert
kostet praktisch nichts, hat keine Abhängigkeit, läuft ohne Netz und ist
genau das, was man bei einem Sicherheitsvorfall haben will: eine lokale,
append-only Datei, die man mitnehmen kann. Sie liegt unter `harw-home`,
rotiert nach Größe, und die Rotation schreibt eine Prüfsumme der
abgeschlossenen Datei.

Für den Sentinel ist das der einzige Sink, der per Voreinstellung aktiv ist.

### 1.2 `harw-observe-prom` wird selbst gebaut

Das Prometheus-Textformat ist ein Zeilenformat mit `# HELP`, `# TYPE` und
Metrikzeilen. Es zu erzeugen sind wenige hundert Zeilen, und `MetricKey`
enthält bereits alles Nötige: Name, Art, Einheit, Labels. Eine
Client-Bibliothek würde ein zweites Metrikmodell mitbringen, das gegen
deines abgeglichen werden müsste.

Das gilt nach Doktrin D6: kleine Fläche, eigene Datenstruktur schon
vorhanden, also selbst bauen. Konkret entsteht dabei zusätzlich die
Namenskonvention als Code statt als Absprache:

```rust
/// Prometheus-Namensregeln, aus MetricKey abgeleitet, nicht getippt.
fn prom_name(key: &MetricKey) -> String {
    // Counter enden auf _total; Basiseinheiten sind Sekunden und Bytes;
    // Gauges tragen die Einheit im Suffix. Verstöße sind Compile-Zeit-
    // Fehler in `metrics!`, nicht Laufzeit-Warnungen des Scrapers.
}
```

Der Endpunkt selbst ist eine Sicherheitsfläche: er bindet auf Loopback oder
einen Unix-Socket, nie auf `0.0.0.0`, und ist damit
`PermissionTier::Operator`. Ein offener Metrikendpunkt ist eine Landkarte
des Systems.

### 1.3 `harw-observe-otlp` ist optional und gekapselt

Nur diese Crate kennt die OpenTelemetry-Familie. Sie ist ein optionales
Feature, sie erscheint in keiner öffentlichen Signatur, und ihr
Abhängigkeitsbaum wird gegen die Doktrin geprüft wie jeder andere.

Zwei Transportvarianten stehen zur Wahl: gRPC über tonic oder HTTP mit
Protobuf. Empfehlung ist HTTP, weil der Baum deutlich kleiner ist und
Harwness ohnehin einen HTTP-Client für die Provider fährt. gRPC lohnt sich
erst bei sehr hohen Raten, die hier nicht anliegen.

**Der Warden linkt diese Crate nie.** Sein Abhängigkeitsbudget nach D7 lässt
das nicht zu, und er braucht sie auch nicht: er schreibt in den File-Sink,
der Sentinel exportiert.

---

## 2. Routing nach Namensraum

Der Punkt, an dem S6 auf die Telemetrie trifft. Nicht jede Metrik darf
überall hin.

```rust
pub struct SinkRouting {
    default: SinkId,
    by_namespace: BTreeMap<&'static str, SinkSet>,
}
```

| Namensraum | Voreinstellung | Begründung |
|---|---|---|
| `context.*`, `plan.*`, `goal.*`, `model.*` | alle konfigurierten Sinks | Betriebsdaten, unkritisch |
| `sensor.*`, `host.*` | alle konfigurierten Sinks | Hostgesundheit, unkritisch |
| `security.*` | **nur File-Sink** | Befundraten und Regeltreffer sind eine Landkarte der Schwachstellen |
| `warden.*` | **nur File-Sink** | dito, plus Aktionshistorie |
| Meta-Nullzähler | alle Sinks | ihr Wert ist null; ihre Existenz ist keine Information |

Der Export von `security.*` ist ein bewusster Konfigurationsschritt mit
Operator-Bestätigung, kein Standard. Ein Angreifer, der die Metriken
mitliest, sieht sonst genau, welche Regeln feuern und welche blind sind.

---

## 3. Kardinalität als Vertrag

Das Feld `Cardinality` in `MetricKey` ist nicht Dokumentation, es ist die
Durchsetzung.

**Verboten als Label, ausnahmslos:** vollständige Pfade, PIDs, Session-IDs,
Turn-IDs, IP-Adressen, Benutzernamen, Dateinamen, Prozessargumente,
Modell-Antworttexte. Alles davon ist entweder unbegrenzt kardinal oder
personenbezogen oder beides.

**Erlaubt:** geschlossene Enums (Rolle, Schrittart, Auslassungsgrund,
Härtegrad, Sensorstatus), begrenzte Aufzählungen (Modell-ID, Provider-ID,
Sektionsname, Sensor-ID), kleine Zahlbereiche (Kernnummer).

**Grenzfälle mit Regel:** cgroup-Identität wird als Rolle plus Tiefe
gelabelt, nicht als voller Pfad. Ein `exe_digest` wird nicht gelabelt,
sondern erscheint nur im Ereignisstrom. Plan-Knoten werden nach Art
gelabelt, nicht nach `TaskId`.

Die Durchsetzung ist zweistufig: `metrics!` lehnt zur Compile-Zeit ein Label
ab, das nicht per `field!` deklariert ist, und der Sink lehnt zur Laufzeit
eine Labelmenge ab, die die deklarierte Obergrenze überschreitet, mit dem
Zähler `context_label_cardinality_exceeded`. Was nicht gemessen werden kann,
ohne die Zeitreihendatenbank zu sprengen, wird als Ereignis geführt, nicht
als Metrik. Das ist die eigentliche Trennlinie zwischen den beiden Strömen.

---

## 4. Traces, Spans und die Brücke

Die vorhandenen `tracing`-Aufrufstellen bleiben, was sie sind. Die Brücke
ist eine Layer-Registrierung in genau einer Datei:

```
tracing (bestehend)
  └─ #[traced] normalisiert Felder und Redaktion
     └─ harw-observe: FieldName, MetricKey, TraceContext
        ├─ harw-observe-file   (immer)
        ├─ harw-observe-prom   (optional, pull)
        └─ harw-observe-otlp   (optional, push)
             └─ tracing-opentelemetry Layer
```

Zwei Punkte aus der Recherche, die die Reihenfolge bestimmen:

**Traces sind auf der Rust-Seite noch Beta, Metriken stabil.** Deshalb ist
die Metrikbrücke Welle W1 und die Trace-Brücke Welle W7. Umgekehrt wäre die
naheliegende Reihenfolge, aber sie wäre für Rust falsch.

**`tracing-opentelemetry` versioniert eigenständig gegen die
OpenTelemetry-Crates.** Das ist eine Kopplung, die man beim Aktualisieren
prüfen muss. Sie steht deshalb im Doktrin-Inventar mit einem expliziten
Kompatibilitätsvermerk, damit ein Update nicht still zwei Versionen mischt.

**Was propagiert wird**, ist bereits geplant: `TraceContext` in
`StoredJob`, `ChildLeaseRecord` und `WardenRequest`. Ohne diese drei Felder
zerfällt jede lange Ausführung in Fragmente, egal welches Backend darunter
liegt. Deshalb gehören sie in W1 und nicht in die Exportwelle.

**Exemplars**, also die Verknüpfung eines Histogrammpunkts mit einer
konkreten Trace-ID, sind der eigentliche Gewinn einer OTLP-Anbindung: aus
"die Turn-Latenz hat einen Ausreißer" wird "hier ist der Turn". Sie sind
optional und an das Vorhandensein des OTLP-Sinks gebunden.

---

## 5. Abhängigkeitsanalyse

### 5.1 Was der OTLP-Sink kostet

| Ebene | Crates | Charakter | Risiko |
|---|---|---|---|
| API und SDK | `opentelemetry`, `opentelemetry_sdk` | reines Rust | pre-1.0, Gleichschritt-Versionierung, Brüche in Minors |
| Exporter | `opentelemetry-otlp` | reines Rust | dito, plus Transportwahl |
| Serialisierung | `prost` | reines Rust | stabil, breit verankert |
| Transport HTTP | vorhandener HTTP-Client | bereits im Baum | keine neue Fläche |
| Transport gRPC | `tonic`, `hyper`, `tower` | reines Rust | großer Baum, deshalb nicht empfohlen |
| Brücke | `tracing-opentelemetry` | reines Rust | eigene Versionsachse gegen die OTel-Crates |
| Semantik | `opentelemetry-semantic-conventions` | Namenskonstanten | harmlos, aber optional |

Alles davon ist reines Rust, verletzt also D1 und D2 nicht. Das Risiko ist
nicht Speichersicherheit, sondern Versionschurn. Genau dagegen hilft die
Kapselung.

### 5.2 Was der Prometheus-Sink kostet

Nahezu nichts, wenn selbst gebaut: Formatierung aus `MetricKey`, ein
minimaler HTTP-Handler auf Loopback oder Unix-Socket. Eine
Client-Bibliothek würde ein zweites Metrikmodell einführen, das mit deinem
abgeglichen werden müsste, und genau diese Doppelung ist der Fehler, den die
Registry vermeiden soll.

### 5.3 Was der File-Sink kostet

`serde_json`, bereits im Baum. Null neue Fläche.

### 5.4 Bewertung gegen die Doktrin

| Regel | OTLP | Prom (selbst) | File |
|---|---|---|---|
| D1 reines Rust | erfüllt | erfüllt | erfüllt |
| D2 kein C-Build | erfüllt | erfüllt | erfüllt |
| D5 nicht in öffentlichen Typen | erfüllt durch Kapselung | erfüllt | erfüllt |
| D6 selbst bauen bei kleiner Fläche | nein, Fläche zu groß | **ja, deshalb selbst** | ja |
| D7 Warden-Budget | linkt es nie | linkt es nie | linkt es |
| D8 CI-Gates | volle Prüfung | trivial | trivial |

---

## 6. Wellen

**T0, im Rahmen von W0.** `TelemetrySink`, `MetricKey`, `Cardinality`,
`SinkRouting`, `NullSink`. Kein Export.

**T1, im Rahmen von W1.** `harw-observe-file` und die Metrikemission der
Selbstbeobachtung. Ab hier existieren Zahlen, lokal, ohne Abhängigkeit. Das
ist der Punkt, an dem die Plan-Arbeit bereits profitiert.

**T2, im Rahmen von W3.** `harw-observe-prom` mit Loopback-Endpunkt und
Namensregeln aus `MetricKey`. Klassische Betriebsintegration ohne Collector.

**T3, im Rahmen von W5.** Routing nach Namensraum scharf gestellt, also
`security.*` und `warden.*` auf File-Sink beschränkt, Export nur nach
Operator-Bestätigung.

**T4, im Rahmen von W7.** `harw-observe-otlp` mit HTTP-Transport,
Metrikbrücke zuerst. Trace-Brücke danach, mit Exemplars, sobald die
Rust-Trace-Seite stabil ist. Vorher Kompatibilitätsmatrix prüfen.

---

## 7. Prüfungen

**Inhaltsfreiheit.** Ein Property-Test über alle `Redact`-Implementierungen:
kein Sink-Output enthält je einen Wert der Klasse `Omitted` oder den
Klartext eines `Digest`-Feldes.

**Kardinalitätsgrenze.** Ein Lasttest, der einen Sensor mit hoher
Wertevielfalt fährt und prüft, dass der Sink ablehnt statt zu wachsen.

**Routing.** Ein Test, dass eine `security.*`-Metrik ohne
Operator-Bestätigung ausschließlich im File-Sink landet.

**Namensregeln.** Golden-Test der Prometheus-Ausgabe gegen einen
eingefrorenen Erwartungswert, damit Namens- und Einheitenkonventionen nicht
driften.

**Adapter-Isolation.** Ein CI-Schritt prüft, dass keine öffentliche Signatur
in `harw-observe`, `harw-dod` oder `harw` einen Typ aus der
OpenTelemetry-Familie nennt.

---

## 8. Offene Punkte

1. **Exporterwahl bei hohen Ereignisraten.** Falls `procmon` und `flow`
   mehr liefern, als ein Push-Exporter verträgt, wird vorverdichtet, nicht
   der Transport gewechselt. Die Verdichtungsschwelle fehlt noch.
2. **Retention des File-Sinks.** Rotation nach Größe ist gesetzt, die
   Aufbewahrungsdauer nicht. Sie hängt an der Frage, wie weit forensisch
   zurückgeschaut werden soll.
3. **Histogramm-Buckets.** Für Turn-Latenz und Kontextgrößen sind
   sinnvolle Grenzen erst nach den ersten Messungen aus T1 festzulegen.
   Vorher geraten wäre wertlos.
4. **OTel-Versionsdisziplin.** Vorschlag: die ganze Familie zusammen
   aktualisieren, nie einzeln, und `tracing-opentelemetry` als Leitversion
   nehmen, weil sie die engste Kopplung hat.
