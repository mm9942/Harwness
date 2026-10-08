# H25 — Schneller Agent-/Modellwechsel: UX-/Vertragsentwurf

Status: Contract-Draft (Befunde verifiziert am Quellcode; offene Punkte am Ende). Wichtig: **KEIN Hot-Switch belegt** — siehe unten.

## Befundene Fakten (mit Belegen)

1. **ModelDescriptor mit Preisen**: Modellmetadaten umfassen Preisdaten (Input/Output), die budgetär relevant sind. Beleg: `descriptor.rs:338-376`.
2. **Eingebettete providers.toml**: Provider-Konfiguration ist als eingebettete Datei ausgeliefert. Beleg: `embedded.rs:12-37`.
3. **Budget-Obergrenze nur verschärfend**: Admission kann bestehende Budget-Obergrenzen nur verschärfen, nie lockern. Belege: `admission.rs:133-145`, `admission.rs:641-669`.
4. **Kein Hot-Switch**: An **laufenden** Agents sind nur Nachrichten- und Interaktionsmodus-Wechsel belegt; ein Wechsel des Modells eines laufenden Agents ist nicht belegt. Belege: `child_comms.rs:1366-1397`, `live_mode.rs:277-307`.

## Vertragsspezifikation

### Neue Agents: voller Wechsel
- Bei Agent-Start (neue Session/neuer Agent) ist die Auswahl von Modell und Provider vollständig frei — unter den Grenzen unten.
- Modellwahl erfolgt über `ModelDescriptor` (inkl. Preise, Befund 1) aus der eingebetteten providers.toml (Befund 2) — keine geratenen Modelle, keine Versionsannahme ohne Descriptor.
- UX: Modellauswahl im Erstell-/Composer-Flow; Preis/Token-Angaben sichtbar, damit die Budgetentscheidung informiert erfolgt.

### Laufende Agents: nur soweit belegt
- Zulässig an laufenden Agents: **Nachrichten senden** und **Interaktionsmodus wechseln** (Befund 4).
- **Nicht zulässig / nicht behauptet**: Modellwechsel oder Providerwechsel am laufenden Agent („Hot-Switch“). Ein Modellwechsel erfordert einen neuen Agent (neue Session) — die UX muss das deutlich machen (z. B. „Modell wechseln → neuen Agent starten“-Fluss mit Kontextübergabe, ohne zu behaupten, derselbe Agent werde fortgesetzt).

### Grenzen (Wiederverwendung aus h2–h4)
- **Budget**: Wechsel darf die Budget-Obergrenze nur verschärfen, nie lockern (Befund 3). Bei Modellauswahl gegen teureres Modell muss der Nutzer die verschärfte Grenze bestätigen oder die Auswahl bleibt abgelehnt.
- **Rechte/Allowlist**: Rollen- und Capability-Grenzen aus h2–h4 gelten unverändert; ein Modellwechsel verleiht keine zusätzlichen Rechte. Provider-Allowlist-Durchsetzung ist offen (siehe unten) — bis zur Verifizierung gilt: nur Provider aus der eingebetteten providers.toml, keine externen Provider-URLs.
- **Rollen→Modell-Zuordnung**: ob bestimmte Rollen auf bestimmte Modelle beschränkt sind, ist offen; ohne Beleg wird keine Einschränkung behauptet.

## Offene Punkte
- **Provider-Allowlist-Durchsetzung** nicht lokalisiert (ob Nicht-allowlistete Provider abgewiesen werden).
- **Rollen→Modell-Zuordnung** nicht lokalisiert.
- **Keine Hot-Switch-Behauptung**: kein UX-Text und kein Vertragspunkt darf Modellwechsel am laufenden Agent versprechen.

## Tests
- Modellauswahl neuer Agent: alle Modelle aus providers.toml wählbar; Descriptor-Preise korrekt angezeigt.
- Budget: Wechsel zu teurerem Modell ohne Bestätigung → abgelehnt; mit Bestätigung → Obergrenze verschärft, nie gelockert.
- Laufender Agent: Nachrichten- und Interaktionsmodus-Wechsel funktionieren; Modell-/Providerwechsel wird abgelehnt bzw. als Neustart-Flow angeboten.
- Kein UX-Pfad, der Hot-Switch suggeriert (Text/Assertions).
