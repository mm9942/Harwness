# W2d-2 — Doku-Agent Z2d-2 / F-CHANGELOG

Rolle: Doku-Agent (Sonnet). BUILD-POLICY eingehalten: kein `cargo`/`make`/
`rustc`-Aufruf, keine git-Schreibbefehle. Nur gelesen und `CHANGELOG.md`
editiert.

Owned: `CHANGELOG.md`, `docs/remediation/ledger/W2d2/F-CHANGELOG.md` (neu).

## Auftrag

Unter `## [Unreleased]` (bestehende Konvention: Format nach Keep a Changelog,
Sprache Englisch, wie in `## [0.2.0]` bereits durchgehend verwendet) neue,
nutzerorientierte Einträge ergänzen, gruppiert nach Added/Changed/Removed/
Security, für die im Brief vorgegebenen zehn Verhaltensänderungen aus
W2d-2 (CONTRACTS-W2d2.md) und zwei zugehörigen W2d-1-Befunden (Web-Decke,
TUI-Shell-Text).

## Gelesen (Read-list)

- `CHANGELOG.md` (Format/Sprache übernommen: `### Added/Changed/Removed/
  Security`, Bullet-Stil, Rückverweise auf Betriebsverhalten statt interne
  Symbolnamen wo möglich).
- `docs/remediation/CONTRACTS-W2d2.md` §4 (Tracking-Liste Verhaltensänderungen).
- `docs/remediation/ledger/W2d2/{M2,W1,J1,J1-F,T2a,CE,E1,R0,R0-F,M1a,M1b,C1,
  F-TUI,T1,T2b,L1}.md` — Abschnitte zu Verhaltensänderungen/Annahmen zur
  Verifikation der zehn Brief-Punkte (z. B. `uid:<n>`-Principal aus M1a/E1,
  `Blocked`-Text aus J1, doppelte MCP-IDs aus M1a, `/tools`-Deckenlogik aus
  CE/W2d-1-F-T, Plan-Knoten-Workspace-Bindung aus J1-F/R0-F).
- `docs/remediation/ledger/W2d1/{F-C,F-T,F-G,F-M,F-W}.md` und die drei
  `Z2d1-*`-Reviewtabellen — zur Bestätigung der Herkunft von „Web-Decke
  ReadWorkspace für alle Tiers“ (F-W/W5) und „Shell-`!`-Befehle abgelehnt“
  (F-T/T6), die im Brief zusammen mit reinen W2d-2-Punkten in einer Zeile
  standen.

## Umsetzung

Vier neue Abschnitte in `CHANGELOG.md` unter `## [Unreleased]` eingefügt,
vor dem bestehenden `### Security`-Abschnitt (der um sieben neue Bullets am
Ende erweitert wurde, ohne bestehenden Text zu verändern):

- **Added**: `harw project trust|untrust|status` + Trust-gebundenes Laden
  der Repo-`.harw`-Ebene.
- **Changed**: `harw web` ohne `--config-dir`/Pflicht-Home + Web-Decke;
  `harw serve` Submitter-/Home-Pflicht; One-shot Modus/Config-Policy +
  Ablehnung statt Rückfrage; `harw run`-Principal; `harw doctor`-Rechte-
  Anzeige; `harw analyze` Provider-Pflicht ohne `--dry-run`.
- **Removed**: `harw web --config-dir`-Flag, `harw-channel-browser`,
  MCP-Client in der Kern-Laufzeitbibliothek.
- **Security** (angehängt): doppelte MCP-Principal-IDs brechen `harw serve`
  ab; `/tools`-Decke Basis∩Modus fail-closed; TUI-Shell-`!`-Ablehnung;
  `[agents]`-Rollen vorerst nicht aus der TUI spawnbar; Gateway ohne
  Echo-Fallback + aufgelöste `secrets:`-Credentials; Plan-Knoten-Sandbox
  exakt am abgeleiteten Workspace; Prompt-Jobs ohne Projektdokumente im
  Kontext.

Jeder Brief-Punkt wurde in mindestens einem Bullet abgebildet; die interne
Fehlerklassen-Kennung „E3“ wurde bewusst nicht in den Nutzertext
übernommen (nur `Blocked` als beobachtbares Ergebnis), ebenso keine
Zeilennummern/Funktionsnamen/lokale Pfade. Keine Agenten-/KI-Erwähnung;
„agent“ wird nur dort verwendet, wo es der literale, bereits bestehende
Konfigurationsabschnittsname (`[agents]`) im Produkt selbst ist.

## Abweichungen vom Brief

Keine inhaltliche Abweichung. Formale Entscheidung: statt eines einzelnen
Mischeintrags pro Brief-Zeile wurden mehrere kurze, in sich abgeschlossene
Bullets je Verhaltensänderung geschrieben (Keep-a-Changelog-Stil, wie im
bestehenden `## [0.2.0]`-Abschnitt), damit jede Änderung einzeln zitierbar
bleibt.

## Verifikation

- `CHANGELOG.md` vor und nach der Änderung vollständig gelesen: bestehender
  Text unverändert, neue Abschnitte an der vorgesehenen Stelle unter
  `## [Unreleased]`, Markdown-Struktur (Bullet-Einrückung, Abschnittstitel)
  konsistent mit dem übrigen Dokument.
- Kein `cargo`/`make`/git-Schreibbefehl ausgeführt (BUILD-POLICY).
- Kein lokaler Dateipfad, keine Zeilennummer, keine Agenten-/KI-Erwähnung im
  neuen CHANGELOG-Text (grep-Sichtprüfung auf `agent` außerhalb von
  `[agents]`, auf `/home/`, auf `Claude`/`Codex`/`KI` — keine Treffer).

## Stubbed imports

Keine (reine Dokumentationsänderung).

## Annahmen

- Der Brief-Punkt „`harw serve` … Einreicher nur konfigurierte Principals“
  wird als Teil des bestehenden Submitter-Gates verstanden (bereits vor
  W2d-2 vorhanden, siehe J1-Ledger `prompt_claim_guard_tests`); hier nur im
  Kontext der neuen Home-Pflicht als Nutzerverhalten mit aufgeführt, keine
  neue Prüfung behauptet.
- „E3“ und andere interne Kennungen aus CONTRACTS-W2d2.md §4 sind
  Tracking-Metadaten für den Remediation-Prozess, nicht Teil des
  nutzerorientierten Textes — bewusst weggelassen.

## Ausgabe

```json
{
  "agent": "F-CHANGELOG",
  "files_created": [
    "docs/remediation/ledger/W2d2/F-CHANGELOG.md"
  ],
  "files_modified": [
    "CHANGELOG.md"
  ],
  "verification": {
    "command": "read-only (BUILD-POLICY: kein cargo/make/git-Schreibbefehl)",
    "exit_code": null,
    "pass": true
  },
  "stubbed_imports": [],
  "assumptions_made": [
    "Einreicher-Beschraenkung (nur konfigurierte Principals) ist bestehendes Verhalten, im Unreleased-Eintrag nur im neuen Home-Pflicht-Kontext mit genannt",
    "Interne Fehlerklassen-Kennungen (z. B. E3) aus CONTRACTS-W2d2.md sind Prozess-Tracking, nicht Teil des Nutzertexts"
  ],
  "blocked": false
}
```
