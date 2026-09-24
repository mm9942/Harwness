# Extending Sensors: Contract, Harness and Cookbook

> Status: partially implemented · Last reviewed: 2026-09-24

**Purpose:** make sure the fifteenth sensor is as cheap to add as the third.
**Related:** `harw-dod-crate-decomposition.md` (C1–C8),
`harw-dod-integration-and-dependencies.md`

---

## 0. What extensibility means here

Splitting into one crate per source gives extensibility on paper: a new
source is a new crate, and no existing one is touched. For that to hold in
practice, four things need to be true, and three of them fall out of the
architecture directly:

**The trait is narrow.** `Sensor` requires identity, permission, cadence,
plus `poll` or `subscribe`. Nothing more.

**Errors don't compose upward.** Every crate has its own rich error type and
only collapses to `SensorError` at the trait boundary, so a new sensor never
touches a foreign enum.

**Failure is a state.** A new sensor may fail without taking anything else
down. An experimental sensor can run in production; if it fails to bind on
some kernel, that's a metric, not an incident.

**The processing chain is inherited.** Because both event streams are
typed, a new sensor is immediately available to the rules layer, shows up
in telemetry, is addressable via selectors in triage context, and can
supply evidence through the ring buffer.

The fourth thing does not fall out automatically, and is this document's
actual subject: **fixture load.** It's what erodes extensibility over time,
not the crate count.

---

## 1. The rule: open set, closed vocabulary

**Open:** the set of sensors. A new source costs one crate and touches no
one else.

**Closed:** `Capability`, `EventKind`, `SampleScope`/`Hardness`. A source
that genuinely needs a new privilege class or a genuinely new event shape
touches the vocabulary, and then the compiler surfaces every rule, renderer
and mapping that doesn't yet handle the new variant.

If a proposed sensor needs a new `Capability` variant, that's the signal to
pause — usually the right answer is a second process, not a twelfth
permission.

**Procedure for a vocabulary change.** No silent addition. A new
`EventKind` variant goes through: an entry in this document with rationale,
a check whether an existing variant suffices, an extension of the ladder's
admission matrix, and a test proving the rules layer doesn't silently pass
it through as `Informational`.

---

## 2. The extension contract

What a new sensor crate must deliver — six points, each checkable:

1. **Exactly one source, exactly one `Capability`.** Two sources means two
   backends behind a trait (precedent: `harw-dod-authlog`) or two crates.
2. **Constructor takes a `ReadScope`.** No sensor opens a path around it —
   no direct filesystem read outside the scope mechanism.
3. **Own error type plus a `From` mapping onto `SensorError`.** The mapping
   is content-free: offsets and lengths, never parsed values.
4. **No dependency on another sensor crate** (C7), checked in CI.
5. **A fixture directory** per the convention in §3, with at least three
   kernel snapshots and one error case.
6. **An entry in the rights matrix** of the decomposition plan: what it
   reads, which process it runs in, what it explicitly may not do.

Point 6 is the one most often forgotten and the most valuable: the "may
not" line is often more informative than the "delivers" line.

---

## 3. The fixture harness

**Implemented:** `dod/crates/harw-dod-fixtures` is a dev-dependency shared
by the sensor crates, providing the `sensor_suite!` macro
(`src/macros.rs`) and the underlying assertion functions (`src/harness.rs`).
The harness currently runs **nine** checks per sensor, more than originally
scoped here: determinism, content freedom (no byte of the fixture tree
appears in a `SensorError`), scope containment, scope tightness, correct
behavior on an empty scope, redaction, label cardinality, a declared error
case, and — importantly — that an adversarial fixture "survives" as a
`Data`-trust fragment rather than being treated as an instruction. See
`dod/crates/harw-dod-fixtures/src/harness.rs` for the current, authoritative
list; §3.2 below is illustrative and may not match every macro parameter
name.

### 3.1 Directory convention

```
fixtures/
  <sensor-id>/
    <kernel>/            e.g. 6.11-fedora, 6.6-lts, 5.14-rhel9
      tree/              path-faithful excerpt of the source
        proc/stat
        sys/class/hwmon/hwmon0/temp1_input
      expect.json        expected samples/events, normalized
    malformed/
      tree/              truncated, empty, unexpected columns
      expect.json        expected SensorError, content-free
    adversarial/
      tree/              values that look like instructions
      expect.json        must end up as a Data fragment, never an instruction
```

The `tree` directory is path-faithful because the sensor's `ReadScope`
points at exactly that directory in tests — the same code path as
production, including the scope check and symlink resolution.

### 3.2 The table test

```rust
harw_dod_fixtures::sensor_suite! {
    sensor: harw_dod_thermal::Thermal,
    id: "thermal",
    // discovers every kernel snapshot automatically and runs the harness
    // checks described in §3 above against each.
}
```

### 3.3 Fixture provenance and upkeep

Excerpts should be captured, not hand-typed: `harw-dod-fixtures` provides
capture support (`src/capture.rs`, `src/capture_manifest.rs`) to copy a
sensor's declared paths from a running system into a fixture directory and
redact what needs redacting (hostnames, serial numbers, usernames). Upkeep
rule: keep three snapshots current — the latest Fedora kernel, an LTS
kernel, and the oldest supported enterprise line — and add a fourth only
when a format actually diverges.

### 3.4 Noticing format drift before it hurts

Kernels change formats. The sensor sees this as a parse error, but a single
parse error is invisible in the noise. `sensor_parse_errors_total`, per
`SensorId`, is a metric with a baseline for exactly this reason: a spike
after a system update is a finding, not silent data loss.

---

## 4. Four archetypes and their cost

| Archetype | Example | Mechanism | Approx. size | Fixtures |
|---|---|---|---|---|
| **A: single sysfs value** | thermal, gpu | glob over paths, one number per file, scaling | ~100–300 lines, hand-written (see note below) | trivial |
| **B: procfs table** | cpu, blockio, netcounters | fixed-column table, delta against the previous poll | ~100–300 lines | moderate, columns vary by version |
| **C: netlink subscription** | authlog | socket, framing, record types, field decoding | ~500–700 lines total across module files | involved |
| **D: eBPF map** | procmon, flow | load program, read map/ring buffer, shape events | Rust glue plus a BPF side | involved, needs a VM |

**Correction to the original plan:** this document originally proposed that
archetype A would be essentially free via a `#[derive(SensorSource)]` macro.
That derive does exist (`harw-macros`'s `sensor_source` module) but the
shipped sensors (`harw-dod-thermal` and siblings) deliberately do **not**
use it — their own module docs explain why: the derive has no attribute for
value scaling, no way to attach a second, per-match read (needed when a
sensor reads both a value and a companion field per matched entry), and its
glob path is not scope-relative (it always resolves from `/`, using the
`ReadScope` only as a post-hoc filter, not as the search root). Archetype A
sensors are therefore hand-written today, at roughly the sizes shown above,
not derive-generated. Revisiting the derive to close those three gaps is an
open item (see §8).

---

## 5. Worked example: a hypothetical `harw-dod-usb`

A realistic new sensor, to make the cost concrete. USB devices are a real
security signal: a newly attached mass-storage device, or a device that
identifies itself as a keyboard, is exactly the kind of event worth seeing.
No such crate exists yet; this section is illustrative of how the contract
in §2 applies to a new source.

**Contract.** Source `/sys/bus/usb/devices`, an unprivileged sysfs-read
capability, runs in `harw-sentinel`, archetype A with an event component.

**Delivers.** On poll, the current device list (vendor, product, device
class, port path). As an event, the diff against the previous poll.

**May not.** Read device contents, resolve mounts, attribute processes.

**Vocabulary question.** Does it need a new `EventKind` variant? Yes —
`DeviceAttached`/`DeviceDetached` would be new, which per §1 means: an entry
with rationale, a check whether `StructureDrift` suffices (it doesn't,
that's for workspace structure), and a test against silent
`Informational` fallthrough.

**Rule wiring without new mechanism.** An allow-list baseline over known
vendor/product pairs; a device outside the set is `Anomaly` while the
baseline is provisional, and becomes `RuleTriggered` once the set is
promoted to established.

**Total estimated cost.** Roughly a hand-written sensor of a couple hundred
lines, a fixture directory with three snapshots plus an adversarial case (a
device whose product string looks like an instruction — USB product strings
are attacker-controlled and land in triage context), two lines of
vocabulary, one rights-matrix row, one baseline definition. Without the
`Data` trust class and the two-block renderer, that adversarial case would
be an injection vector via hardware.

---

## 6. Sensors from outside

Can a plugin bring its own sensor? Yes, with a hard boundary: an externally
registered sensor cannot acquire a `Capability` the process doesn't already
have. It runs inside the sentinel, unprivileged, with a `ReadScope` assigned
by the composition root that can only shrink. A plugin sensor claiming
`FanotifyMark` isn't expressible, because the sentinel doesn't hold that
permission and `bind()`'s real probe access fails. Same rule as for agents:
plugins may bring programs, never privileges.

An external sensor must additionally meet the §2 contract, plus
registration with a declared `Capability` that the registry checks against
the process's actual permission and rejects on overreach — a registration
error, not a runtime one. **Status: Open** — no plugin sensor registration
mechanism was found in the current codebase; the sensor set today is
compiled in, not dynamically registered.

---

## 7. Checks

- **Contract check.** A CI step verifying, for every `harw-dod-*` crate
  implementing `Sensor`, the mechanically checkable parts of §2 (one
  capability, no sensor-sibling dependency, fixture directory present, no
  raw filesystem call bypassing the scope). **Status: Open** as a single
  automated gate — the individual properties are enforced piecemeal
  (`facade.rs`, the fixture harness, C7 isolation), but no one CI step
  currently walks every `harw-dod-*` crate against the full six-point list.
- **Harness completeness.** Every sensor calls `sensor_suite!`. Implemented
  for the shipped sensors listed in `harw-dod-crate-decomposition.md` §4/§5.
- **Vocabulary gate.** A `Capability`/`EventKind` change without an
  accompanying entry here should fail review; a machine-checkable variant
  count test is proposed but not confirmed present — Open.
- **Drift metric.** `sensor_parse_errors_total` — confirmed present as a
  sensor-degradation signal (see `harw-dod-crate-decomposition.md` §9).

---

## 8. Open items

1. **`SensorSource` derive gaps.** Add scaling, per-match companion reads,
   and scope-relative globbing, or retire the derive if hand-written
   sensors remain the norm.
2. **Capture tool and redaction.** Which fields get redacted on capture
   needs pinning down per archetype.
3. **Fixture size.** Full `/proc` excerpts can get large; capture only the
   sensor's declared paths.
4. **Archetype D without a VM.** eBPF sensors have no cheap fixture path;
   map contents can be synthesized, loading cannot. Proposal: split a
   testable shaping part from an untested loading part that only runs in
   the VM suite.
5. **Who decides on new `EventKind` variants.** Proposal: the same bar as a
   new warden action — a deliberate decision with a written entry, not an
   incidental part of a pull request.
