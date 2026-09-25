# Agent Artifact v1 — the binary format of a compiled agent

> Status: proposal (planned in #22) · Last reviewed: 2026-09-25

**Decision record:** [ADR 0001](../adr/0001-agent-compiler.md).
**Binds to:** [`agent-ir-v1.md`](agent-ir-v1.md) §6 (IR v2),
[`agent-definition-dsl.md`](agent-definition-dsl.md) §17–§18.
**Implemented by (planned):** `harw-agent-artifact` (`ArtifactBuilder::build`,
`Artifact::from_bytes` (parses and verifies), `EmbeddedArtifact::from_path` /
`from_current_exe`, `append_to_executable`).

An **agent artifact** is the frozen, self-contained form of one compiled
agent: its `AgentIr` plus every file the agent reads at runtime, protected
by BLAKE3 hashes. The compiler writes it; the runner reads it, either
appended to its own executable (§6) or, with `harw agent run`, from a
file.

Byte-level constants (magic values, limits, kind numbers) are fixed in
`harw-agent-artifact`. As everywhere in `docs/`, if the crate and this
document disagree, the code wins and the document is a bug.

---

## 1. Overview

```text
┌──────────────────────────── artifact ────────────────────────────┐
│ preamble │ header (AgentIr, canonical JSON) │ payload table │     │
│          │                                  │               │ ... │
│ payload data (concatenated, table order) │ artifact hash (32 B)  │
└──────────────────────────────────────────────────────────────────┘

┌────────────── built binary ──────────────┐
│ runner executable │ artifact │ footer    │
└──────────────────────────────────────────┘
```

All integers are unsigned little endian. All strings are UTF-8 without a
terminator. Hashes are raw 32-byte BLAKE3 digests (unkeyed, default
output length).

## 2. Layout

| Offset | Size | Field | Notes |
|---|---|---|---|
| 0 | 8 | `magic` | ASCII `HARWAGNT` |
| 8 | 2 | `format_version` | `1` for this document |
| 10 | 2 | `flags` | reserved, must be `0` in v1 (§9) |
| 12 | 4 | `header_len` | length of the header in bytes |
| 16 | `header_len` | `header` | canonical JSON of `AgentIr` (§3) |
| … | 4 | `payload_count` | number of payload table entries |
| … | variable | `payload_table` | `payload_count` entries (§4) |
| … | Σ lengths | `payload_data` | payload bytes, concatenated in table order |
| end − 32 | 32 | `artifact_hash` | BLAKE3 over every preceding byte, from `magic` to the last payload byte |

A reader rejects any `format_version` it does not know and any non-zero
`flags` bit it does not understand. There is no padding and no trailing
data after `artifact_hash`.

## 3. Header

The header is the **canonical JSON** serialization of `AgentIr` with
schema `harwness.agent-ir/v2` ([`agent-ir-v1.md`](agent-ir-v1.md) §6):
compact (no insignificant whitespace), **all object keys sorted by their
UTF-8 bytes** (struct fields included), set-valued lists sorted,
order-carrying lists in declaration order, no floating-point numbers. The
artifact reader rejects any header that is not in exactly this form. The reader deserializes it
with `deny_unknown_fields`; an unknown field or a different schema string
is an error.

The header contains the rights manifest (`Permissions`), the interface
selection (`Binary`) and the model requirements (`Models`). It never
contains a secret, only the *names* of required environment variables.

The v7 snapshot hash of the IR can be recomputed from the header alone;
`harw agent inspect` shows it next to the artifact hash.

## 4. Payload table

Each entry describes one file the agent reads at runtime:

| Size | Field | Notes |
|---|---|---|
| 1 | `kind` | payload kind, see below |
| 2 | `path_len` | length of `path` |
| `path_len` | `path` | logical path inside the artifact |
| 8 | `length` | payload length in bytes |
| 32 | `blake3` | BLAKE3 of the payload bytes |

Payload offsets are implicit: payload *n* starts where payload *n − 1*
ends, and the first starts right after the table. This leaves no room
for overlapping or out-of-range offsets.

| `kind` | Name | Typical path | Content |
|---|---|---|---|
| 1 | instructions | `instructions/system.md` | the resolved instruction text |
| 2 | skill | `skills/<name>/instructions.md` | embedded skill files (`ResolveSkills`) |
| 3 | knowledge | `knowledge/<path>` | knowledge files the agent may read |
| 4 | context program | `context/<program-id>.toml` | bound context programs |
| 5 | template | `templates/<path>` | prompt and output templates |
| `0xFF` | other | any | named extension kind: followed by a u16 name length and a name of 1–64 bytes `[a-z0-9._-]` that is not a built-in name; sorts after the built-in kinds |

Any other tag is an error in v1. Paths are relative, use `/` as the only
separator, contain no empty, `.` or `..` components, no NUL and no
backslash, and are unique within an artifact. The runner serves payloads
from memory under these logical paths; they are never written to disk
by path.

The header references payloads by path and hash (for example the source
hash of the instructions and the content hash of each skill). A reader
checks that every reference resolves to a table entry with the same
BLAKE3, and that no table entry is unreferenced.

## 5. Artifact hash

`artifact_hash` covers preamble, header, table and payload data. It is
computed last by the writer and checked first by the reader, before the
header is parsed. It is the identity of a build: `harw agent inspect` and
`./agent --manifest` print it, and two builds with the same hash are the
same agent.

## 6. Footer on an executable

Backend A appends the artifact to a copy of `harw-agent-runner`, followed
by a fixed 56-byte footer at the very end of the file:

| Offset from footer start | Size | Field |
|---|---|---|
| 0 | 8 | `artifact_offset` — file offset of the artifact's first byte |
| 8 | 8 | `artifact_len` — artifact length in bytes |
| 16 | 32 | `artifact_hash` — copy of the artifact's trailing hash |
| 48 | 8 | `footer_magic` — ASCII `HARWAEND` |

The magic comes last so that a reader can recognise the footer from the
last 8 bytes of the file.

**How the runner locates its artifact** (`EmbeddedArtifact::from_current_exe`):

1. Open the running executable (`std::env::current_exe()`; on Linux
   `/proc/self/exe`, which still points at the right file after a rename).
2. Read the last 56 bytes. If `footer_magic` is missing, there is no
   embedded artifact: the runner prints an error and exits.
3. Check `artifact_offset + artifact_len + 56 == file_len`. The artifact
   must sit directly before the footer.
4. Check `artifact_len` against the size limits (§8) before reading it.
5. Read the artifact, recompute BLAKE3 over all but its last 32 bytes, and
   compare the result with both the artifact's own `artifact_hash` and the
   footer's copy.
6. Only then parse the header and the payload table, and run the checks of
   §4 and §7.

Any failure is fatal: the runner does not start, does not fall back to a
different agent and does not continue with a partial artifact.

Backend B (`--native`) embeds the same artifact bytes with
`include_bytes!` instead of a footer and runs the same verification on
them at startup.

**Platform notes.** On Linux (the release targets x86_64 and aarch64)
appended data is ignored by the loader. On macOS, appending invalidates an
existing code signature, so the binary must be re-signed after the build.
Stripping or re-signing a built binary with tools that rewrite the file
tail destroys the footer; build again instead.

## 7. Verification

`./agent --verify`, `harw agent inspect` and every runner start perform:

1. footer checks (§6, steps 2–4), when reading from an executable;
2. magic, `format_version` and `flags`;
3. the artifact hash;
4. header parse (`deny_unknown_fields`, schema `harwness.agent-ir/v2`);
5. payload table: kinds, path rules, lengths summing exactly to the
   payload region, per-payload BLAKE3;
6. cross-references between header and payloads (§4);
7. manifest sanity: the interfaces built into this runner cover
   `Binary.interfaces`, and the default interface is one of them.

`--verify` prints the artifact hash and exits `0` on success, non-zero
with the first failed check otherwise.

## 8. Determinism and limits

**Determinism.** The same definition, the same sources and the same
compiler version produce the same artifact bytes and therefore the same
artifact hash. To guarantee that, the writer:

- serializes the header canonically (§3);
- sorts payload entries by `(kind, path)`;
- records no timestamp, host name, user name, absolute path, build
  directory or environment value;
- normalizes nothing it did not read: payload bytes are embedded as read,
  so line endings are part of the content and of its hash.

The runner binary itself is not part of the artifact hash. Two binaries
built from the same artifact on different runner versions have the same
artifact hash but different file hashes.

**Size limits** (initial values; the constants in `harw-agent-artifact`
are authoritative). Both writer and reader enforce them; the reader
checks them before it allocates.

| Limit | Value |
|---|---|
| header | 1 MiB |
| payload count | 4096 |
| single payload | 16 MiB |
| whole artifact | 64 MiB |
| path length | 512 bytes |

## 9. Future work: signatures

A hash detects modification but not authorship: whoever can rewrite the
binary can also recompute the hash. An optional signature (for example
Ed25519 over `artifact_hash`, with the public key pinned by the user) is
planned but not part of v1. No signature crate is in the lockfile today,
and adding one needs its own ADR covering key management and trust roots.
The `flags` field is reserved for it: a future version would set a flag
bit and place a signature block after `artifact_hash`, and a v1 reader
rejects such an artifact rather than silently ignoring the signature.
