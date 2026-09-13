// Sicherheitstests für die generische Listenfläche (Knoten UI-05).
//
// Deckt die harten Auflagen des Auftrags ab:
// - ein Dateipfad mit `<img src=x onerror=...>` erscheint als Text, nie als
//   Markup (auch wenn er wie ein Beweis-/Sensorfeld aussieht);
// - ein Dateipfad wird nicht zum Link — auch nicht durch Auto-Verlinkung;
// - „keine Daten verfügbar" ist von „keine Befunde" unterscheidbar
//   (verschiedene `data-testid`, verschiedener Text);
// - es gibt keinen Knopf, der eine Durchsetzungsaktion auslöst (kein
//   `<button>` in dieser rein lesenden Fläche überhaupt).
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { SecurityListPanel } from "../_components/SecurityListPanel";
import type { SecurityFetchResult } from "../_lib/types";

interface FakeFinding {
  readonly path: string;
}

function renderPanel(result: SecurityFetchResult<FakeFinding>): string {
  return renderToStaticMarkup(
    <SecurityListPanel
      title="Befunde"
      result={result}
      itemLabel="Befund"
      toLines={(item) => [`Pfad: ${item.path}`]}
    />,
  );
}

describe("SecurityListPanel", () => {
  it("zeigt einen bösartigen Dateipfad als reinen Text, nie als <img>-Markup", () => {
    const payload = "<img src=x onerror=alert(1)>";
    const html = renderPanel({ status: "ok", items: [{ path: payload }] });
    const dom = new DOMParser().parseFromString(html, "text/html");

    expect(dom.querySelectorAll("img").length).toBe(0);
    expect(html).toContain("&lt;img");
    expect(dom.querySelector("pre")?.textContent).toBe(`Pfad: ${payload}`);
  });

  it("verwandelt einen Dateipfad nie in einen Link, auch nicht automatisch", () => {
    const payload = "/var/log/auth.log; rm -rf / #";
    const html = renderPanel({ status: "ok", items: [{ path: payload }] });
    const dom = new DOMParser().parseFromString(html, "text/html");

    expect(dom.querySelectorAll("a").length).toBe(0);
    expect(html).not.toContain("href=");
  });

  it("unterscheidet 'keine Daten verfügbar' (Route fehlt) von 'keine Befunde' (leere Antwort)", () => {
    const missingHtml = renderPanel({ status: "route-missing", operation: "security.findings.list" });
    const emptyHtml = renderPanel({ status: "ok", items: [] });

    const missingDom = new DOMParser().parseFromString(missingHtml, "text/html");
    const emptyDom = new DOMParser().parseFromString(emptyHtml, "text/html");

    expect(missingDom.querySelector('[data-testid="security-no-data"]')).not.toBeNull();
    expect(missingDom.querySelector('[data-testid="security-no-findings"]')).toBeNull();

    expect(emptyDom.querySelector('[data-testid="security-no-findings"]')).not.toBeNull();
    expect(emptyDom.querySelector('[data-testid="security-no-data"]')).toBeNull();

    // Der sichtbare Text darf sich nicht überschneiden — sonst könnte ein
    // Bediener beide Fälle verwechseln.
    expect(missingHtml).not.toContain("Keine Befunde");
    expect(emptyHtml).not.toContain("Keine Daten verfügbar");
  });

  it("meldet einen Fehler ebenfalls als 'keine Daten verfügbar', nie als 'keine Befunde'", () => {
    const errorHtml = renderPanel({
      status: "error",
      operation: "security.findings.list",
      message: "Zeitüberschreitung",
    });
    const dom = new DOMParser().parseFromString(errorHtml, "text/html");
    expect(dom.querySelector('[data-testid="security-no-data"]')).not.toBeNull();
    expect(dom.querySelector('[data-testid="security-no-findings"]')).toBeNull();
  });

  it("rendert keinen einzigen <button> — diese Fläche löst keine Durchsetzungsaktion aus", () => {
    const results: readonly SecurityFetchResult<FakeFinding>[] = [
      { status: "route-missing", operation: "security.findings.list" },
      { status: "ok", items: [] },
      { status: "ok", items: [{ path: "/etc/passwd" }] },
      { status: "error", operation: "security.findings.list", message: "boom" },
    ];
    for (const result of results) {
      const html = renderPanel(result);
      const dom = new DOMParser().parseFromString(html, "text/html");
      expect(dom.querySelectorAll("button").length).toBe(0);
    }
  });

  it("rendert mehrere Befunde einzeln, ohne sie zusammenzuführen", () => {
    const html = renderPanel({
      status: "ok",
      items: [{ path: "/a" }, { path: "/b" }],
    });
    const dom = new DOMParser().parseFromString(html, "text/html");
    const pres = Array.from(dom.querySelectorAll("pre")).map((el) => el.textContent);
    expect(pres).toEqual(["Pfad: /a", "Pfad: /b"]);
  });
});
