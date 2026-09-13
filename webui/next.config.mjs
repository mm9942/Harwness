// Next.js-Konfiguration der Control-Plane-Schale (Knoten UI-01).
//
// Bewusst minimal: diese Schale hat keinen eigenen API-Layer (kein
// `app/api/**`) und keinen zweiten Autoritätspfad — jeder Datenzugriff läuft
// über `lib/webClient.ts` gegen die von `harw-web` (UI-00) aus der Registry
// gebauten Routen. `next.config.mjs` hat hier deshalb nichts zu proxen oder
// umzuschreiben.
/** @type {import('next').NextConfig} */
const nextConfig = {
  reactStrictMode: true,
  // `harw-web` bindet auf einen Unix-Socket, nicht auf einen HTTP-Port —
  // die tatsächliche Anbindung (Socket vs. lokaler Reverse-Proxy) ist
  // Sache der Bereitstellung, nicht dieser Konfigurationsdatei. Siehe
  // `lib/webClient.ts` für die eine Stelle, die `fetch` aufruft.
};

export default nextConfig;
