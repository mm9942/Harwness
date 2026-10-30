# Revision notes: publication preparation

## Artifact and evidence

- Updated the reviewed artifact from `df08fc77` / workspace 0.9.1 to the observed `dev` head `14082d16` / workspace 0.10.0.
- Compared the 35 intervening commits and retained immutable source links.
- Added a machine-readable map of source identities and checked claims.
- Replaced stale validation emphasis with PR #161's reported results and verified that its tested commit and the reviewed merge share tree `5948efdd459252f70b165e31c4bb6b3f779d94b5`.
- Preserved the distinction between source inspection, attributed validation, limited integration and prospective experiments. Runtime tests were not independently executed.

## Technical corrections

- Restricted the attenuation equation to ordinary delegation under the same resource interpretation. Trusted read-view rebinding is treated separately; the role ceiling includes the explicit WorkDriver exception while retaining the author ceiling.
- Corrected the per-job write-scope description: explicit directory grants are possible; broader incidental grants that would leave the entire workspace writable are rejected. A nonexistent target requires an explicitly granted existing ancestor directory.
- Qualified live lease-liveness protection to the integration that installs `LeaseLivenessGuard`; constructing `CycleStore` alone supplies epoch fencing.
- Kept the live adaptive-cycle read-only restriction, candidate-only exact-file pilot, backend-specific isolation limits and incomplete Warden authorization integration explicit.
- Recast unverified remote-interface maturity assertions as untested paths; retained the concrete optional gateway placeholder and reported all-features limitation.

## Publication and presentation

- Replaced drafting instructions with finished authorship, assistance, availability and licensing prose.
- Retained the supplied byline and explicit dual-license choice; added a first-page license notice.
- Checked all 19 external references; pinned arXiv versions, aligned ReAct's year with the cited version, restored author diacritics and linked the primary MIT/USENIX sources.
- Produced editable LaTeX/BibTeX sources, a vector architecture figure, machine-readable evidence and an unpopulated evaluation template.
- Checked the rendered pages, citation resolution, source links, tables, equations, fonts and page flow.

The manuscript remains a technical preprint with no new empirical benchmark claims. No external publication, DOI registration, peer-review acceptance or final human approval is asserted by these changes.
