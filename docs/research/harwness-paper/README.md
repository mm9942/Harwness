# Harwness paper: publication package

**Harwness: Policy-First Composition and Durable Execution for Model-Heterogeneous Agents**  
Author: **Mia**  
Version: **1.0, prepared 30 October 2026**  
Type: **architecture and implementation report / technical preprint**  
Reviewed software: **Harwness 0.10.0**, commit `14082d16b5e21f920acd2dc0b84ebd42f32ac859`  
Status: **not peer reviewed; not submitted by this preparation workflow**

## Files

- `harwness-paper.pdf`: complete typeset manuscript.
- `../harwness-paper.md`: readable Markdown edition of the same manuscript.
- `source/`: editable LaTeX, BibTeX, compiled bibliography, vector figure, and Makefile.
- `evidence/source-map.json`: 47 selected source paths, immutable URLs, Git blob identities and SHA-256 digests.
- `evidence/claim-evidence.json`: checked implementation claims, code anchors and explicit qualifications.
- `evidence/review-record.json`: pinned artifact, revision comparison, and attributed test provenance.
- `evaluation-template.json`: a deliberately unpopulated experimental record. Nulls are not measured results.
- `submission.md` and `submission-metadata.json`: prepared descriptive metadata and publication options.
- `REVISION-NOTES.md`: substantive changes from the supplied manuscript.
- `LICENSE-MIT`, `LICENSE-APACHE`: the manuscript's existing alternative licenses.

## Build the PDF

Use a conventional TeX Live installation with pdfLaTeX, BibTeX and latexmk. The source uses standard packages, including Latin Modern, TikZ, natbib, booktabs, tabularx, longtable, xurl, hyperref and needspace. No shell escape or network access is required during the build.

```bash
cd source
make
```

Without latexmk:

```bash
cd source
pdflatex -interaction=nonstopmode -halt-on-error harwness-paper.tex
bibtex harwness-paper
pdflatex -interaction=nonstopmode -halt-on-error harwness-paper.tex
pdflatex -interaction=nonstopmode -halt-on-error harwness-paper.tex
```

`source/references.bib` is the complete canonical bibliography. `references-external.bib` retains the separately checked scholarly and platform references for convenient maintenance. The PDF uses the standard numeric `unsrt` style; URLs are also included in notes so that the standard style prints them. The supplied `.bbl` allows submission systems to use the compiled bibliography. To refresh the optional Markdown edition, run `python3 source/export_markdown.py --output ../harwness-paper.md --link-prefix harwness-paper/` from this directory, with Pandoc and PyYAML installed. `harwness-paper.tex` is the top-level source file.

The source package was built with pdfTeX from TeX Live 2023. This verifies that the manuscript can be typeset; it does not verify a Harwness runtime build. The paper does not claim that different TeX distributions produce bit-identical PDFs.

## Reproduce the source identity

These commands are instructions for a reader with Git/network access; they were not a successful local clone during paper preparation. Source inspection used pinned GitHub file and metadata responses.

```bash
git clone https://github.com/mm9942/Harwness.git
cd Harwness
git checkout --detach 14082d16b5e21f920acd2dc0b84ebd42f32ac859
git rev-parse HEAD
git rev-parse 'HEAD^{tree}'
git hash-object Cargo.toml
```

Expected tree: `5948efdd459252f70b165e31c4bb6b3f779d94b5`. The source map contains expected blob identities for the selected files. Runtime build/test instructions belong to the pinned repository's `CONTRIBUTING.md`; they are separate from building this paper.

## What the evidence establishes

This is a selected-path static analysis of an implemented system. The previous manuscript baseline was `df08fc77baacacd085e6f5c6a7824f2aa391dedf`; the reviewed development head is 35 commits ahead. Core authority/compiler/cycle/sandbox/Warden paths were checked against current source and the intervening changes.

PR #161 reports 18,523 passing default-feature workspace tests, zero failures and 202 ignored tests, including doctests, on commit `5ebeffc87bac7e3bac4fdaac1ee81de2fca08c91`. GitHub commit metadata confirms that its source tree equals the reviewed merge tree. This relationship is independently checked metadata; the tests themselves were **not rerun for this paper**. No new task benchmark, security certification, privileged activation or production deployment is claimed.

## Authorship and license

The byline remains **Mia**, as supplied in the manuscript. No surname, affiliation, ORCID or contact details have been inferred. ChatGPT (OpenAI) assisted with repository inspection, literature checking, manuscript revision and document preparation. The package does not assert completed final human approval.

The manuscript and preparation package are licensed under **MIT OR Apache-2.0**, at the recipient's option. This retains the explicit license in the supplied draft. Third-party works retain their own terms. Publication platforms may require a separate distribution grant; the prepared metadata does not silently change the manuscript's license.
