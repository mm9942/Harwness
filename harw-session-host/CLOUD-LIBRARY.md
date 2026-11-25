# Cloud Library v1

The remote gateway opts in by setting `HARW_CLOUD_LIBRARY_KEY_FILE` to an
absolute path to a dedicated private regular credential containing exactly
32 raw random bytes. Set the file to mode 0600, owned by the gateway
process, with one link and an operator-controlled parent path. Use an
independent credential per installation. It must not be stored alongside
the Library backups, reused for Assistant, logged, or sent to a phone. A
missing environment setting disables this extension; an explicitly
configured missing, corrupt, or mismatched credential fails closed.
Retain the credential and stable configured profile path to reopen existing
encrypted catalogues; code does not rotate it or promise rollback resistance
against an administrator who restores old files.

Cloud here means host-encrypted storage over the existing authenticated
transport, not E2EE from the gateway. The gateway handles only explicit
Library-selected uploads. It never reads or syncs Android's local PIN
vault; never sends an asset to an inference provider or chat; and never
exposes it through `media.get`, session IDs, tools, or agent credentials.
JPEG dimensions/uploads, catalogue revisions, and disk/concurrency budgets
are bounded. Private per-profile directories contain only opaque
compartment names and authenticated CryptGuard ML-KEM-1024 /
XChaCha20-Poly1305 ciphertext; a separate scoped keyed authenticator binds
the ciphertext to the profile, enrolled device/node, principal and
tenant. This is not a public digital signature.

Client requests must negotiate `cloud_library_v1` and use `cloud.library`.
Explicit upload and scope consent belong in the client. An uncertain
mutation should reload the same Cloud scope and never auto-resend.
