# hello-analyst

The smallest useful example for `harw agent build`: a read-only worker
that lists and reads files and reports what it found. See
[`docs/guides/agent-compiler.md`](../../../docs/guides/agent-compiler.md)
for the full picture; this file is just the commands to run against this
one definition.

## Check it

```sh
harw agent check ./examples/agents/hello-analyst
```

Runs parsing, resolution, lowering and the compiler passes and prints
diagnostics, if any, with a stable code and `file:line`. A clean
definition prints nothing and exits `0`.

## Build it

```sh
harw agent build ./examples/agents/hello-analyst --interface cli,mcp -o ./hello-analyst
```

Produces `./hello-analyst` (plus the versioned copy under
`~/.harw/bin/.versions/hello-analyst/…`, with `~/.harw/bin/hello-analyst`
pointing at it). Needs `harw-agent-runner` installed next to `harw`
(`make install` does this) or found under
`~/.harw/bin/.runners/<target>/<version>/`.

## Inspect it

```sh
harw agent inspect ./hello-analyst
```

Prints the rights manifest, the artifact and snapshot hashes, the built-in
interfaces and the embedded skills, after verifying the binary's integrity.

## Run it

```sh
export ANTHROPIC_API_KEY=…
./hello-analyst "What is in this folder?"
```

`./hello-analyst --manifest` shows the baked-in rights without running
anything; `./hello-analyst --verify` checks the embedded artifact's
hashes. Actually answering a task needs the runner's interfaces, which are
still landing (#22 wave 3) — see the status note at the top of the agent
compiler guide.

## Test it

```sh
harw agent test ./examples/agents/hello-analyst
```

Runs [`tests/basic.toml`](tests/basic.toml): the manifest checks
(`tools`/`not_tools`) run now; the answer checks (`contains`/
`not_contains`) are reported as `pending` until the runner exists.
