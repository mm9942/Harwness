// Lokale ID-Erzeugung für Chat-Züge (Knoten UI-02).
//
// `crypto.randomUUID()` ist nicht in jeder Test-/Laufzeitumgebung
// garantiert vorhanden (ältere Node-Versionen, bestimmte jsdom-Konfigu-
// rationen) — statt einer harten Abhängigkeit darauf reicht hier ein
// einfacher, kollisionsarmer Zähler plus Zeitstempel, da diese IDs nie das
// Gerät verlassen und nur lokale React-Keys/Signal-Korrelation bedienen.
let counter = 0;

/**
 * Erzeugt eine lokal eindeutige, nicht kryptografische Kennung.
 *
 * # Description
 * Kombiniert einen monoton steigenden Zähler mit `Date.now()`, um
 * Kollisionen innerhalb eines Tabs auszuschließen. Nicht für
 * sicherheitsrelevante Zwecke geeignet — nur für React-Keys und die
 * clientseitige Korrelation von Daumen-Runter-Signalen zu einem Chat-Zug.
 *
 * # Returns
 * Einen String der Form `"<counter>-<timestamp>"`.
 */
export function nextLocalId(): string {
  counter += 1;
  return `${counter}-${Date.now()}`;
}
