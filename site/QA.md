# Site QA (2026-10-30)


- HTML parsed successfully for all retained and new pages; one H1 per page, unique IDs, local route/asset/anchor references checked.
- `node --check` passed for current.js and preserved site.js.
- Desktop Chromium preview visually inspected (1180 px browser viewport); no horizontal overflow.
- Responsive iframe viewports of 390 and 320 px inspected. After explicit long-word wrapping fixes, small viewport clientWidth=scrollWidth=305 px (15 px scrollbar); no horizontal page overflow.
- Interactive tabs: pointer selection and arrow-key selection verified; aria-selected/panel visibility update correctly.
- Status filter: Open shows two rows and announces "2 Bereiche"; All restored.
- Mobile menu: Enter opens, navigation selection closes; mobile evidence tab works. Pointer automation inside the narrow iframe did not reliably toggle the menu, so keyboard behavior and resulting visible menu were explicitly verified. Real-device touch testing was not performed.
- Preserved historical crate explorer still fetches 101 records and search "harw-core" shows 2 / 101 matches.
- Preview console showed extension metadata errors only, no site-script errors observed.
- No PDF, LaTeX archive, credentials, private evidence notes, personal contact details or temporary QA harness in deployment payload.
- Archived home, docs and explorer remain available; all historical routes show prominent snapshot warnings.
- Runtime itself was not built/tested; this QA covers the website only.

## Logo-driven redesign QA
- Actual first PNG, last full-logo PNG, and official original SVG inspected visually.
- Original full logo copied byte-for-byte; SHA-256 375177e5c0e48931bc95ae0d14e487fdbee1ca0c77f9da6a0cefd62c04c62438.
- Original SVG copied byte-for-byte; SHA-256 a74078601a7c3fad9ed36046dead52ba9d771cb87007e801a7c9bccb00e0dcca.
- New main-page composition: large complete wordmark on black, split introduction below, mauve/ivory technical sections; all non-brand original text retained (automated comparison).
- Header/footer use the last original combined image intact; favicon uses official SVG.
- Desktop and responsive 390/320 px screenshots inspected. Both mobile documents retain equal clientWidth and scrollWidth (375 and 305 px respectively including scrollbar allocation).
- Tabs and status filtering retested; JavaScript syntax checks pass.
- Temporary QA harness removed before packaging. Audience unchanged: owner-private.


## Current dev refresh — 2026-10-30
- Source review pinned to upstream 14082d16; former linked mechanism files unchanged by 35-commit/134-path comparison. New settings/localization/provider claims reviewed in source, not just planning documents.
- `python validate_site.py`: PASS for five HTML pages, unique IDs, one H1 each, all local assets/routes/anchors, root English and /de/ German, ten status entries, separate current/API pins, original wordmark SHA256. No language storage/detection overrides.
- `node --check dist/current.js` and `node --check dist/site.js`: PASS.
- Desktop development update visually reviewed in managed preview at 1165 px viewport. New panel, typography and source links clear.
- Responsive iframe inspection at 390 px English and 320 px German: no horizontal overflow (375/375, 305/305 client/scroll widths with scrollbars). Visually reviewed new content in both languages.
- Functional browser checks: Implemented filter gives 4 areas; German Open filter gives 2 Bereiche. EN→DE and DE→EN language links work and preserve #stand. Existing tabs/archives remain intact.
- Fixed inherited missing whitespace around German mobile headline line break without altering copy.
- Temporary QA fixture removed before package. No Rust-runtime tests, privileged host execution, or live provider calls performed. Maintainer-reported upstream validation is explicitly attributed, not presented as our execution.
