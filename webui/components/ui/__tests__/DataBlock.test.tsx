// Sicherheitstest für den Datenblock-Renderer (siehe DataBlock.tsx-Kopf).
//
// Prüft mit einer aktiv bösartigen Eingabe (HTML-Injektion) und einer
// Markdown-Link-Eingabe, dass `DataBlock` beides ausschließlich als Text
// ausgibt — kein `<img>`, kein `<a>`, kein interpretiertes Markup.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { DataBlock, DataBlockList } from "../DataBlock";

describe("DataBlock", () => {
  it("gibt eine HTML-Injektion als reinen Text aus, nie als Markup", () => {
    const payload = '<img src=x onerror=alert(1)>';
    const html = renderToStaticMarkup(<DataBlock value={payload} />);

    // Die rohe Payload darf im ausgegebenen HTML nicht als echtes Tag
    // auftauchen — React muss sie als Text-Entity escapt haben.
    expect(html).not.toContain("<img src=x");
    expect(html).toContain("&lt;img");

    const dom = new DOMParser().parseFromString(html, "text/html");
    expect(dom.querySelectorAll("img").length).toBe(0);
    expect(dom.querySelector("pre")?.textContent).toBe(payload);
  });

  it("verwandelt einen Markdown-Link nicht in ein <a>-Element", () => {
    const payload = "[hier klicken](http://evil.example/steal)";
    const html = renderToStaticMarkup(<DataBlock value={payload} />);

    const dom = new DOMParser().parseFromString(html, "text/html");
    expect(dom.querySelectorAll("a").length).toBe(0);
    expect(dom.querySelector("pre")?.textContent).toBe(payload);
  });

  it("leitet niemals ein href aus dem angezeigten Wert ab", () => {
    const payload = "https://example.com/looks-like-a-link";
    const html = renderToStaticMarkup(<DataBlock value={payload} />);
    expect(html).not.toContain("href=");
  });

  it("DataBlockList rendert jede Zeile einzeln als Text, ohne Zusammenführung", () => {
    const lines = ["<script>evil()</script>", "normale Zeile"];
    const html = renderToStaticMarkup(<DataBlockList values={lines} />);
    const dom = new DOMParser().parseFromString(html, "text/html");
    expect(dom.querySelectorAll("script").length).toBe(0);
    const pres = Array.from(dom.querySelectorAll("pre")).map((el) => el.textContent);
    expect(pres).toEqual(lines);
  });

  it("zeigt die optionale Beschriftung, ohne sie mit dem Wert zu vermengen", () => {
    const html = renderToStaticMarkup(<DataBlock label="Dateiname" value="../../etc/passwd" />);
    const dom = new DOMParser().parseFromString(html, "text/html");
    expect(dom.querySelector(".harw-data-block-label")?.textContent).toBe("Dateiname");
    expect(dom.querySelector("pre")?.textContent).toBe("../../etc/passwd");
  });

  it("setzt data-harw-trust auf 'data', wenn keine trust-Prop übergeben wird", () => {
    const html = renderToStaticMarkup(<DataBlock value="unbekannte Herkunft" />);
    const dom = new DOMParser().parseFromString(html, "text/html");
    expect(dom.querySelector(".harw-data-block")?.getAttribute("data-harw-trust")).toBe("data");
  });

  it("gibt eine übergebene trust-Prop unverändert als data-harw-trust weiter", () => {
    const html = renderToStaticMarkup(<DataBlock value="geprüfter Fund" trust="evidence" />);
    const dom = new DOMParser().parseFromString(html, "text/html");
    expect(dom.querySelector(".harw-data-block")?.getAttribute("data-harw-trust")).toBe("evidence");
  });

  it("DataBlockList reicht ihre trust-Prop an jede Zeile weiter", () => {
    const html = renderToStaticMarkup(
      <DataBlockList values={["a", "b"]} trust="instruction" />,
    );
    const dom = new DOMParser().parseFromString(html, "text/html");
    expect(dom.querySelector(".harw-data-block-list")?.getAttribute("data-harw-trust")).toBe(
      "instruction",
    );
    const rows = Array.from(dom.querySelectorAll(".harw-data-block"));
    expect(rows.every((row) => row.getAttribute("data-harw-trust") === "instruction")).toBe(true);
  });
});
