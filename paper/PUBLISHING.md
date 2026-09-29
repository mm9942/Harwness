# Harw Paper Publishing Plan

## Goal

Publish one canonical, reproducible Harw research artifact without allowing GitHub, arXiv and Hugging Face copies to drift.

## Publication topology

```
GitHub
  source of truth for:
  - paper source
  - experiment code
  - evidence ledger
  - reproducibility manifests
  - tagged release
        │
        ▼
arXiv / canonical preprint
  source of truth for:
  - citable paper version
  - stable arXiv identifier
        │
        ▼
Hugging Face
  research discovery + artifacts:
  - README / research card
  - PDF mirror if desired
  - experiment summaries
  - optional dataset repo for traces/results
  - links to GitHub + arXiv
        │
        ▼
HF Paper Page
  linked through arXiv ID
```

## GitHub plan

Use this repository for the living paper until there is a reason to split it.

Before first preprint create a release/tag:

```
paper-v0.1
paper-v0.1-baseline
```

Recommended release assets:

- `harw-paper-v0.1.pdf`
- `harw-paper-v0.1-source.tar.gz`
- `harw-paper-v0.1-experiments.tar.gz` or links to a dataset repository
- checksums
- `artifact-manifest.json`

Do not package unrelated repository files into the paper source archive.

## arXiv plan

Use arXiv as the canonical preprint identifier once:

- the abstract and claims are stable;
- all primary experiments are frozen;
- the evidence ledger is complete;
- author metadata is final;
- the source bundle builds cleanly in a clean environment;
- source hygiene has been checked for secrets, comments and unrelated files.

After submission, record:

- arXiv ID;
- version number;
- submission date;
- exact Git tag/commit represented by that version.

Each later paper revision should map one-to-one to a Git tag.

## Hugging Face plan

The connected account observed during planning is:

```
@lizsophie
```

The currently observed OAuth credential exposes read-oriented Hub access plus Jobs, but no repository write scope. Direct publication therefore requires a write-capable Hugging Face credential before the publish step.

Recommended repository name:

```
lizsophie/harw-agent-runtime-paper
```

A standard Hub repository can hold the research card and lightweight artifacts. If large experimental traces or structured benchmark results are released, create a separate dataset repository such as:

```
lizsophie/harw-agent-runtime-evals
```

Suggested paper repository contents:

```
README.md
paper/
  harw-paper.pdf
  citation.bib
artifacts/
  artifact-manifest.json
  checksums.txt
results/
  summary.json
  tables/
```

The Hugging Face README should contain:

- paper title;
- concise abstract;
- authors;
- current arXiv link;
- GitHub link;
- Harw baseline commit;
- reproduction instructions;
- artifact hashes;
- limitations;
- citation block.

Once the README links to the arXiv paper, Hugging Face can associate the repository with the paper's Paper Page through its arXiv identifier.

## Automation plan

Add an explicit publication script only after the repository layout stabilizes.

Conceptual command:

```
./scripts/publish-paper   --tag paper-v0.1   --arxiv-id <id>   --hf-repo lizsophie/harw-agent-runtime-paper
```

It should:

1. require a clean Git tree;
2. require the expected paper baseline tag;
3. rebuild the PDF;
4. run source hygiene/secret checks;
5. verify experiment manifests and artifact hashes;
6. build a minimal source archive;
7. generate/update the HF README and citation metadata;
8. refuse to publish if the arXiv/Git/HF version tuple is inconsistent;
9. upload only explicitly allowlisted artifacts;
10. print the resulting immutable version manifest.

No model should be allowed to decide on its own that a new scientific version is ready for publication.

## CI gates before publication

Minimum gates:

- paper source compiles from a clean checkout;
- bibliography resolves;
- no broken figure/table references;
- no untracked paper dependencies;
- no secrets;
- no absolute local paths;
- every primary result has a manifest;
- every primary manifest points at the frozen baseline;
- hashes of published artifacts are deterministic or explicitly explained;
- evidence ledger has no unresolved primary claim marked as measured;
- generated HF card points to the intended arXiv version.

## Version mapping

Maintain:

| Paper version | Git tag | Harw baseline | arXiv version | HF revision |
|---|---|---|---|---|
| v0.1 | TBD | TBD | TBD | TBD |

## Release sequence

1. Complete experiments.
2. Freeze Harw baseline.
3. Freeze paper source.
4. Build and review PDF.
5. Create GitHub paper release.
6. Submit canonical preprint.
7. Record arXiv ID.
8. Generate Hugging Face research card from the frozen metadata.
9. Publish HF artifact repository using a write-capable credential.
10. Verify the Hugging Face Paper Page association.
11. Add reciprocal links back into GitHub documentation.

## Current implementation status

As of this planning document:

- GitHub paper workstream: **created in a dedicated branch**.
- Paper manuscript: **outline/abstract only**.
- Experiments: **not yet run for publication**.
- arXiv submission: **not started**.
- Hugging Face account: **authenticated as @lizsophie**.
- Hugging Face direct write from the currently observed OAuth scope: **not available**.
- Hugging Face publication automation: **planned, not implemented**.
