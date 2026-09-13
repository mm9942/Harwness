// Minimale Testeinrichtung für diesen Knoten (UI-01).
//
// Es gab beim Anlegen dieses Knotens noch keine Testeinrichtung unter
// `webui/` — diese Datei ist bewusst die kleinstmögliche, die die Tests aus
// `lib/__tests__/**` und `components/ui/__tests__/**` trägt: `vitest` als
// Runner (kein Jest — kein zweiter Test-Stack neben dem, was `webui/`
// ohnehin über Next.js/Vite-kompatibles Tooling erwartet), `jsdom` als
// DOM-Umgebung (der Datenblock-Test braucht ein echtes DOM, um zu prüfen,
// dass kein `<img>`/`<a>` entsteht). Auf `@vitejs/plugin-react` wird
// verzichtet: `esbuild.jsx = "automatic"` reicht für die JSX-Transformation
// in Tests aus und spart eine weitere Abhängigkeit.
//
// Diese Tests werden von diesem Knoten aus **nicht ausgeführt** (harte
// Regel: kein `npm`/`pnpm`/`yarn` in diesem Auftrag) — sie stehen bereit für
// den zentralen, sequentiellen Verifikationslauf.
import { defineConfig } from "vitest/config";

export default defineConfig({
  esbuild: {
    jsx: "automatic",
  },
  resolve: {
    // Die Route-Gruppen unter `app/**` importieren über `@/` --
    // ohne diesen Alias findet der Testlauf `lib/` und `components/`
    // nicht, und die Tests von UI-02 bis UI-07 laufen ins Leere.
    alias: { "@": new URL("./", import.meta.url).pathname },
  },
  test: {
    environment: "jsdom",
    include: ["lib/**/*.test.{ts,tsx}", "components/**/*.test.{ts,tsx}",
      // Die Route-Gruppen unter `app/**` tragen eigene Tests (UI-02..07).
      // Ohne diesen Eintrag findet der zentrale Lauf sie nicht.
      "app/**/*.test.{ts,tsx}",
    ],
  },
});
