# Macro roadmap (wave 1 scan)

Source: four read-only scans (macro inventory, three redundancy scans). Counts are
grep-based and approximate; line savings are estimates. Verify before acting.

## How the existing macros work (short)

- `harw-macros` proc-macro crate: `HarwError` (Display/Error/From/source + `XResult`),
  `Tool`/`#[tool]` (schema + executor), `#[operation]` + `OpArgs` + `FromRawArgs`
  (one declaration feeds operator command, model tool, web route), `HarwId`,
  `KebabEnum` (`ALL`, `as_str`, Display, FromStr; kebab or snake accepted on parse),
  `field!`, `metrics!`, `Redact`, context/instructions providers, DoD sensor macros.
- `macro_rules!`: `tool_provider!` (provider struct + duplicate-name const check),
  six ID-newtype generators, `row!`/`provider!` catalog, test helpers.
- Expansion is textual (`::harw_tools::…`), no production dependency from the macro crate.

## Findings

- Operator-command vs agent-tool duplication is already solved by `#[operation]`
  (~123 sites). The gap is crates that never adopted the macros.
- Unused or near-unused: `agent`, `traced`, `warden_actions!`, `operations!`,
  `try_operations!`, macro_rules `field!` (0 real callers); `KebabEnum` 1 site,
  `HarwId` 1 crate. Candidates for adoption or removal.
- Duplicates: six ID-newtype generators (`newtype_id!`, `arc_str_id!`, `string_id!`,
  `netsec_id!`, `id_newtype!`, `uuid_id!`) plus `HarwId`; `code!` vs `build_code!`;
  `field!` as proc-macro and macro_rules; five `assert_*_blank*` test macros.

## Ranked work (do in this order)

| # | Item | Kind | Est. lines | Risk |
|---|------|------|-----------:|------|
| 1 | Shared `TestError`/`TestResult`/`ctx` (`define_test_error!`) replacing ~36 copies; plus shared temp-dir helper (17 copies) | test-only crate/macro | 1,500+ | none |
| 2 | Migrate hand-written `Display`/`Error`/`From` enums (~40) to `HarwError` | migration; first check struct variants (`#[source]` on named fields) | 450–700 | low |
| 3 | Extend `tool_provider!`/`#[tool]` with `state =` and external schema; migrate ~26 manual providers | extension + migration | 1,500–2,500 | medium |
| 4 | Hoist 7 copied schema helpers into `harw-tools::schema`; then `schema!` | helper first | 600–900 | low |
| 5 | Shared `parse_args` + `fail!` for tool argument errors | function | 350–500 | low |
| 6 | `KebabEnum` snake_case option (or `WireEnum`); adopt for ~25 enums; promote `display_via_as_str!` | extension | ~300 | low |
| 7 | Consolidate ID generators onto `HarwId` (+ `validate = path`); replace `netsec_id!` | extension | ~140 | low |
| 8 | `limits_struct!` for ~20 `impl Default` structs; shared `default_true` (8 copies) | macro/fn | ~210 | low |
| 9 | Capability catalog parity test vs provider `TOOL_NAMES`; `register_ops!` | test/macro | 100+ | low |
| 10 | Remove or adopt zero-caller macros | cleanup | – | low |

## Constraints

- Panic-free tests; macros must not weaken `missing_docs` (forward `#[doc]`).
- Keep serde `rename_all` and `as_str` in sync with a paired test.
- Orchestrator-only shared registration files per W00 §11; central gates on a frozen SHA.
- Not yet scanned: harw-tui-layout, harw-lens-*, harw-completions, harw-model-catalog,
  xtask, harw-observe-otlp/prom, clap dispatch.
