# TEMP · CRITICAL: TUI in schmalem Hochformat

> **Temporäres Review-Material für Opus.** Dieser Branch und sein Screenshot sind nur für Diagnose und Umsetzung. Den PR nicht in `dev` mergen; den Bildbeleg nach der Übernahme des Befunds entfernen.

## Sichtbarer Befund

Foto der laufenden Harw-TUI am 28.09.2026: [portrait-tui-2026-09-28.jpg](./portrait-tui-2026-09-28.jpg).

Die Harw-TUI läuft in einer schmalen, hochkant proportionierten Terminalspalte. Am unteren Rand werden `Agenten: … 2 aktiv …` sowie `Goal: Har…` und weitere Metadaten rechts abgeschnitten. Die für die Steuerung relevanten Angaben sind dort nicht vollständig lesbar. Gleichzeitig liegt rechts außerhalb dieser Terminalspalte freie Monitorfläche; entscheidend ist daher die **tatsächliche Terminalbreite**, nicht die physische Bildschirmbreite. Der Input bleibt sichtbar, aber Status und Agentenüberblick sind nur fragmentarisch.

## Auftrag an Opus

1. Reproduziere die TUI bei schmalen Terminalgrößen und prüfe besonders den Fußbereich mit Agentenstatus, Goal/Fortschritt, Modus und aktiver Eingabe.
2. Sorge dafür, dass wesentliche Zustände auch bei Hochformatbreite erreichbar und vollständig lesbar sind: umfließen, sinnvoll priorisieren oder eine fokussierbare Detailansicht anbieten. Kein stilles Abschneiden relevanter Information.
3. Prüfe Größenänderungen während der Laufzeit sowie längere deutsche Texte, Agentennamen und Goal-Titel. Eingabe, Fokus und Scrollposition müssen bedienbar bleiben.
4. Zeige die Korrektur mit einem Vorher/Nachher-Beleg bei realistischen schmalen Terminalmaßen; dokumentiere die gemessenen Größen und die verbleibenden Grenzen.

**Priorität: kritisch für mobile/schmale TUI-Nutzung.** Dieser Screenshot belegt den Zustand, nicht die Ursache. Bitte die Ursache im Layout und die wirksame Korrektur anhand des Codes überprüfen.
