# uia-mia

A minimal user-interface agent. Building it with `--native` produces a complete,
personalized `harw` (Harw flavor) instead of a slim per-agent runner.

```sh
harw agent check ./examples/agents/uia-mia
harw agent build ./examples/agents/uia-mia --native --harw-src <checkout> -o ./mia
```

`scripts/native-e2e-uia.sh` builds and exercises it end to end.
