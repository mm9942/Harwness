# HARW Tunnel Policy — v1

> Status: planned contract · Scope: managed SSH tunnels in `harw-tool-tunnel`.

This document defines the security and policy boundary for the first version of the HARW tunnel tool. The contract is intentionally narrower than general SSH forwarding: v1 creates only local forwards, binds them only to loopback, and permits remote destinations only through an explicit target allowlist.

## 1. Purpose and scope

`harw-tool-tunnel` manages SSH port forwards within the HARW job/WorkDriver lifecycle. v1 supports only local SSH forwards (`-L`): a local loopback port is forwarded over an SSH connection to a target host and target port on the remote side.

The tool must integrate policy checks, approval, secret redaction, and controlled reconnection into the managed lifecycle. This document sets the binding limits; it does not describe any concrete CLI or configuration syntax.

## 2. Target allowlist: hosts and ports

Every forward must match an explicit allowlist entry. An entry defines at least:

- the target host on the remote side (DNS name or IP address; names are treated as full hostnames, not as implicit wildcards),
- the permitted target port, or an explicitly configured set/range of target ports,
- whether the target is classified as private and a separate permission exists for it.

Before starting and before every reconnect, the tool must check host and port against the effective allowlist. A match requires that both host and port are permitted; missing, ambiguous, or invalid entries lead to rejection. Wildcards must not be silently derived from a hostname.

Targets in private address space (RFC 1918), as well as loopback or localhost targets reachable from the remote host, are forbidden by default. They may be used only if the policy explicitly allows them for the specific target. A general approval of the tunnel or of the SSH server does not lift this default prohibition.

## 3. Binding rule

The local listen endpoint of a forward must be bound exclusively to `127.0.0.1` or `::1`. Any other bind address is inadmissible. In particular, the listener must never listen on `0.0.0.0`, `::`, or a LAN/public address. A missing bind address must not lead to a broader binding; it must be pinned to one of the two loopback addresses or rejected.

## 4. Approval rule at start

An explicit approval is required when a tunnel is first started. A new approval is likewise required as soon as the effective target allowlist changes; an earlier approval then does not count as consent to the changed targets or ports.

Without a valid approval, the tunnel must not be started or automatically restored after an interruption. The approval must cover the effective tunnel configuration, including permitted targets and the local loopback binding. A reconnect may reuse a granted approval only as long as this configuration is unchanged and still policy-compliant.

## 5. Network path and relay: egress policy

The SSH connection path and any relay/host context must be compatible with the HARW egress policy. The tool must not circumvent egress rules by using a tunnel as a generic relay channel or the remote SSH host as an unrestricted proxy.

The policy check must take the actual network path into account: the outgoing SSH endpoint in the local execution context as well as the target reached through the forward in the context of the remote host. Where a relay is used, its identity or context must also be permitted by the egress policy; a relay must not justify a broader target allowlist. Unclassified paths, or paths not cleared by the egress policy, must be rejected. The same checks apply at start and at reconnect.

## 6. Layer assignment

For the planned crate `harw-tool-tunnel`, a package entry in `xtask/arch-policy.toml` is to be provided. The layer assignment must reflect its role as a managed runtime/job tool and comply with the dependency rules that apply there. This contract requires the later classification; this document does not change `xtask/arch-policy.toml`.

## 7. Key material: keyfile reference instead of content

Configuration and process handover may contain only a reference to a key file, never the content of a private key. Private key content must not appear as a process argument, nor in logs, error messages, approval displays, artifacts, or any other persistent or diagnostic output.

The tool must redact sensitive values before any output. The reference itself is not key content; paths or metadata that are themselves classified as sensitive must be protected or redacted in accordance with the secret and audit policy. Key files must not be copied or embedded into tunnel artifacts.

## 8. Limits of v1

v1 covers only local forwards (`-L`). The following are not supported and must not be silently permitted:

- remote forwards (`-R`),
- SOCKS forwards (`-D`),
- a separate daemon independent of the managed HARW lifecycle.

Any later extension of these limits requires its own policy and approval review; it is not cleared by this contract.
