# Lokale Branch-Konsolidierung vom 30. Oktober 2026

Geprüft wurden **alle 79 Online-Branches** von `mm9942/Harwness`, nicht nur
die zuvor lokal sichtbaren `dev`- und PR-Referenzen. Zusätzlich wurden alle
**158 abrufbaren PR-Heads** berücksichtigt. 19 davon waren aus keinem der
aktuellen Online-Branches erreichbar und wurden gesondert geprüft.

Diese Arbeit bleibt lokal. Es wurden keine Branches gepusht oder auf GitHub
gelöscht, keine PRs geschlossen und keine Dienste oder Webhooks aktiviert.

## Stände und Verfahren

- Lokaler Ausgangspunkt: `23cd2e4e1b3fb629ecd9cb18aa09f493befe4d28`.
- Erster Online-dev-Snapshot: `3929f159a57c1df65c60861d5ff206d63a769740`.
- Während der Prüfung extern aktualisiertes Online-dev:
  `287777ba50d1ea02b51951d75823d0ca5c23d6a5` (inklusive PR #163).
- Online-main: `843054a35b7eb0297ca26973a4c58cad2ff624af`.

Die paginierte `gh`-Branchliste wurde mit `git ls-remote --heads` verglichen.
Alle Branch- und PR-Heads wurden abgerufen. 38 der ursprünglichen 79 Heads
waren bereits Vorfahren des ersten Online-dev-Snapshots. Die übrigen 41
wurden zusätzlich anhand ihrer Patches, Dateien und heutigen Implementierungen
geprüft. Ein anderer Commit-Hash oder ein fehlgeschlagener Probe-Merge gilt
ausdrücklich nicht allein als Nachweis fehlenden Codes.

[branches.csv](branches.csv) enthält eine Zeile für jeden Online-Branch.
[inventory.json](inventory.json) enthält die SHAs, Einzelentscheidungen,
Belege und alle PR-Zuordnungen. Die Angaben unter `content_review` dokumentieren
den Befund vor der Wiederherstellung; `local_resolution` erklärt die Behandlung
in dieser Konsolidierung.

## Wiederhergestellt

| Herkunft | Lokale Umsetzung |
|---|---|
| PR #116, `922b12a8` | Atomare Reservierung noch startender Jobs, angepasst an die heutige asynchrone Laufzeit und veränderliche Job-Obergrenze. Normale und gepipte Starts teilen dasselbe Limit; Fehler und Abbrüche geben Reservierungen frei. |
| `coop/deployment-profile` | Reine DeploymentProfile-/Service-Tabelle mit ihren Tests. Sie ist ein Kompositionsvertrag, keine bereits angeschlossene Laufzeit- oder Berechtigungsdurchsetzung. |
| `chatgpt/cycle-composed-agents-20261008` | Fehlende Fehler-, Budget- und Evidenzregeln selektiv in die aktuellen Rollenregeln übernommen. Der heutige Ablauf über Abschlussmeldungen bleibt erhalten. Die Regeln sind innerhalb des bestehenden Budgets von je 1.700 Bytes verdichtet. |
| `paper/harw-runtime-paper` | Neun historische Paper-/Webhook-Dateien wiederhergestellt. Die aktuelle Veröffentlichung bleibt unter `docs/research/harwness-paper`; die alten Werkzeuge sind ausdrücklich separat und opt-in. |
| `coop/tools-gap` | Historischer PL87-Bericht mit Datums- und Statushinweis. |
| PR #118 | Fehlende sachliche PL92-Korrekturen zu ETags, Exporten, Injection und Egress. Der Plan bleibt als historischer Entwurf gekennzeichnet. |
| `refactor/rename-dod-directory` | Während der Prüfung bereits online gemergt; durch Fast-forward übernommen, einschließlich neuerer Manuskriptänderungen. |

Die Gesamtprüfung fand zusätzlich einen vorhandenen Fehler im optionalen
Cloud-Gateway-Feature. Die Pingora-Feature-Verknüpfung und die erforderliche
Schnittstelle wurden ergänzt. Der Platzhalter verweigert jede Weiterleitung
mit HTTP 501; sechs Pakettests und dessen Clippy-Prüfung bestehen.

Im vollständigen Testlauf wurde außerdem eine bestehende Abhängigkeit von
globalem Testzustand sichtbar: vier Agent-Runner-Tests verwendeten einen
Command-Port mit bereits entferntem temporären Job-Store. Sie erhalten nun
jeweils einen eigenen Port, dessen Verzeichnis während des Tests bestehen
bleibt. Produktionscode und Test-Erwartungen wurden dabei nicht verändert.

Der GNU-Umgebungsvergleich berücksichtigte außerdem die bereits vorhandene
Begrenzung langer Werte auf 1.024 Zeichen nicht. Sein Test prüft nun bei
gekürzten Werten Länge, Originalpräfix, Auslassungszeichen und Markierung;
kurze Werte müssen weiterhin exakt übereinstimmen. Der Produktivpfad bleibt
unverändert.

## Bewusst erhaltene aktuelle Implementierungen

- Der frühere P2-Podman-/Worker-Prototyp ist unvollständig und durch den
  OCI-Executor ersetzt. Er verwendet `spec.program` an der Image-Position,
  behauptet nicht gesetzte Sicherheitsflags und hat keinen passenden
  `/worker`-Operationspfad. Er wird nicht wieder aktiviert.
- PR #66 enthält eine nicht angeschlossene n8n-Bridge. Ihre Credentials- und
  Transportpfade passen nicht zu den heutigen Profil-, Secret- und
  Egress-Grenzen. Der Quellstand bleibt in der Historie, ohne ihn als
  funktionierende TUI-Integration auszugeben.
- Der alte Blocking-Handoff-Patch prüft die Pausierbarkeit des falschen
  Beteiligten und behebt das Warten nicht vollständig. Die aktuelle
  Hintergrund-Delegation bleibt maßgeblich.
- Alte Lockfile-Auflösungen, CI-Diagnoseänderungen und pauschale englische
  Übersetzungen überschreiben keine neueren Inhalte. Die Übersetzungen bleiben
  als eigener historischer Stand zugänglich; es fehlt dadurch kein ausführbarer
  Rust-Code.
- Cloudflare-Adaptive-Lanes, Qwen-Experimente, Common-Command-Tools,
  Skill-Trigger, Container- und Async-Runtime-Arbeit sind bereits integriert
  oder durch geprüfte Nachfolger ersetzt. Die Einzelbelege stehen im Inventar.

## Erhalt der Historie und spätere Bereinigung

Nach der Inhaltsabstimmung wurden die geprüften Heads durch einen ausdrücklich
als solchen bezeichneten **History-only-Merge** in der lokalen Historie erhalten.
Diese Merges erhalten die Herkunft und Wiederherstellbarkeit; sie behaupten
nicht, dass jeder alte Prototyp Teil der aktiven Software ist.

Das zusätzliche, geprüfte Vorher-Bundle liegt auf diesem Host unter
`/root/harw-release/branch-consolidation-20261030/before-consolidation.bundle`.
Die Prüfung bestätigt für alle **174 unterschiedlichen Quellstände**, dass sie
Vorfahren von `02eddce300eec94ea6231e4076a9456ae2653f83` sind. Dieser Merge
hat den Dateiinhalt unverändert gelassen. Die abschließenden Dokumentations-
Commits bauen darauf auf.

Lokal bleiben nur `main` und `dev` sowie deren `origin/main`-/`origin/dev`-
Tracking-Referenzen. 240 zusätzlich importierte lokale Tracking-/Audit-Refs
wurden erst nach Vorfahrenprüfung und geprüftem vollständigem Bundle mit
erwarteten alten SHAs atomar entfernt. Die Fetch-Konfiguration verfolgt nun
`main` und `dev`. **Auf GitHub bleiben unverändert alle 79 Branches bestehen.**

Prüflogs, Review und Git-Nachweise liegen dauerhaft unter
`/root/harw-release/branch-consolidation-20261030/evidence/`. Das abschließende
Bundle heißt dort im übergeordneten Verzeichnis `final-main-dev.bundle`.

[remote-cleanup-proposal.json](remote-cleanup-proposal.json) ist nur ein
Vorschlag für 77 spätere Remote-Löschungen. Er bewahrt `main` und `dev`.
Zum erfassten Zeitpunkt gab es keine offenen PRs. Vor irgendeiner späteren
Remote-Aktion sind neue ausdrückliche Freigabe, erneuter Fetch und Vergleich
sämtlicher erwarteter SHAs erforderlich. Zwischenzeitliche Änderungen dürfen
nicht durch eine alte Löschliste verloren gehen.

## Prüfung

Die maschinenlesbaren Ergebnisse stehen in [verification.json](verification.json).
Der Job-Regressionsnachweis ist zuerst rot und anschließend grün; die erste
vollständige Prüfung des betroffenen Pakets bestand 110 Tests. Die historische
Paper-Testsuite bestand 11 isolierte Tests. Der Python-Testloader wurde für
korrekte Dataclass-Importe repariert, ohne die produktiven Webhook-Skripte zu
verändern.

Das unabhängige Review fand keine kritische oder wesentliche Regression.
Ein zusätzlicher Test für eine Limitänderung genau während eines bereits
ausstehenden Starts wäre eine mögliche Ergänzung; ausstehende Starts und
veränderliche Limits sind bisher separat geprüft.

Der abschließende vollständige Lauf (`--workspace --all-features --no-fail-fast`)
erreichte 18.545 bestandene, einen fehlgeschlagenen und 202 ignorierte Tests.
Der einzige fehlgeschlagene Target war der oben beschriebene GNU-Vergleich.
Nach dessen reiner Testkorrektur bestand der gesamte betroffene Target alle
sieben Tests. Damit sind 18.546 Testergebnisse erfolgreich nachgeprüft;
es wird kein einzelner vollständig grüner Gesamtlauf nach dieser letzten
Testkorrektur behauptet. Alle anderen Targets einschließlich Doc-Tests liefen
vollständig durch. Architektur-Gates, Formatprüfung und das abschließende Workspace-Clippy
(alle Targets, alle Features, Warnungen als Fehler) sind grün.

Die erste Prüfung mit vollem Debug-Output erschöpfte beim Linken den freien
Datenträger. Der erfolgreiche Prüfpfad nutzt die vorhandenen CI-Einstellungen
`line-tables-only`, einen Cargo-Job und keinen inkrementellen Cache. Entfernt
wurden nur regenerierbare Build-Ausgaben und Debug-Symbole fertiger Testdateien.
Installierte Release-Binaries und laufende Dienste blieben unberührt.
