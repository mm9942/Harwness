# Wave manifests

One manifest per merged fix wave, written by
[`kit/wave_manifest.py`](../kit/wave_manifest.py) and committed on the wave's
branch on top of its content commit. It records:

- `base_sha`: the commit the wave's branch was cut from;
- `branch` and `head_sha`: the branch and its content commit;
- `files`: the exact set of files the wave was allowed to change;
- `findings`: stable IDs (`F-` + SHA-1 of `file:line:title`, 12 characters)
  with file, line, severity, pattern, and title;
- `workflow_runs`: the run IDs of all workflows that wrote or reviewed;
- `disposition`: what the workflow returned (`complete`, open files,
  ripple status and ripple IDs, cluster status);
- `central_build`: the commands the central build must run over the final
  integration state.

Manifests are never modified. If a wave is re-cut, it gets a new name. The
manifests of the waves `wa-egress`, `wa-authz`, `contract-a`, `contract-b`,
and `wa-web` were added after the merge (field `note`); those of their ripple
findings are with the follow-up waves.
