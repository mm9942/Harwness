# Web research: network policy, search backend and fetch limits

> **Status:** implemented. Describes the current implementation
> (`harw-tool-web`, `harw-egress`, `harw-config`, `harw-registry-defaults`).
> What is *not* implemented yet is listed at the end.

This page covers the tools agents use to read the web (`web.fetch`,
`web.search`, `web.docs_rs`, `web.crates_io`) and the configuration that
controls them. It is not about the local control plane `harw web`; see
[Control plane](web.md) for that.

## What is reachable, and by whom

Three layers decide whether a request goes out; the effective reach is the
**intersection** of all of them.

1. **Tool surface per role.** Only some roles carry web tools:
   `researcher-web`, `researcher`, `dependency-researcher`, `explorer`, and
   the UIA helpers `uia-explorer`, `uia-worker` and `uia-writer`.
   `researcher-web` has no workspace access at all (`fs.*`, `deps.*` are
   forbidden), so what it reads from the network cannot be combined with local
   files.
2. **Process egress policy.** One policy for all web tools, built from the
   configuration below. Every URL, every redirect target and every address a
   name resolves to is checked against it before anything is sent. Loopback,
   private (RFC 1918), CGNAT and unique-local addresses are reachable only
   with `allow_private = true`. Link-local, cloud-metadata, multicast and
   other special ranges are **never** allowed, not even with `allow_private`
   and not even when the address is listed literally.
3. **Sandbox network scope of the calling agent.** Only entry points with
   network permission (the TUI and one-shot runs) start with a network scope,
   built from the same lists as the process policy. A child never has a wider
   scope than its parent.

**Defaults.** `[research].network_allow_hosts` lists four Rust documentation
hosts, so by default research-capable agents started from the TUI or a
one-shot run can reach those, plus the host of the search backend. An empty
union of all lists means no network at all (fail-closed); the search host
alone never opens network. There is no role-specific network list today:
`researcher-web` gets the same scope as any other role that holds web tools
(its parent's scope). What sets the role apart is its tool surface (no
workspace access) and the open-web approval rule below; see "Known gaps".

## `[network]`

```toml
[network]
allow_hosts = []                 # general harness egress allowlist
allow_private = false            # loopback, RFC 1918, CGNAT, unique-local
researcher_web_hosts = ["docs.rs", "crates.io", "doc.rust-lang.org"]
research_web = "allowlist"       # or "open"
```

| Key | Default | Meaning |
|---|---|---|
| `allow_hosts` | `[]` | Plain host names (no scheme, no path) the harness may reach in general. |
| `allow_private` | `false` | Allow loopback, private (RFC 1918), CGNAT and unique-local destinations. It applies to the shared process policy and therefore to every role: an allowlisted host name that resolves to such an address becomes reachable. Link-local, cloud-metadata and multicast ranges stay refused regardless. |
| `researcher_web_hosts` | `[]` | Extra hosts. Meant for `researcher-web` only, but today merged into the same shared lists as `allow_hosts` (see below), so it widens the network of every role that has web tools. |
| `research_web` | `"allowlist"` | `"allowlist"`: only the configured hosts. `"open"`: see below. |

Entries are host names, not URLs; an entry with a scheme prefix or an empty
entry is a configuration error. An entry matches the host and its
subdomains.

The **process-wide** policy that the web tools are configured with is the
union of `[network].allow_hosts`, `[network].researcher_web_hosts`,
`[research].network_allow_hosts` and the host of the configured search
backend. The same union is the root sandbox network scope of the TUI and
one-shot entry points. It is the upper bound; a child agent still only reaches
what its parent's scope allows.

### The open research web

With `research_web = "open"`, the research roles (`researcher-web`,
`researcher`, `dependency-researcher` and agents derived from them, such as
`intel-web-researcher`) may read **any public DNS host**, read-only:

- `GET` only, no cookies, no credentials, no custom headers.
- A host reachable only through the open web must resolve to a public
  address; names that resolve to private, loopback or link-local addresses are
  refused, whatever `allow_private` says.
- Under the `ask` and `auto` approval modes, the **first request to a domain
  asks you**; the answer is remembered for the session (until the process
  ends). A domain is the host without a leading `www.`; an approval covers
  its subdomains. Under `full`, nothing asks.
- Hosts that are on a configured allowlist never ask.
- A redirect to a domain that is not yet approved aborts the fetch and names
  the domain, so the agent can request it itself. A redirect is never
  approved implicitly, because the server chooses the target, not you.
- Only `web.fetch` takes a free-form URL. `web.search`, `web.docs_rs` and
  `web.crates_io` talk to fixed hosts.

## `[web.search]`

```toml
[web.search]
provider = "brave"               # duckduckgo | brave | tavily | searxng
api_key_env = "BRAVE_API_KEY"    # name of an environment variable, never the key
max_results = 8                  # 1 to 20
# endpoint = "https://searx.example.org"   # required for searxng
```

| Provider | Needs | Notes |
|---|---|---|
| `duckduckgo` (default) | nothing | Parses DuckDuckGo's HTML result page. Convenient without a key; not an API. |
| `brave` | API key | Key is sent in a header. |
| `tavily` | API key | Authenticated `POST`. |
| `searxng` | `endpoint` | Your own instance, with JSON output enabled. |

- The key is read from the environment variable named by `api_key_env` and is
  never stored in a file, never put in a URL, a message or a log; the debug
  output of the configuration redacts it.
- The backend's host is added to the process policy automatically.
- A query is at most 400 characters. Results are not cached. A `3xx` from
  the backend is an error; redirects are not followed.

## `[research]`

```toml
[research]
network_allow_hosts = ["docs.rs", "crates.io", "doc.rust-lang.org", "static.crates.io"]
cargo_registry_read = true
max_fetch_bytes = 1048576
fetch_timeout_secs = 20
cache_ttl_secs = 3600
```

| Key | Default | Applied? |
|---|---|---|
| `network_allow_hosts` | docs.rs, crates.io, doc.rust-lang.org, static.crates.io | Yes, feeds the process policy. Must not be empty. |
| `cargo_registry_read` | `true` | Read access to the local cargo registry cache (offline crate lookups), not a web setting. |
| `max_fetch_bytes` | 1 MiB | Yes. Read limit per response. A tool call can only lower it; hard ceiling 8 MiB. |
| `cache_ttl_secs` | 3600 | Yes. Lifetime of a cached response before the next conditional request. |
| `fetch_timeout_secs` | 20 | **Not applied today.** It is parsed, validated and merged, but no web tool reads it; see "Known gaps". |

## What the fetch tool always does

These are fixed in code and not configurable:

- `https` only; at most 5 redirects, each one re-checked against the policy.
- 20 s per request, 45 s total deadline per fetch.
- HTML is reduced to visible text or Markdown (script, style and hidden
  elements never reach the model); PDFs are extracted as text; output is
  capped (64 KiB by default).
- Responses are cached under `<HARW_HOME>/cache/web/`, isolated per tenant and
  workspace scope; a cache hit counts only if its recorded redirect chain
  passes the policy check again.

## Which layer may change what

A repository-level layer (not trusted) can only **narrow** these settings:
host lists are intersected with the trusted ones, `allow_private` can only
move to `false`, `research_web` can only move towards `"allowlist"`, and the
restricted `[research]` fields use intersection, AND or minimum. `[web]` is
global; a repository cannot influence it. `cache_ttl_secs` is a profile
setting. The authoritative tables are in
[Config scopes](../design/config-scopes.md).

## Known gaps

Measured against what a research-heavy workload needs, the web stack does
not yet have:

- **No `robots.txt` handling.** Nothing fetches or honors it. The prompt of
  `intel-web-researcher` tells the agent to respect robots and terms of use,
  but that is an instruction, not enforcement.
- **No per-host rate limiting and no global cap on concurrent web requests**,
  and no handling of `429` or `Retry-After`. (Rate limiting exists for model
  providers, not for `web.*`.)
- **`research.fetch_timeout_secs` has no effect** (see above).
- **No role-specific network policy.** Code for a stricter `researcher-web`
  policy exists (`researcher_web_policy` and `researcher_web_network_scope` in
  `harw-registry-defaults`: a list taken only from `researcher_web_hosts`, with
  `allow_private` forced to `false`), but the runtime never calls it.
  `researcher_web_hosts` is only read into the shared union described above,
  so in effect it is not separate from `allow_hosts`.
- No URL de-duplication, no batch fetch, no run-level state for long
  research.

These are the subject of the planning compartment PL-92 (Web Research Fabric,
draft pull request #104); nothing in it is implemented yet.
