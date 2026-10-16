# Bible-Konflikt: lokale Checks (coding-philosophy.md) vs. zentrale Wave-Builds (DEC-004)

Datum: 2026-10-16

## 1. Konfliktlage

- **coding-philosophy.md (Z. 567)** sieht vor, dass Worker lokale Checks
  (z. B. `cargo check`, `cargo test`) vor der Rückmeldung ausführen, um
  ihre Arbeit selbst zu verifizieren.
- **DEC-004** (docs/planning/70-decisions/DEC-004-no-parallel-builds.md,
  Status: accepted, 2026-09-27) verbietet Agenten/Workern die Ausführung
  von `cargo`/`rustc`/`make` und begrenzt Verifikation auf genau einen
  zentralen Lauf je Workspace (via `verify.lock`, vgl. DEC-005, Z. 51–52).
- **docs/planning/75-harness-patterns/README.md** („agents never build",
  zentrale Wave-Builds) steht auf der Seite von DEC-004.

Beide Aussagen können nicht gleichzeitig uneingeschränkt gelten: die
Bibel-Anweisung verlangt Worker-seitige Checks, DEC-004 untersagt sie
ausdrücklich.

## 2. Aktueller Umgang im PR136

- DEC-004 gilt: Worker führen keine Builds oder Checks aus.
- Verifikation wird gesammelt und zentral durchgeführt — nach Abschluss
  aller Plan-Knoten durch die Nutzerschnittstelle bzw. Claude Code
  (zentraler Wave-Build über den Workspace).

## 3. Offene Entscheidung (nicht still entschieden)

Es ist noch nicht entschieden, ob

1. **coding-philosophy.md angepasst wird**, damit sie DEC-004-konform
   lokale Worker-Checks nicht mehr vorschreibt, oder
2. **der Konflikt als dokumentierte Ausnahme bestehen bleibt**
   (coding-philosophy.md unverändert; DEC-004 regiert für Builds/Checks,
   lokale Worker-Checks nur, wenn ausdrücklich freigegeben).

Diese Entscheidung muss explizit durch den Nutzer getroffen werden;
sie wird hier bewusst offengehalten und nicht still entschieden.

## Verwandt

- DEC-003 (Signale/Commit-Struktur, related)
- DEC-004 — Keine parallelen Builds (Kern des Konflikts)
- DEC-005 — Kleine Scopes/Waves (Z. 51–52: `verify.lock`)
- DEC-006 (related)
- DEC-007 — Worker-Rechte (Grenzen der Worker-Werkzeuge)
