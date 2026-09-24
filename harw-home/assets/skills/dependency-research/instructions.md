# Abhängigkeiten recherchieren — bewerten, bevor man sie aufnimmt

**Regel:** Eine Abhängigkeit wird erst aufgenommen oder aktualisiert, wenn **Pflege, Lizenz, Sicherheit, API-Passung und Alternativen** aus **autoritativen Quellen** geprüft sind. Jede Aussage im Ergebnis nennt ihre Quelle; Unbekanntes wird als unbekannt markiert.

**Warum:** Jede Abhängigkeit ist fremder Code mit Zugriff auf alles, was das Programm darf. Sie bringt ihre eigenen Abhängigkeiten, Lizenzpflichten und Sicherheitslücken mit — und muss über Jahre gepflegt werden. Eine Stunde Recherche ist billiger als eine spätere Migration oder ein Sicherheitsvorfall.

## Wann anwenden

- Eine neue Bibliothek, ein Werkzeug oder ein Paket soll hinzugefügt werden.
- Eine Version soll angehoben werden (besonders Major-Versionen).
- Zwischen mehreren Kandidaten muss entschieden werden.
- Eine bestehende Abhängigkeit wirkt verwaist oder hat eine gemeldete Lücke.

## Vorab: Brauchen wir sie überhaupt?

- Löst die Standardbibliothek oder eine **bereits vorhandene** Abhängigkeit das Problem?
- Sind es nur wenige Zeilen, die man selbst schreiben und testen kann?
- Wie viel der Bibliothek würde tatsächlich genutzt?

## Bewertungskriterien

| Kriterium | Prüfen | Warnsignale |
|---|---|---|
| **Pflege** | letzte Veröffentlichung, Commit-Aktivität, Reaktion auf Issues, Anzahl Maintainer | seit Jahren kein Release, offene Sicherheits-Issues ohne Antwort, ein einzelner Maintainer |
| **Lizenz** | Lizenz des Pakets **und** seiner transitiven Abhängigkeiten | fehlende Lizenz, Copyleft in proprietärem Kontext, Lizenzwechsel zwischen Versionen |
| **Sicherheit** | Advisory-Datenbanken, Changelog, bekannte CVEs, Umgang mit Meldungen | ungepatchte Lücken, keine Sicherheitsrichtlinie |
| **Herkunft** | offizielles Repository, Namensgleichheit Registry ↔ Quellcode, signierte Releases | Tippfehler-Namen (Typosquatting), Paket ohne verlinkten Quellcode, neuer Besitzer |
| **API-Passung** | deckt es den Anwendungsfall, passt es zu unseren Laufzeit-/Plattformannahmen | zwingt eigene Laufzeit/Architektur auf, viele Workarounds nötig |
| **Kompatibilität** | Mindestversion der Sprache/Laufzeit, Plattformen, Konflikte mit vorhandenen Versionen | inkompatible Versionsanforderungen, doppelte Versionen im Baum |
| **Umfang** | Anzahl transitiver Abhängigkeiten, Build-Zeit, Binärgröße, native Komponenten | großer Baum für kleine Funktion, Build-Skripte mit Netzwerkzugriff |
| **Stabilität** | Versionierungsschema, Häufigkeit von Breaking Changes, Dokumentation | 0.x mit häufigen Brüchen ohne Migrationshinweise |

## Ablauf

1. **Anforderung formulieren:** Was genau soll die Abhängigkeit leisten? Welche Randbedingungen (Lizenz, Plattform, Laufzeit)?
2. **Kandidaten sammeln:** zwei bis drei, inklusive „selbst schreiben“ und „vorhandene Abhängigkeit nutzen“.
3. **Primärquellen lesen:** Registry-Seite, offizielles Repository, Changelog, Sicherheitsrichtlinie, offizielle Dokumentation. Blogposts und Foren nur ergänzend.
4. **Kriterien prüfen** (Tabelle oben) und je Kandidat festhalten.
5. **Aktuelle Version** aus der Registry ermitteln — nie aus dem Gedächtnis.
6. **Empfehlung** mit Begründung und Restrisiken; offene Punkte klar benennen.

## Ökosystem-Hinweise

- **Cargo (Rust):** crates.io und docs.rs; `cargo tree` für den Abhängigkeitsbaum, `cargo audit` (RustSec) für Advisories, `cargo deny` für Lizenz-/Quellenregeln. Features prüfen — oft lässt sich der Umfang mit `default-features = false` stark reduzieren.
- **npm (JavaScript/TypeScript):** npmjs.com-Seite, `npm ls`, `npm audit`; `postinstall`-Skripte besonders kritisch prüfen. Auf Typdefinitionen und ESM/CJS-Kompatibilität achten.
- **pip (Python):** PyPI-Seite, `pip-audit`, `pipdeptree`; prüfen, ob Wheels für die Zielplattformen existieren oder native Builds nötig sind. Unterstützte Python-Versionen beachten.
- **Go:** pkg.go.dev, `go mod graph`, `govulncheck`; Modulpfad muss zum Repository passen, Major-Versionen ≥ 2 stehen im Importpfad.

Allgemein: Lockfiles committen, Versionen bewusst festlegen, Änderungen am Lockfile im Review ansehen.

## Ergebnisformat

```text
Anforderung: <ein Satz>
Kandidaten:  A (v1.4.2), B (v0.9.1), eigene Implementierung
A: Pflege gut (Release vor 3 Wochen, 4 Maintainer) — Lizenz MIT — keine offenen Advisories
   API passt; +6 transitive Pakete. Quelle: <URLs>
B: letzte Veröffentlichung vor 2 Jahren; offenes Advisory <ID>. Quelle: <URLs>
Empfehlung: A, weil …
Restrisiken / offen: Lizenz der transitiven Abhängigkeit X nicht eindeutig.
```

## Fallstricke

- **Download-Zahlen ≠ Qualität:** Beliebt heißt nicht gepflegt oder sicher.
- **Versionsnummer aus dem Gedächtnis:** immer in der Registry nachsehen.
- **Transitive Lizenzen vergessen:** Die Lizenz des direkten Pakets reicht nicht.
- **Recherche ist nicht Installation:** Dieser Skill empfiehlt; das Hinzufügen selbst ist ein eigener, freigabepflichtiger Schritt.
- **Unbelegte Aussagen:** „gilt als sicher“ ohne Quelle ist keine Bewertung.

## Checkliste

1. Ist eine neue Abhängigkeit überhaupt nötig?
2. Pflege, Lizenz (inkl. transitiv), Sicherheit, Herkunft geprüft?
3. API-Passung und Kompatibilität mit vorhandenem Baum geprüft?
4. Aktuelle Version aus der Registry, nicht geraten?
5. Alternativen verglichen?
6. Jede Aussage mit Quelle; Unbekanntes als unbekannt markiert?
