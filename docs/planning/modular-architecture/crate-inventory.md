# Crate-by-crate modularity inventory

Baseline: PR #172 `6bd6b984adcc5d941561d1b55d4206321f36f2d3`. All 180 root Cargo members including 40 DoD members. Purpose and target group are architectural assessments of the source paths, selected code and existing docs, not a complete symbol audit. Rust bytes include inline tests and comments. The internal dependency column is the full direct internal Cargo.lock adjacency (all dependency kinds, targets, features combined); reverse edges use the same graph. Levels omit test-support edges, the macro crate's internal test edges, and the known rule→knowledge and compiler→registry dev edges for ordering only. Raw edges remain listed. Unresolved nodes: none. Verify with cargo metadata and real feature/target builds before moving crate boundaries. P0 = architecture critical/giant hotspot; P1 = material decomposition; P2 = useful organization; P3 = cohesive/small, no forced split. Folder proposals are not implemented unless stated separately.

## Level 0 (22 members)

| Crate | Ring | Purpose | Internal dependencies (lockfile, harw- prefix omitted) | Reverse edges | src files / KB / nested / mod.rs | Largest src file | Proposed module groups | Priority |
|---|---|---|---|---:|---|---|---|---|
| [harw-activity](../../../harw-activity/) | I | Semantic activity events and reducers | — | 2 | 6 / 142 / 1 / 0 | reducer.rs (42 KB) | event,reducer,pattern | P2 |
| [harw-agent-artifact](../../../harw-agent-artifact/) | C | Canonical agent artifacts and bundles | — | 6 | 11 / 128 / 1 / 0 | embed.rs (31 KB) | artifact,bundle,encoding | P2 |
| [harw-browser](../../../harw-browser/) | A | Browser-neutral actions, observations and policy | test-support | 3 | 16 / 214 / 0 / 0 | policy.rs (54 KB) | action,observation,policy,capability,host | P1 |
| [harw-container-model](../../../harw-container-model/) | I | Container identity and lifecycle vocabulary | — | 3 | 5 / 27 / 0 / 0 | instance.rs (9 KB) | flat model | P3 |
| [harw-context-ledger](../../../harw-context-ledger/) | I | Context-use evidence contract | test-support | 2 | 1 / 9 / 0 / 0 | lib.rs (9 KB) | flat contract | P3 |
| [harw-digest](../../../harw-digest/) | F | Content identity and hashing | — | 4 | 2 / 15 / 0 / 0 | lib.rs (13 KB) | flat | P3 |
| [harw-dod-config](../../../harw-dod/crates/harw-dod-config/) | D | Trusted observation configuration and scope validation | test-support | 4 | 2 / 69 / 0 / 0 | lib.rs (69 KB) | profile,scope,loader,validation | P1 |
| [harw-dod-dispatch](../../../harw-dod-dispatch/) | F | Pure triage admission, budgets, routing and incidents | test-support | 0 | 19 / 572 / 0 / 0 | dispatcher.rs (98 KB) | admission,routing,incident,state,queue | P1 |
| [harw-dod-nft](../../../harw-dod/crates/harw-dod-nft/) | D | Native nftables/Netlink transactions | test-support | 0 | 13 / 238 / 0 / 0 | session_tests.rs (43 KB) | wire,transport,transaction,rules | P2 |
| [harw-explorer](../../../harw-explorer/) | A | Project and filesystem exploration | — | 4 | 6 / 125 / 0 / 0 | relations.rs (42 KB) | index,relations,discovery | P2 |
| [harw-fsutil](../../../harw-fsutil/) | I | Scoped filesystem primitives and safe atomic writes | — | 22 | 6 / 113 / 0 / 0 | open.rs (59 KB) | open,atomic,walk,permissions | P1 |
| [harw-macros](../../../harw-macros/) | F | Compile-time declarations, schemas and instrumentation | authority, dod-cap, dod-readfs, dod-signals, observe, operations, retention, test-support, tools, types | 88 | 21 / 613 / 0 / 0 | operation.rs (77 KB) | operation,tool,agent,derive,instrumentation,common | P1 |
| [harw-matrix-game](../../../harw-matrix-game/) | A | Deterministic simulation and adjudication | — | 1 | 16 / 619 / 0 / 0 | phases.rs (134 KB) | game,phases,adjudication,effects,contracts,dice | P1 |
| [harw-onion](../../../harw-onion/) | A | Embedded Arti client and onion service | — | 3 | 12 / 68 / 10 / 2 | server/mod.rs (14 KB) | client,server,lifecycle | P2 |
| [harw-placement-model](../../../harw-placement-model/) | I | Executor placement vocabulary | — | 4 | 3 / 18 / 0 / 0 | offer.rs (9 KB) | flat model | P3 |
| [harw-step-list](../../../harw-step-list/) | F | Structured steps | — | 1 | 1 / 13 / 0 / 0 | lib.rs (13 KB) | flat | P3 |
| [harw-tailscale](../../../harw-tailscale/) | A | Tailscale local API and proxy | — | 1 | 5 / 33 / 0 / 0 | localapi.rs (15 KB) | client,policy | P3 |
| [harw-test-support](../../../harw-test-support/) | F | Shared test utilities | — | 106 | 2 / 7 / 0 / 0 | lib.rs (6 KB) | flat test support | P3 |
| [harw-tool-container](../../../harw-tool-container/) | A | Container execution plan and mount validation | — | 2 | 13 / 136 / 0 / 0 | plan.rs (54 KB) | model,mounts,validation | P2 |
| [harw-tool-tunnel](../../../harw-tool-tunnel/) | A | Tunnel tool and provider policy | — | 0 | 5 / 47 / 0 / 0 | policy.rs (28 KB) | provider,policy | P2 |
| [harw-tui-layout](../../../harw-tui-layout/) | F | Deterministic UI layout | — | 1 | 5 / 138 / 0 / 0 | mission.rs (99 KB) | mission,dock,placement | P2 |
| [harw-untrusted-text](../../../harw-untrusted-text/) | F | Untrusted text normalization | test-support | 12 | 1 / 26 / 0 / 0 | lib.rs (26 KB) | sanitize,unicode,limits | P2 |

## Level 1 (9 members)

| Crate | Ring | Purpose | Internal dependencies (lockfile, harw- prefix omitted) | Reverse edges | src files / KB / nested / mod.rs | Largest src file | Proposed module groups | Priority |
|---|---|---|---|---:|---|---|---|---|
| [harw-browser-thirtyfour](../../../harw-browser-thirtyfour/) | A | Concrete WebDriver adapter | browser, test-support | 1 | 26 / 199 / 0 / 0 | launcher.rs (34 KB) | driver,actions,navigation,observation,runtime,security | P1 |
| [harw-completions](../../../harw-completions/) | I | Command completion generation/installation | macros | 6 | 6 / 51 / 0 / 0 | install.rs (25 KB) | generation,installation | P3 |
| [harw-dod-encrypt](../../../harw-dod/crates/harw-dod-encrypt/) | D | CryptGuard-backed envelopes and transcripts | macros, test-support | 3 | 10 / 78 / 0 / 0 | cg.rs (14 KB) | crypto,policy,transcript | P2 |
| [harw-killer](../../../harw-killer/) | I | Controlled process termination | untrusted-text | 2 | 12 / 120 / 2 / 1 | api.rs (25 KB) | process,termination,typestate | P2 |
| [harw-lens-types](../../../harw-lens-types/) | F | Retrieval contracts | digest, macros, test-support | 21 | 6 / 42 / 0 / 0 | rank.rs (16 KB) | flat vocabulary | P3 |
| [harw-observe](../../../harw-observe/) | I | Trace, metric and sink contracts | macros, test-support | 18 | 12 / 224 / 0 / 0 | routing.rs (65 KB) | trace,metrics,routing,redaction | P1 |
| [harw-retention](../../../harw-retention/) | I | Retention classes and sweep policy | macros, test-support | 6 | 5 / 46 / 0 / 0 | classes.rs (16 KB) | flat policy | P3 |
| [harw-types](../../../harw-types/) | F | Shared identities, roles, usage, execution vocabulary | digest | 120 | 16 / 175 / 0 / 0 | provider_ids.rs (35 KB) | identity,execution,model,time | P1 |
| [harw-vpn](../../../harw-vpn/) | unassigned | VPN engine over embedded onion transport | onion | 0 | 13 / 85 / 12 / 1 | vpn/upstream_dns.rs (15 KB) | vpn/dns,vpn/engine,vpn/routing,vpn/flow | P2 |

## Level 2 (22 members)

| Crate | Ring | Purpose | Internal dependencies (lockfile, harw- prefix omitted) | Reverse edges | src files / KB / nested / mod.rs | Largest src file | Proposed module groups | Priority |
|---|---|---|---|---:|---|---|---|---|
| [harw-auth-hub](../../../harw-auth-hub/) | A | Local KMS/crypto daemon | dod-encrypt, fsutil, test-support | 0 | 18 / 300 / 5 / 1 | sealed/tests.rs (48 KB) | authentication,worker,storage,service | P2 |
| [harw-authority](../../../harw-authority/) | I | Permission, scope, policy and workspace authority | types | 49 | 4 / 102 / 0 / 0 | lib.rs (88 KB) | permission,scope,policy,workspace,snapshot | P0 |
| [harw-cloud-enroll](../../../harw-cloud/harw-cloud-enroll/) | A | Cloud enrollment contract | types | 1 | 1 / 10 / 0 / 0 | lib.rs (10 KB) | flat contract | P3 |
| [harw-code-graph](../../../harw-code-graph/) | I | Workspace dependency inspection | macros, test-support, types | 7 | 6 / 61 / 0 / 0 | workspace.rs (41 KB) | workspace,lockfile,registry | P2 |
| [harw-config](../../../harw-config/) | I | Trusted configuration discovery and restrictive merge | fsutil, macros, retention, test-support, types | 15 | 40 / 956 / 0 / 0 | merge.rs (155 KB) | discovery,schema,merge,trust,validation | P0 |
| [harw-dod-client](../../../harw-dod-client/) | A | Read-only DoD status, evidence and findings client | dod-config, types, untrusted-text | 5 | 13 / 263 / 0 / 0 | findings.rs (76 KB) | client,findings,health,correlation | P1 |
| [harw-dod-warden-types](../../../harw-dod/crates/harw-dod-warden-types/) | D | Unsigned Warden vocabulary | macros, test-support, types | 3 | 9 / 78 / 0 / 0 | action.rs (31 KB) | flat types | P3 |
| [harw-job-core](../../../harw-job-core/) | J | Portable job specification and lifecycle/fencing | macros, observe, test-support, types | 19 | 13 / 189 / 0 / 0 | spec.rs (61 KB) | identity,specification,lifecycle,governance | P1 |
| [harw-lens-chunk](../../../harw-lens-chunk/) | A | Retrieval chunking | lens-types, macros, test-support, types | 1 | 7 / 68 / 0 / 0 | rust_code.rs (21 KB) | strategies,relations | P3 |
| [harw-lens-embed](../../../harw-lens-embed/) | A | Embedding backends and model selection | lens-types, macros, test-support, types | 4 | 11 / 95 / 0 / 0 | http_backend.rs (24 KB) | backend,catalog,runtime | P2 |
| [harw-lens-rank](../../../harw-lens-rank/) | A | Retrieval scoring and fusion | lens-types, types | 3 | 7 / 52 / 0 / 0 | collapse.rs (12 KB) | scoring,fusion,packing | P3 |
| [harw-netsec](../../../harw-netsec/) | A | Network security daemon and policy state | macros, types | 1 | 14 / 146 / 0 / 0 | store.rs (41 KB) | policy,state,daemon,api | P2 |
| [harw-node-transport](../../../harw-node-transport/) | A | Authenticated node protocol and TLS | dod-encrypt, types | 5 | 14 / 207 / 0 / 0 | tests.rs (54 KB) | tls,handshake,identity,transport | P1 |
| [harw-observe-file](../../../harw-observe-file/) | I | File telemetry sink | macros, observe, test-support | 2 | 4 / 33 / 0 / 0 | sink.rs (26 KB) | flat sink | P3 |
| [harw-observe-otlp](../../../harw-observe-otlp/) | I | OTLP export and transport | macros, observe, test-support | 1 | 9 / 122 / 0 / 0 | sink.rs (36 KB) | encoding,transport,sink | P2 |
| [harw-observe-prom](../../../harw-observe-prom/) | I | Prometheus sink and endpoint | macros, observe, test-support | 1 | 6 / 61 / 0 / 0 | endpoint.rs (25 KB) | metrics,endpoint | P3 |
| [harw-plan](../../../harw-plan/) | A | Plan/goal graph, mutation and storage | macros, types | 7 | 21 / 787 / 0 / 0 | file_store.rs (118 KB) | graph,goal,mutation,storage,admission | P1 |
| [harw-protocol](../../../harw-protocol/) | F | Versioned wire and session ports | test-support, types | 23 | 14 / 291 / 0 / 0 | session_wire.rs (113 KB) | session,events,wire,media,approval | P0 |
| [harw-provider](../../../harw-provider/) | A | Model/provider contracts and registry | macros, types | 5 | 12 / 86 / 0 / 0 | slots.rs (20 KB) | model,auth,invocation,registry | P2 |
| [harw-research](../../../harw-research/) | I | Structured research contracts | macros, test-support, types | 5 | 7 / 89 / 0 / 0 | types.rs (26 KB) | finding,validation | P3 |
| [harw-secrets](../../../harw-secrets/) | I | Secret storage, envelopes and audit | fsutil, macros, observe | 5 | 18 / 405 / 6 / 1 | store.rs (105 KB) | store,envelope,keys,policy,audit | P1 |
| [harw-security-hub](../../../harw-security-hub/) | A | Security service, findings and policy | test-support, types | 0 | 10 / 149 / 0 / 0 | server.rs (45 KB) | findings,policy,service | P2 |

## Level 3 (12 members)

| Crate | Ring | Purpose | Internal dependencies (lockfile, harw- prefix omitted) | Reverse edges | src files / KB / nested / mod.rs | Largest src file | Proposed module groups | Priority |
|---|---|---|---|---:|---|---|---|---|
| [harw-context](../../../harw-context/) | I | Context fragments, budgets and rendering | authority, lens-types, macros, test-support, types | 13 | 10 / 97 / 0 / 0 | reference.rs (23 KB) | assembly,selection,budget,render | P2 |
| [harw-dod-cap](../../../harw-dod/crates/harw-dod-cap/) | D | Sensor capabilities and scoped handles | authority, macros, test-support, types | 29 | 6 / 80 / 0 / 0 | scope.rs (46 KB) | capability,scope,handle | P2 |
| [harw-dod-warden-proto](../../../harw-dod/crates/harw-dod-warden-proto/) | D | Signed Warden authorization wire | dod-warden-types, macros, test-support, types | 2 | 7 / 175 / 0 / 0 | signed.rs (77 KB) | authorization,proof,wire,canonical | P1 |
| [harw-infra-client](../../../harw-infra-client/) | I | Authenticated crypto/key/network service adapters | secrets, untrusted-text | 5 | 16 / 233 / 0 / 0 | posture.rs (58 KB) | client,auth,crypto,posture | P1 |
| [harw-job-darwin](../../../harw-job-darwin/) | J | Darwin job platform adapter | job-core | 1 | 13 / 99 / 4 / 1 | macos/process.rs (24 KB) | process,sandbox,resources,recovery | P2 |
| [harw-job-linux](../../../harw-job-linux/) | J | Linux job, process, cgroup and sandbox | job-core | 5 | 13 / 280 / 0 / 0 | sandbox.rs (85 KB) | process,cgroup,sandbox,recovery | P1 |
| [harw-job-store](../../../harw-job-store/) | J | Durable fenced job records | job-core, types | 5 | 9 / 134 / 0 / 0 | tests.rs (30 KB) | record,locking,fencing,storage | P2 |
| [harw-media](../../../harw-media/) | I | Digest-bound media validation and storage | digest, fsutil, protocol, test-support | 9 | 10 / 57 / 0 / 0 | sanitize.rs (18 KB) | store,inspection,processing | P2 |
| [harw-oauth](../../../harw-oauth/) | A | OAuth flows, credentials and refresh | config, fsutil, test-support | 2 | 9 / 132 / 0 / 0 | codex_refresh.rs (49 KB) | flows,credentials,providers | P2 |
| [harw-sandbox](../../../harw-sandbox/) | I | Sandbox projection and host permits | authority, test-support, types | 31 | 10 / 342 / 0 / 0 | host_permit_session.rs (124 KB) | filesystem,network,process,permits,backends | P0 |
| [harw-session-client](../../../harw-session-client/) | A | Transport-neutral session client | protocol, test-support, types | 2 | 9 / 39 / 0 / 0 | tally.rs (11 KB) | connection,submission,events | P3 |
| [xtask](../../../xtask/) | A | Architecture/security/build gates | code-graph, test-support | 0 | 10 / 530 / 0 / 0 | gate_operator_decision.rs (129 KB) | gates/arch,gates/security,gates/modularity | P1 |

## Level 4 (14 members)

| Crate | Ring | Purpose | Internal dependencies (lockfile, harw- prefix omitted) | Reverse edges | src files / KB / nested / mod.rs | Largest src file | Proposed module groups | Priority |
|---|---|---|---|---:|---|---|---|---|
| [harw-agent-dsl](../../../harw-agent-dsl/) | C | Agent syntax, resolution and executable IR | context | 14 | 22 / 769 / 0 / 0 | executable.rs (132 KB) | syntax,resolution,authority,ir,lifecycle | P0 |
| [harw-dod-bpf](../../../harw-dod/crates/harw-dod-bpf/) | D | eBPF ABI, loader and scope | dod-cap, macros, test-support, types | 4 | 14 / 197 / 0 / 0 | real.rs (40 KB) | abi,loader,events,scope | P2 |
| [harw-dod-netlink](../../../harw-dod/crates/harw-dod-netlink/) | D | Netlink transport and records | dod-cap, macros | 2 | 7 / 93 / 0 / 0 | record.rs (48 KB) | wire,socket,records | P2 |
| [harw-dod-netpolicy](../../../harw-dod/crates/harw-dod-netpolicy/) | D | Network policy planning and backend | authority, dod-cap, macros, sandbox, test-support | 0 | 6 / 41 / 0 / 0 | plan.rs (11 KB) | model,planning,backend | P3 |
| [harw-dod-readfs](../../../harw-dod/crates/harw-dod-readfs/) | D | Scoped sensor filesystem reads | dod-cap, macros, test-support | 14 | 5 / 69 / 0 / 0 | read.rs (33 KB) | scope,reader,glob | P2 |
| [harw-dod-signals](../../../harw-dod/crates/harw-dod-signals/) | D | Sensor samples, evidence and verdict types | dod-cap, macros, observe, test-support, types | 31 | 9 / 106 / 0 / 0 | verdict.rs (31 KB) | observation,event,evidence,verdict | P1 |
| [harw-dod-warden](../../../harw-dod/crates/harw-dod-warden/) | D | Privileged authorization/enforcement library | dod-warden-proto, macros, test-support, types | 1 | 19 / 572 / 0 / 0 | warden.rs (191 KB) | authorization,enforcement,containment,ledger,recovery | P0 |
| [harw-egress](../../../harw-egress/) | I | External network admission and proxy | authority, sandbox, test-support | 4 | 9 / 185 / 1 / 0 | proxy.rs (57 KB) | policy,classification,proxy,client | P1 |
| [harw-job-exec](../../../harw-job-exec/) | J | Job launch/trampoline and outcome | job-core, job-linux, test-support | 1 | 9 / 115 / 0 / 0 | plan.rs (35 KB) | launch,trampoline,outcome | P2 |
| [harw-job-executor-bwrap](../../../harw-job-executor-bwrap/) | J | Bubblewrap backend | authority, job-core, job-linux, sandbox, test-support, types | 2 | 5 / 108 / 1 / 0 | executor.rs (52 KB) | planning,launch,enforcement | P2 |
| [harw-job-tokio](../../../harw-job-tokio/) | J | Async process supervision | job-core, job-linux | 1 | 4 / 49 / 0 / 0 | supervisor.rs (29 KB) | process,supervision | P3 |
| [harw-model-catalog](../../../harw-model-catalog/) | A | Model capabilities and provider metadata | config, context, lens-types, types | 6 | 25 / 599 / 0 / 0 | vendor_qwen.rs (55 KB) | model,vendors,sources,resolution | P1 |
| [harw-session-store](../../../harw-session-store/) | I | Durable sessions, approvals, leases, retention | fsutil, job-core, job-store, macros, observe, retention, types, untrusted-text | 18 | 17 / 786 / 0 / 0 | dod_verdict.rs (231 KB) | transcript,approval,leases,security,retention | P0 |
| [harw-tools](../../../harw-tools/) | I | Tool specification and invocation contracts | activity, authority, context, lens-types, macros, sandbox, test-support, types | 34 | 14 / 182 / 0 / 0 | provider_macro.rs (50 KB) | definition,execution,schema,context | P1 |

## Level 5 (14 members)

| Crate | Ring | Purpose | Internal dependencies (lockfile, harw- prefix omitted) | Reverse edges | src files / KB / nested / mod.rs | Largest src file | Proposed module groups | Priority |
|---|---|---|---|---:|---|---|---|---|
| [harw-channel](../../../harw-channel/) | A | Channel pairing and dispatch | macros, secrets, session-store, test-support, types | 3 | 9 / 72 / 0 / 0 | pairing_store.rs (34 KB) | pairing,dispatch,events | P2 |
| [harw-dod-advisory](../../../harw-dod/crates/harw-dod-advisory/) | D | Advisory matching sensor | dod-cap, dod-readfs, dod-signals, macros, test-support, types, untrusted-text | 1 | 11 / 184 / 0 / 0 | sensor.rs (85 KB) | sources,matching,severity,sensor | P1 |
| [harw-dod-container](../../../harw-dod/crates/harw-dod-container/) | D | Container observation sensor | container-model, dod-cap, dod-readfs, dod-signals, test-support, types | 1 | 3 / 23 / 0 / 0 | sensor.rs (20 KB) | small sensor | P3 |
| [harw-dod-fixtures](../../../harw-dod/crates/harw-dod-fixtures/) | D | Reproducible security fixtures | dod-cap, dod-readfs, dod-signals, macros, test-support, types | 12 | 8 / 159 / 0 / 0 | harness.rs (73 KB) | capture,manifest,harness | P2 |
| [harw-dod-flow](../../../harw-dod/crates/harw-dod-flow/) | D | Flow-event conversion and observation | authority, dod-bpf, dod-cap, dod-signals, macros, sandbox, test-support, types | 2 | 6 / 88 / 0 / 0 | sensor.rs (27 KB) | conversion,observation,report | P2 |
| [harw-dod-procmon](../../../harw-dod/crates/harw-dod-procmon/) | D | Process event sensor | dod-bpf, dod-cap, dod-signals, macros, test-support, types, untrusted-text | 2 | 6 / 87 / 0 / 0 | sensor.rs (30 KB) | event,process,sensor | P2 |
| [harw-dod-rules](../../../harw-dod/crates/harw-dod-rules/) | D | Deterministic rules and finding typestates | authority, code-graph, dod-signals, knowledge, research, test-support, types, untrusted-text | 7 | 18 / 447 / 9 / 1 | finding.rs (83 KB) | finding,engine,baseline,rules,advisory | P1 |
| [harw-dod-submit-proto](../../../harw-dod/crates/harw-dod-submit-proto/) | D | Triage submission wire | dod-signals, macros, test-support, types | 1 | 2 / 14 / 0 / 0 | lib.rs (14 KB) | flat contract | P3 |
| [harw-home](../../../harw-home/) | I | Profile/home layout and bundle trust | agent-dsl, config, fsutil | 20 | 10 / 239 / 0 / 0 | bundle.rs (61 KB) | layout,profile,bundle,import,trust | P1 |
| [harw-job-runtime](../../../harw-job-runtime/) | J | Job coordination and backend selection | job-core, job-darwin, job-exec, job-executor-bwrap, job-linux, job-store, job-tokio, placement-model, provider, types | 9 | 18 / 526 / 12 / 1 | coordinator/linux.rs (177 KB) | coordinator,admission,supervision,recovery,backends | P0 |
| [harw-mcp-server](../../../harw-mcp-server/) | A | MCP server transport and authority | job-core, session-store, test-support, types | 1 | 7 / 194 / 0 / 0 | transport.rs (98 KB) | protocol,transport,auth,session | P1 |
| [harw-reverse-proxy](../../../harw-reverse-proxy/) | A | Proxy routing and websocket bridge | egress, netsec | 0 | 7 / 105 / 0 / 0 | ws.rs (35 KB) | routing,http,websocket | P2 |
| [harw-session-host](../../../harw-session-host/) | A | Authoritative hosted session control plane | authority, fsutil, job-core, media, protocol, sandbox, session-store, tools, types | 12 | 39 / 953 / 14 / 0 | host.rs (236 KB) | host,connection,admission,turn,owner | P0 |
| [harw-warden](../../../harw-dod/crates/harw-warden/) | D | Privileged Warden process composition | completions, dod-warden, dod-warden-proto, macros, test-support, types | 0 | 15 / 291 / 0 / 0 | isolation.rs (66 KB) | startup,ipc,sandbox,enforcement | P1 |

## Level 6 (24 members)

| Crate | Ring | Purpose | Internal dependencies (lockfile, harw- prefix omitted) | Reverse edges | src files / KB / nested / mod.rs | Largest src file | Proposed module groups | Priority |
|---|---|---|---|---:|---|---|---|---|
| [harw-catalog](../../../harw-catalog/) | I | Skill catalog, selection and triggers | config, context, home, lens-types, test-support, types | 8 | 5 / 179 / 0 / 0 | lib.rs (53 KB) | skills,selection,triggers,index | P1 |
| [harw-channel-telegram](../../../harw-channel-telegram/) | A | Telegram conversation, approval and work adapter | authority, channel, sandbox, session-store, test-support, types | 2 | 10 / 218 / 0 / 0 | telegram.rs (88 KB) | conversation,approval,attachments,work | P1 |
| [harw-cloud-gateway](../../../harw-cloud/harw-cloud-gateway/) | A | Cloud ingress, owner identity and placement | fsutil, infra-client, node-transport, onion, protocol, session-host | 4 | 21 / 242 / 1 / 0 | owner_ingress.rs (72 KB) | ingress,owner,placement,identity,directory | P0 |
| [harw-dod-authlog](../../../harw-dod/crates/harw-dod-authlog/) | D | Authentication event observation | dod-cap, dod-fixtures, dod-netlink, dod-signals, macros, test-support, types | 2 | 8 / 94 / 0 / 0 | audit_backend.rs (33 KB) | backends,records,sensor | P2 |
| [harw-dod-blockio](../../../harw-dod/crates/harw-dod-blockio/) | D | Block I/O sensor | dod-cap, dod-fixtures, dod-readfs, dod-signals, macros, test-support, types | 2 | 3 / 53 / 0 / 0 | sensor.rs (38 KB) | small sensor | P3 |
| [harw-dod-cgroup](../../../harw-dod/crates/harw-dod-cgroup/) | D | Cgroup sensor | dod-cap, dod-fixtures, dod-readfs, dod-signals, macros, test-support, types | 1 | 3 / 55 / 0 / 0 | sensor.rs (40 KB) | small sensor | P3 |
| [harw-dod-cpu](../../../harw-dod/crates/harw-dod-cpu/) | D | CPU sensor | dod-cap, dod-fixtures, dod-readfs, dod-signals, macros, test-support, types | 2 | 3 / 25 / 0 / 0 | sensor.rs (17 KB) | small sensor | P3 |
| [harw-dod-fsmon](../../../harw-dod/crates/harw-dod-fsmon/) | D | Filesystem event observation | dod-cap, dod-fixtures, dod-readfs, dod-signals, macros, test-support, types | 2 | 11 / 102 / 0 / 0 | sensor.rs (24 KB) | events,filter,coalesce,sensor | P2 |
| [harw-dod-gpu](../../../harw-dod/crates/harw-dod-gpu/) | D | GPU sensor | dod-cap, dod-fixtures, dod-readfs, dod-signals, macros, test-support, types | 2 | 3 / 50 / 0 / 0 | sensor.rs (37 KB) | small sensor | P3 |
| [harw-dod-ladder](../../../harw-dod/crates/harw-dod-ladder/) | D | Deterministic escalation ladder | dod-rules, dod-signals, dod-warden-types, macros, test-support, types | 2 | 2 / 18 / 0 / 0 | lib.rs (18 KB) | flat decision | P3 |
| [harw-dod-listener](../../../harw-dod/crates/harw-dod-listener/) | D | Listening sockets and owner observation | dod-cap, dod-fixtures, dod-readfs, dod-signals, macros, test-support, types | 2 | 5 / 76 / 0 / 0 | procnet.rs (27 KB) | source,ownership,sensor | P2 |
| [harw-dod-memory](../../../harw-dod/crates/harw-dod-memory/) | D | Memory sensor | dod-cap, dod-fixtures, dod-readfs, dod-signals, macros, test-support, types | 2 | 3 / 31 / 0 / 0 | sensor.rs (22 KB) | small sensor | P3 |
| [harw-dod-netcounters](../../../harw-dod/crates/harw-dod-netcounters/) | D | Network-counter sensor | dod-cap, dod-fixtures, dod-readfs, dod-signals, macros, test-support, types | 2 | 3 / 48 / 0 / 0 | sensor.rs (36 KB) | small sensor | P3 |
| [harw-dod-scanreport](../../../harw-dod/crates/harw-dod-scanreport/) | D | Scan reports and SARIF | dod-cap, dod-fixtures, dod-readfs, dod-signals, home, macros, test-support, types | 2 | 8 / 94 / 0 / 0 | sensor.rs (41 KB) | formats,redaction,sensor | P2 |
| [harw-dod-sentinel](../../../harw-dod/crates/harw-dod-sentinel/) | D | Observation aggregation, health and spool | authority, code-graph, dod-cap, dod-rules, dod-signals, fsutil, macros, observe, sandbox, test-support, types | 2 | 8 / 217 / 0 / 0 | spool.rs (84 KB) | collection,health,findings,spool | P1 |
| [harw-dod-thermal](../../../harw-dod/crates/harw-dod-thermal/) | D | Thermal sensor | dod-cap, dod-fixtures, dod-readfs, dod-signals, macros, test-support, types | 2 | 3 / 33 / 0 / 0 | sensor.rs (23 KB) | small sensor | P3 |
| [harw-dod-workspace](../../../harw-dod/crates/harw-dod-workspace/) | D | Workspace inventory and drift | code-graph, dod-cap, dod-fixtures, dod-signals, macros, test-support, types | 2 | 5 / 102 / 0 / 0 | inventory.rs (60 KB) | inventory,baseline,sensor | P2 |
| [harw-install](../../../harw-install/) | A | Install, deployment and service control | config, dod-client, home, test-support | 2 | 18 / 360 / 1 / 0 | doctor.rs (57 KB) | installation,deployment,services,doctor | P1 |
| [harw-job-executor-k8s](../../../harw-job-executor-k8s/) | A | Kubernetes job backend | container-model, job-core, job-runtime, placement-model, test-support, types | 0 | 9 / 87 / 0 / 0 | executor.rs (22 KB) | transport,workload,executor | P2 |
| [harw-job-executor-oci](../../../harw-job-executor-oci/) | J | OCI job backend and pool | container-model, job-core, job-runtime, placement-model, test-support, types | 1 | 12 / 112 / 0 / 0 | executor.rs (21 KB) | engine,pool,lifecycle,executor | P2 |
| [harw-knowledge](../../../harw-knowledge/) | A | Durable knowledge and epistemic workflows | agent-dsl, context, dod-rules, dod-signals, job-core, lens-types, macros, model-catalog, observe, plan, session-store, types | 10 | 27 / 664 / 9 / 2 | context_steward.rs (88 KB) | artifacts,diary,dream,memory,workbench,context | P1 |
| [harw-lens-store](../../../harw-lens-store/) | A | Retrieval persistence | home, lens-types, macros, test-support, types | 3 | 4 / 48 / 0 / 0 | store.rs (37 KB) | small storage | P3 |
| [harw-probe-bpf](../../../harw-dod/crates/harw-probe-bpf/) | D | Privileged BPF probe process | authority, completions, dod-bpf, dod-cap, dod-config, dod-flow, dod-procmon, dod-signals, sandbox, test-support, types | 0 | 13 / 422 / 0 / 0 | main.rs (103 KB) | startup,collection,transport,sandbox | P1 |
| [harw-session-ws](../../../harw-session-ws/) | A | WebSocket protocol adapter | media, protocol, session-host, session-store, types | 5 | 7 / 142 / 1 / 0 | dispatch.rs (65 KB) | connection,codec,dispatch,limits | P2 |

## Level 7 (13 members)

| Crate | Ring | Purpose | Internal dependencies (lockfile, harw- prefix omitted) | Reverse edges | src files / KB / nested / mod.rs | Largest src file | Proposed module groups | Priority |
|---|---|---|---|---:|---|---|---|---|
| [harw-agent-compiler](../../../harw-agent-compiler/) | C | Agent compilation and artifacts | agent-artifact, agent-dsl, catalog, home, registry-defaults | 4 | 35 / 430 / 12 / 2 | commands.rs (56 KB) | frontend,passes,backend,artifacts,diagnostics | P1 |
| [harw-channel-telegram-transport](../../../harw-channel-telegram-transport/) | A | Telegram ingress, delivery and rendering | channel, channel-telegram, config, secrets, test-support, types | 1 | 13 / 229 / 0 / 0 | render.rs (49 KB) | ingress,delivery,render,media,dedup | P1 |
| [harw-cloudctl](../../../harw-cloud/harw-cloudctl/) | A | Cloud administration entrypoint | cloud-gateway, install | 0 | 1 / 11 / 0 / 0 | main.rs (11 KB) | flat entry | P3 |
| [harw-dod](../../../harw-dod/crates/harw-dod/) | D | DoD facade | authority, dod-authlog, dod-blockio, dod-bpf, dod-cap, dod-cpu, dod-flow, dod-fsmon, dod-gpu, dod-listener, dod-memory, dod-netcounters, dod-procmon, dod-rules, dod-scanreport, dod-sentinel, dod-signals, dod-thermal, dod-workspace, sandbox, test-support, types | 0 | 2 / 25 / 0 / 0 | lib.rs (25 KB) | flat facade | P3 |
| [harw-dod-escalate](../../../harw-dod/crates/harw-dod-escalate/) | D | Escalation and authorization transitions | authority, dod-ladder, dod-rules, dod-signals, dod-submit-proto, dod-warden-types, macros, sandbox, session-store, test-support, types | 0 | 5 / 89 / 0 / 0 | freeze_ops.rs (39 KB) | decision,authorization,state | P1 |
| [harw-extension-api](../../../harw-extension-api/) | I | Extension contracts and registry | authority, catalog, context, lens-types, macros, sandbox, test-support, tools, types | 35 | 13 / 236 / 0 / 0 | registry.rs (45 KB) | registry,capabilities,contributors,approval | P1 |
| [harw-job](../../../harw-job/) | J | Job facade | job-core, job-executor-oci, job-runtime, job-store, types | 2 | 1 / 15 / 0 / 0 | lib.rs (15 KB) | flat facade | P3 |
| [harw-lens-index](../../../harw-lens-index/) | A | Lexical/vector retrieval index | lens-rank, lens-store, lens-types, macros, test-support, types | 3 | 8 / 88 / 0 / 0 | flat.rs (31 KB) | lexical,vector,query | P2 |
| [harw-node-listener](../../../harw-node-listener/) | A | Authenticated listener and revocation | fsutil, node-transport, protocol, session-host, session-ws, types | 3 | 3 / 61 / 0 / 0 | identity.rs (29 KB) | identity,listener,revocation | P2 |
| [harw-probe-fs](../../../harw-dod/crates/harw-probe-fs/) | D | Privileged filesystem probe | completions, dod-cap, dod-config, dod-fsmon, dod-signals, test-support, types | 0 | 10 / 177 / 0 / 0 | source.rs (68 KB) | source,watch,sandbox,transport | P1 |
| [harw-probe-netlink](../../../harw-dod/crates/harw-probe-netlink/) | D | Privileged Netlink collection probe | completions, dod-authlog, dod-cap, dod-netlink, dod-signals, test-support, types | 0 | 8 / 106 / 0 / 0 | collect.rs (59 KB) | collection,transport,sandbox | P2 |
| [harw-sentinel](../../../harw-dod/crates/harw-sentinel/) | D | Production DoD aggregation process | authority, completions, dod-advisory, dod-blockio, dod-cap, dod-cgroup, dod-client, dod-config, dod-container, dod-cpu, dod-gpu, dod-listener, dod-memory, dod-netcounters, dod-rules, dod-scanreport, dod-sentinel, dod-signals, dod-thermal, dod-workspace, home, macros, observe, observe-file, retention, sandbox, test-support, types, untrusted-text | 0 | 18 / 926 / 0 / 0 | main.rs (216 KB) | startup,polling,ingress,export,health | P0 |
| [harw-session-remote](../../../harw-session-remote/) | A | Remote session ports and reconnection | fsutil, node-transport, protocol, session-host, session-ws, types | 5 | 6 / 105 / 0 / 0 | conn.rs (36 KB) | connection,reconnect,port | P2 |

## Level 8 (18 members)

| Crate | Ring | Purpose | Internal dependencies (lockfile, harw- prefix omitted) | Reverse edges | src files / KB / nested / mod.rs | Largest src file | Proposed module groups | Priority |
|---|---|---|---|---:|---|---|---|---|
| [harw-command](../../../harw-command/) | J | Governed command execution port | job, job-core, job-executor-bwrap, job-runtime, sandbox, test-support, types | 9 | 4 / 111 / 1 / 0 | port.rs (89 KB) | port,request,execution,result | P1 |
| [harw-instructions](../../../harw-instructions/) | A | Trusted instruction assembly | extension-api, macros, test-support, types | 2 | 5 / 67 / 0 / 0 | baseline.rs (53 KB) | baseline,workflow,trust | P2 |
| [harw-lens-source](../../../harw-lens-source/) | A | Knowledge/document retrieval sources | home, knowledge, lens-chunk, lens-embed, lens-index, lens-store, lens-types, macros, observe, types | 3 | 7 / 130 / 0 / 0 | build.rs (45 KB) | source,document,indexing | P2 |
| [harw-mcp-client](../../../harw-mcp-client/) | A | External MCP transport and adapter | extension-api, tools | 2 | 4 / 85 / 0 / 0 | tool_bridge.rs (36 KB) | transport,discovery,tool_adapter | P2 |
| [harw-memory](../../../harw-memory/) | A | Facts, consolidation, feedback and learning | context, extension-api, fsutil, lens-types, test-support, types | 5 | 29 / 754 / 0 / 0 | facts.rs (96 KB) | facts,capture,consolidation,feedback,context,storage | P1 |
| [harw-operations](../../../harw-operations/) | I | Operation metadata, registry and dispatch | authority, extension-api, macros, sandbox, tools, types | 11 | 14 / 437 / 4 / 1 | operation.rs (110 KB) | operation,registry,dispatch,adapters | P1 |
| [harw-project-discovery](../../../harw-project-discovery/) | A | Project discovery/context contribution | extension-api, macros, test-support, types | 2 | 4 / 63 / 0 / 0 | discovery.rs (53 KB) | discovery,provider | P2 |
| [harw-session-com](../../../harw-session-com/) | A | Session/node transport composition | node-listener, node-transport, protocol, session-host, session-remote, session-ws, types | 1 | 13 / 61 / 0 / 0 | remote.rs (13 KB) | local,remote,handshake,binding | P2 |
| [harw-tool-browser](../../../harw-tool-browser/) | A | Governed browser adapter | authority, browser, extension-api, sandbox, test-support, tools | 1 | 7 / 92 / 0 / 0 | harness_provider.rs (31 KB) | dispatch,provider,validation | P2 |
| [harw-tool-deps](../../../harw-tool-deps/) | A | Dependency research adapter | authority, code-graph, extension-api, macros, sandbox, tools, types | 1 | 6 / 113 / 0 / 0 | source_tool.rs (53 KB) | graph,sources,provider | P2 |
| [harw-tool-doc](../../../harw-tool-doc/) | A | Document-processing adapter | authority, egress, extension-api, fsutil, macros, tools | 3 | 8 / 107 / 0 / 0 | mistral.rs (35 KB) | backends,processing,tool | P2 |
| [harw-tool-explorer](../../../harw-tool-explorer/) | A | Exploration tool adapter | authority, explorer, extension-api, macros, sandbox, tools, types | 1 | 2 / 32 / 0 / 0 | provider.rs (31 KB) | small adapter | P3 |
| [harw-tool-fs](../../../harw-tool-fs/) | A | Governed filesystem tools | authority, extension-api, fsutil, home, macros, media, protocol, sandbox, tools, types | 2 | 15 / 334 / 0 / 0 | read.rs (62 KB) | read,write,search,media,provider | P1 |
| [harw-tool-fsread](../../../harw-tool-fsread/) | A | Read-only filesystem tools | authority, digest, extension-api, fsutil, macros, test-support, tools, types | 5 | 25 / 301 / 0 / 0 | ls.rs (28 KB) | metadata,content,traversal,analysis,scope | P1 |
| [harw-tool-obsidian](../../../harw-tool-obsidian/) | A | Vault adapter | authority, extension-api, knowledge, macros, tools, types | 1 | 3 / 38 / 0 / 0 | lib.rs (26 KB) | vault,provider | P3 |
| [harw-tool-plan](../../../harw-tool-plan/) | A | Planning and user interaction tools | authority, extension-api, fsutil, home, tools, types | 4 | 11 / 130 / 0 / 0 | provider.rs (37 KB) | planning,interaction,session,admission | P2 |
| [harw-tool-process](../../../harw-tool-process/) | A | Controlled process adapter | authority, extension-api, killer, macros, tools, types | 1 | 2 / 30 / 0 / 0 | provider.rs (30 KB) | small adapter | P3 |
| [harw-tool-remote](../../../harw-tool-remote/) | A | Remote tool invocation adapter | authority, extension-api, macros, protocol, test-support, tools, types | 1 | 6 / 41 / 0 / 0 | tests.rs (22 KB) | executor,provider | P3 |

## Level 9 (11 members)

| Crate | Ring | Purpose | Internal dependencies (lockfile, harw- prefix omitted) | Reverse edges | src files / KB / nested / mod.rs | Largest src file | Proposed module groups | Priority |
|---|---|---|---|---:|---|---|---|---|
| [harw-cloud-ops](../../../harw-cloud/harw-cloud-ops/) | A | Cloud management operations | authority, cloud-gateway, home, macros, node-listener, operations, tools, types | 2 | 5 / 35 / 0 / 0 | enrollments.rs (17 KB) | enrollment,status,administration | P2 |
| [harw-core](../../../harw-core/) | A | Agent, turn and child lifecycle | agent-dsl, authority, catalog, config, context, context-ledger, extension-api, instructions, job-core, lens-types, macros, media, observe, protocol, sandbox, session-store, tools, types, untrusted-text | 13 | 51 / 3075 / 7 / 0 | turn_loop.rs (714 KB) | session,turn_loop,children,context,model,admission | P0 |
| [harw-lens-query](../../../harw-lens-query/) | A | Retrieval query execution | home, knowledge, lens-embed, lens-index, lens-rank, lens-source, lens-store, lens-types, macros, test-support, types | 3 | 6 / 47 / 0 / 0 | query.rs (25 KB) | resolution,selection,execution | P2 |
| [harw-session-daemon](../../../harw-session-daemon/) | A | Session daemon composition | protocol, session-com, session-host, session-ws, types | 4 | 3 / 17 / 0 / 0 | server.rs (13 KB) | composition,server | P3 |
| [harw-tool-cargo](../../../harw-tool-cargo/) | A | Governed Cargo commands | authority, extension-api, macros, test-support, tool-fsread, tools, types | 1 | 9 / 190 / 0 / 0 | tools.rs (78 KB) | commands,planning,execution,reporting | P1 |
| [harw-tool-container-run](../../../harw-tool-container-run/) | A | Container execution adapter | authority, command, extension-api, job, tool-container, tools, types | 1 | 4 / 54 / 0 / 0 | tools.rs (25 KB) | execution,inspection | P2 |
| [harw-tool-gitread](../../../harw-tool-gitread/) | A | Read-only Git object/history inspection | authority, extension-api, macros, test-support, tool-fsread, tools, types | 1 | 24 / 323 / 0 / 0 | tools.rs (28 KB) | objects,refs,history,index,diff,tools | P1 |
| [harw-tool-shell](../../../harw-tool-shell/) | A | Governed shell execution and permits | authority, command, extension-api, macros, observe, sandbox, test-support, tools, types | 5 | 19 / 779 / 8 / 0 | exec.rs (250 KB) | execution,authorization,capture,provider,limits | P0 |
| [harw-tool-sys](../../../harw-tool-sys/) | A | Read-only system observation tools | authority, extension-api, macros, test-support, tool-fsread, tools, types, untrusted-text | 1 | 16 / 257 / 0 / 0 | ps.rs (40 KB) | process,network,environment,system | P2 |
| [harw-tool-web](../../../harw-tool-web/) | A | Web and dependency research | authority, egress, extension-api, fsutil, macros, sandbox, tool-doc, tools | 1 | 12 / 297 / 0 / 0 | fetch.rs (97 KB) | fetch,search,cache,sources | P1 |
| [harw-web](../../../harw-web/) | A | Web service and session integration | authority, context, infra-client, macros, operations, session-store, test-support, types | 2 | 12 / 295 / 0 / 0 | server.rs (77 KB) | server,routing,identity,authorization,events | P1 |

## Level 10 (10 members)

| Crate | Ring | Purpose | Internal dependencies (lockfile, harw- prefix omitted) | Reverse edges | src files / KB / nested / mod.rs | Largest src file | Proposed module groups | Priority |
|---|---|---|---|---:|---|---|---|---|
| [harw-cloud](../../../harw-cloud/) | A | Cloud facade | cloud-enroll, cloud-gateway, cloud-ops | 0 | 1 / 0 / 0 / 0 | lib.rs (0 KB) | flat facade | P3 |
| [harw-core-bridge](../../../harw-core-bridge/) | A | Agent tool/orchestration adapters | agent-dsl, authority, catalog, core, dod-signals, extension-api, macros, observe, operations, research, session-store, test-support, types, untrusted-text | 2 | 12 / 612 / 1 / 0 | agent_tool.rs (304 KB) | delegation,return_contracts,fanout,communication | P0 |
| [harw-lens](../../../harw-lens/) | A | Retrieval facade | lens-embed, lens-query, lens-source, lens-types, macros, test-support | 2 | 5 / 42 / 0 / 0 | lib.rs (22 KB) | flat facade | P3 |
| [harw-lens-federation](../../../harw-lens-federation/) | A | Federated retrieval sources | knowledge, lens-embed, lens-index, lens-query, lens-rank, lens-source, lens-types, macros, test-support, types | 1 | 4 / 28 / 0 / 0 | federated.rs (20 KB) | sources,aggregation,query | P2 |
| [harw-llama](../../../harw-llama/) | A | Local LLM engine/provider | core, protocol, tools, types | 0 | 9 / 117 / 1 / 0 | provider.rs (56 KB) | engine,inference,provider,stream | P2 |
| [harw-mobile-core](../../../harw-mobile-core/) | A | Mobile session controller/projection | protocol, session-client, session-daemon, session-host, session-remote, test-support, types | 1 | 3 / 46 / 0 / 0 | lib.rs (25 KB) | controller,state,projection | P2 |
| [harw-plan-bridge](../../../harw-plan-bridge/) | A | Plan cycles, jobs, verification and evidence | agent-dsl, authority, context, core, dod-ladder, dod-rules, dod-signals, extension-api, home, job-core, job-runtime, job-store, knowledge, lens-types, macros, observe, operations, plan, research, sandbox, session-store, test-support, tool-fsread, types | 4 | 29 / 1329 / 0 / 0 | cycle_runtime.rs (156 KB) | cycles,execution,verification,context,persistence | P0 |
| [harw-provider-http](../../../harw-provider-http/) | A | HTTP providers, credentials, replay and pacing | config, core, fsutil, media, oauth, protocol, provider, sandbox, test-support, tools, types | 4 | 28 / 1146 / 0 / 0 | lib.rs (385 KB) | providers,auth,transport,replay,pacing,routing | P0 |
| [harw-session-driver](../../../harw-session-driver/) | A | Hosted session to Core bridge | core, extension-api, media, protocol, session-host, session-store, types | 2 | 2 / 78 / 0 / 0 | bridge.rs (75 KB) | driver,session,events | P1 |
| [harw-tool-job](../../../harw-tool-job/) | A | Job tools and legacy process manager | authority, command, extension-api, job-core, job-linux, macros, test-support, tool-shell, tools, types | 5 | 16 / 808 / 3 / 0 | manager.rs (196 KB) | management,execution,persistence,supervision,tools | P0 |

## Level 11 (2 members)

| Crate | Ring | Purpose | Internal dependencies (lockfile, harw- prefix omitted) | Reverse edges | src files / KB / nested / mod.rs | Largest src file | Proposed module groups | Priority |
|---|---|---|---|---:|---|---|---|---|
| [harw-mobile-term](../../../harw-mobile-term/) | A | Mobile terminal projection | mobile-core, protocol, session-daemon, session-host, session-remote, test-support, types | 0 | 7 / 32 / 0 / 0 | render.rs (13 KB) | session,commands,render | P3 |
| [harw-tool-lens](../../../harw-tool-lens/) | A | Governed retrieval tool | authority, extension-api, home, lens, lens-federation, lens-query, macros, sandbox, test-support, tools, types | 2 | 6 / 73 / 0 / 0 | ask_tool.rs (26 KB) | query,provenance,provider | P2 |

## Level 12 (1 members)

| Crate | Ring | Purpose | Internal dependencies (lockfile, harw- prefix omitted) | Reverse edges | src files / KB / nested / mod.rs | Largest src file | Proposed module groups | Priority |
|---|---|---|---|---:|---|---|---|---|
| [harw-registry-defaults](../../../harw-registry-defaults/) | A | Default role/tool/profile registration | agent-compiler, agent-dsl, authority, browser, browser-thirtyfour, catalog, command, config, egress, extension-api, home, instructions, knowledge, memory, project-discovery, sandbox, test-support, tool-browser, tool-cargo, tool-deps, tool-doc, tool-explorer, tool-fs, tool-fsread, tool-gitread, tool-job, tool-lens, tool-obsidian, tool-plan, tool-process, tool-shell, tool-sys, tool-web, tools, types | 8 | 22 / 1409 / 0 / 0 | profile.rs (356 KB) | profiles,roles,providers,definitions,assembly | P0 |

## Level 13 (1 members)

| Crate | Ring | Purpose | Internal dependencies (lockfile, harw- prefix omitted) | Reverse edges | src files / KB / nested / mod.rs | Largest src file | Proposed module groups | Priority |
|---|---|---|---|---:|---|---|---|---|
| [harw-ops](../../../harw-ops/) | A | Application operations and adapters | agent-compiler, agent-dsl, authority, catalog, code-graph, command, config, core, core-bridge, dod-client, explorer, extension-api, home, infra-client, job-runtime, knowledge, macros, matrix-game, memory, model-catalog, operations, plan, plan-bridge, protocol, provider, provider-http, registry-defaults, research, retention, sandbox, session-store, tool-job, tool-plan, tool-shell, tools, types, untrusted-text, web | 5 | 80 / 3670 / 8 / 1 | dod.rs (286 KB) | agent,provider,knowledge,jobs,security,planning,extensions | P0 |

## Level 14 (2 members)

| Crate | Ring | Purpose | Internal dependencies (lockfile, harw- prefix omitted) | Reverse edges | src files / KB / nested / mod.rs | Largest src file | Proposed module groups | Priority |
|---|---|---|---|---:|---|---|---|---|
| [harw](../../../harw/) | A | Public facade | agent-dsl, authority, code-graph, core, extension-api, model-catalog, operations, ops, plan, plan-bridge, provider, registry-defaults, research, sandbox, test-support, types | 0 | 3 / 8 / 0 / 0 | prelude.rs (4 KB) | flat facade | P3 |
| [harw-runtime](../../../harw-runtime/) | A | Shared runtime/session composition | agent-artifact, agent-dsl, authority, catalog, cloud-ops, command, config, context, context-ledger, core, core-bridge, dod-client, explorer, extension-api, fsutil, home, infra-client, job-core, job-runtime, knowledge, lens-types, macros, mcp-client, media, memory, model-catalog, observe, operations, ops, placement-model, plan, plan-bridge, project-discovery, protocol, provider-http, registry-defaults, sandbox, secrets, session-store, tool-container, tool-container-run, tool-fs, tool-job, tool-plan, tool-remote, tool-shell, tools, types | 5 | 43 / 1989 / 3 / 0 | assembly.rs (515 KB) | assembly,sessions,children,authority,models,services | P0 |

## Level 15 (3 members)

| Crate | Ring | Purpose | Internal dependencies (lockfile, harw- prefix omitted) | Reverse edges | src files / KB / nested / mod.rs | Largest src file | Proposed module groups | Priority |
|---|---|---|---|---:|---|---|---|---|
| [harw-local-host](../../../harw-local-host/) | A | Local session host composition | authority, config, core, extension-api, operations, ops, protocol, registry-defaults, runtime, session-driver, session-host, session-store, tools, types | 1 | 3 / 75 / 0 / 0 | lib.rs (47 KB) | composition,owner,projects,storage | P1 |
| [harw-tui](../../../harw-tui/) | A | Terminal UI composition/projection | activity, agent-artifact, agent-dsl, authority, catalog, command, config, context, core, explorer, extension-api, fsutil, home, lens-types, macros, media, memory, model-catalog, operations, ops, plan, protocol, provider, registry-defaults, runtime, sandbox, session-store, step-list, test-support, tool-job, tool-plan, tool-shell, tools, tui-layout, types | 2 | 123 / 4861 / 42 / 1 | app.rs (850 KB) | app,conversation,interaction,turns,approvals,monitoring,sessions,rendering | P0 |
| [harwness-sdk](../../../harwness-sdk/) | A | Public embeddable SDK | agent-artifact, agent-dsl, config, core, extension-api, home, media, protocol, provider-http, runtime, session-client, session-daemon, session-host, session-remote, session-store, tools, types | 1 | 12 / 177 / 0 / 0 | remote.rs (37 KB) | client,session,events,tools,remote | P2 |

## Level 16 (2 members)

| Crate | Ring | Purpose | Internal dependencies (lockfile, harw- prefix omitted) | Reverse edges | src files / KB / nested / mod.rs | Largest src file | Proposed module groups | Priority |
|---|---|---|---|---:|---|---|---|---|
| [harw-agent-runner](../../../harw-agent-runner/) | A | Compiled agent execution/interfaces | agent-artifact, agent-compiler, agent-dsl, command, core, home, job-runtime, registry-defaults, runtime, tool-job, tui, types, harwness-sdk | 0 | 19 / 397 / 8 / 1 | iface/mcp.rs (64 KB) | artifact,execution,backend,interface | P1 |
| [harw-cli](../../../harw-cli/) | A | CLI commands, bootstrap, workers and gateway | agent-artifact, agent-compiler, agent-dsl, authority, channel, channel-telegram, channel-telegram-transport, cloud-gateway, command, completions, config, context, core, dod-client, dod-encrypt, extension-api, fsutil, home, infra-client, install, job-runtime, job-store, killer, knowledge, lens, lens-types, local-host, macros, mcp-client, mcp-server, memory, model-catalog, node-listener, node-transport, oauth, observe, observe-file, observe-otlp, observe-prom, onion, operations, ops, plan, plan-bridge, protocol, provider-http, registry-defaults, retention, runtime, sandbox, secrets, session-daemon, session-driver, session-host, session-remote, session-store, session-ws, tailscale, tool-doc, tool-lens, tools, tui, types, web | 0 | 128 / 3855 / 50 / 2 | job_worker.rs (340 KB) | bootstrap,commands,workers,gateway,diagnostics | P0 |

