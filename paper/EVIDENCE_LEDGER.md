# Harw Paper Evidence Ledger

This file prevents architecture intent from becoming accidental scientific evidence.

Initial baseline:

```
repository: mm9942/Harwness
ref: main
commit: 442cf92e3e79f0a6fd31e650a55e8be676c47f1e
```

Statuses:

- **CURRENT-CODE** — implemented in the baseline.
- **CURRENT-TESTED** — implemented with relevant automated tests/gates.
- **MEASURED** — evaluated by a paper experiment.
- **DESIGNED** — merged design/contract only.
- **FUTURE** — proposed.
- **REJECTED** — claim was too broad or unsupported.

| Claim ID | Candidate claim | Status at initial baseline | Required paper evidence |
|---|---|---|---|
| C01 | Harw embeds trusted built-in agent definitions/context material into the binary and resolves them through typed agent definitions/IR. | CURRENT-CODE | source anchors + build/runtime characterization |
| C02 | Harw can bound knowledge retrieval by artifact count, graph hops and visibility rather than injecting unbounded history. | CURRENT-CODE | source anchors + E1 |
| C03 | Harw has distinct durable knowledge surfaces including Diary and Palace, with controlled promotion/recording paths. | CURRENT-CODE | source anchors + persistence tests + E1/E3 |
| C04 | Tool execution can be routed through an explicit gateway with capability and placement constraints. | CURRENT-CODE | source anchors + E2 |
| C05 | Delegated child-agent authority narrows rather than silently widening. | CURRENT-CODE/CURRENT-TESTED candidate | exact tests/gates + E2 |
| C06 | Session state supports replay/recovery and idempotent client message handling. | CURRENT-CODE/CURRENT-TESTED candidate | exact tests + E3 |
| C07 | Harw can improve long-horizon engineering reliability relative to a simpler harness under a fixed model. | UNPROVEN | E4 |
| C08 | Harw reduces context cost without materially reducing task quality. | UNPROVEN | E1 |
| C09 | Runtime authority enforcement prevents forbidden side effects more reliably than prompt-only restrictions. | UNPROVEN | E2 |
| C10 | Harw's persistence mechanisms recover correctly across process/client interruption. | UNPROVEN | E3 |
| C11 | Core runtime properties transfer across model/provider families. | UNPROVEN | E5 |
| C12 | The Model-Turn-Chain improves reasoning quality or compute efficiency. | DESIGNED | implementation + dedicated experiment; otherwise Future Work |
| C13 | Harw can generate training data that improves future models. | FUTURE | not a claim for v1 paper |

## Rules

1. A source-code link establishes implementation, not usefulness.
2. A test establishes behavior under its tested conditions, not broad empirical superiority.
3. A design document establishes intent only.
4. A benchmark result must pin the model, model settings, code commit, benchmark version and date.
5. Any claim using words such as *better*, *safer*, *faster*, *more reliable*, *efficient*, or *robust* requires a defined comparator and measured result.
6. Negative results remain in the experiment record.
7. If a mechanism changes after the paper freeze, either rerun the affected experiment or keep the paper pinned to the old commit.

## Evidence manifest template

Each experiment should emit a machine-readable manifest containing at least:

```yaml
experiment_id:
paper_claims: []
harw_commit:
harw_version:
date:
host:
os:
arch:
model:
provider:
model_settings:
dataset_or_tasks:
random_seed:
commands: []
artifact_hashes: {}
metrics: {}
notes:
```

## Freeze policy

Before first public preprint:

- choose a paper baseline commit;
- tag it, e.g. `paper-v0.1-baseline`;
- rerun every primary experiment on that exact baseline;
- freeze experiment outputs by hash;
- build the PDF only from the frozen paper source;
- verify that every empirical statement maps to the ledger.
