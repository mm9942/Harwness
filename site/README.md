# Harwness website (harw.dev)

Static, dependency-free site: no backend, no external fonts, no analytics.

## Layout

- `src/` - hand-maintained sources (HTML, CSS, JS, `crates.json`, brand assets). Source of truth.
- `build_content.py` - copies `src/` to `dist/` (generated, git-ignored).
- `validate_site.py` - checks `dist/`: unique IDs, one H1 per page, local links/anchors, EN/DE defaults, status entries, pinned snapshots, wordmark SHA-256, no storage/cookie/language-detection in `current.js`.
- `translations.en.json` - German-to-English phrase map used while producing the English root page.
- `legacy/build_content_v030.py` - the original v0.3.0 page generator, for reference only. Its output differs from `src/` (the pages were later edited by hand); do not use it to build.
- `QA.md` - QA report of the 2026-10-30 release.

```
make site         # python3 -I site/build_content.py
make site-check   # build + python3 -I site/validate_site.py
```

Deployment: see `deploy/site/README.md`.

## Design

Existing identity preserved: dark graphite, warm coral, green architectural accent, lowercase h brand. Existing docs and 101-package explorer retained with visible historical-snapshot banners. The v0.3.0 home is archived at `/archive/overview-0-3-0/`. The current source-bound overview separates implemented source, limited integration and open evaluation. No manuscript or source archive is included.

### Layout (overview page)

```
header: brand | System | Status | Sources
hero: logo stage (wordmark) + two-column introduction
facts strip: Rust, Linux-first, provider-separated, source 0.9.1
intro: editorial text + principles
system path: interactive layer explorer (Definition > Authority > Execution > Evidence)
light band: sandbox / controlled pipeline
development: filterable status ledger
evidence + source entrypoints, FAQ, archive links
```

Theme: graphite #131516, surface #1a1d1e, warm white #f1f1eb, coral #f28b66, sage #c5ceb7; later identity update adds mauve #9C586B and charcoal #474746 taken from the official SVG, on a warm ivory canvas. Body 16px minimum, mono labels 12px, 4px spacing scale. Motion: hover 180ms, pane fade 180ms, nothing hidden by scroll, reduced motion disables transitions. Mobile: linear hero, wrapping nav drawer, single-column pipeline, full-width buttons.

### Logo-driven identity

The original lion SVG and full wordmark PNG were supplied by the project owner and are used byte-for-byte (no recolor, no redraw). The hero uses the last full lion+wordmark PNG intact on a black stage; the SVG is the favicon and header/footer mark. The full PNG has an opaque near-black background; only the earlier standalone wordmark has transparency. Wordmark SHA-256 `375177e5c0e48931bc95ae0d14e487fdbee1ca0c77f9da6a0cefd62c04c62438`; SVG SHA-256 `a74078601a7c3fad9ed36046dead52ba9d771cb87007e801a7c9bccb00e0dcca`. Source of the SVG: `docs/assets/harwness-icon.svg` in the Harwness GitHub repository (blob `a366d1ef1c3ec8b141209f75ab7b03e338608b85`).

## Language and docs

- `/` is server-delivered English HTML (`lang=en`, English title, meta, labels, navigation, status controls and counter). `/de/` carries the German overview; EN/DE links keep the current anchor.
- No localStorage/sessionStorage/cookie/browser-language/Service Worker locale logic: new and returning visitors both get English at `/`; `validate_site.py` enforces this for `current.js`.
- The API docs are an existing separate site, linked only: 169 Rustdoc crates, Harwness 0.9.1, snapshot `fcb12b41` (distinct from this overview's pin). The external documentation hub has a German UI; Rustdoc navigation is English and may contain German source comments. Historical v0.3.0 docs/explorer are labeled separately.

## Content provenance

- Overview revision: upstream `14082d16b5e21f920acd2dc0b84ebd42f32ac859`, inspected 2026-10-30. The page states source-observed implementation, not independent runtime verification; source anchors are embedded with each claim.
- No paper, LaTeX archive, personal details, credentials or private evidence files are in the payload.
- Legacy home is preserved at `/archive/overview-0-3-0/`; `/docs/` and `/architecture/` are retained with prominent historical notices.
- 2026-10-30 refresh: upstream moved from former overview pin `df08fc77` by 35 commits and 134 changed paths; the ten previously linked mechanism/source files were unchanged, so their line anchors stay valid. Cargo workspace 0.10.0 (Rust 2024, MSRV 1.89) is upcoming; release v0.9.1 remains the published one, and the site distinguishes source metadata from release publication.
- Added source-confirmed claims: profile settings and incremental ten-language TUI catalogs (`docs/cli.md`, `harw-ops/src/settings.rs`, `harw-tui/src/settings_view.rs`, `harw-tui/src/app/settings_glue.rs`, `harw-config/src/harness_config.rs`, `harw-tui/src/i18n.rs`, `harw-tui/src/locales/README.md`) and corrected Responses continuation (`harw-provider-http/src/responses_reasoning.rs`, `lib.rs`). No live-provider interoperability or performance claim.
- The upstream PR record reports local test validation; the site never claims green CI or independent runtime validation. The repository research paper is a working draft and is not used as implementation evidence.

## Brand provenance

Identity colors come directly from the SVG (mauve #9C586B, charcoal #474746). The owner explicitly authorized use of the supplied wordmark, lion SVG and combined logo for the site. An earlier reference image was inspected for color/type direction only and is not a site asset. No generated or recolored logo is used.
