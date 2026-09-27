//! Structured audit events.
//!
//! Two sources, both on the tracing target [`AUDIT_TARGET`]:
//!
//! - [`AuditProvider`] wraps the policy-guarded provider and emits one
//!   `crypto` event per executed operation: request id (same lowercase hex
//!   as the `x-request-id` response header), principal, operation kind, key
//!   reference(s) and outcome (`ok` or the stable CryptGuard error class,
//!   e.g. `forbidden`). It sits *outside* `PolicyProvider`, so policy denials
//!   are audited too.
//! - [`http_event`] emits one `http` event per HTTP request with the peer
//!   uid/pid, method, path and status. This also covers requests rejected
//!   before they reach the provider (unknown route, unauthenticated peer,
//!   oversized body).
//!
//! Never logged (tower plan §27): plaintext, key material, wrapped blobs,
//! `info`/`aad`, request or response bodies, bearer tokens or any header
//! value. Key references and principals are identities, not secrets.

use core::task::{Context, Poll};

use crypt_guard_service::{
    CryptoOperation, CryptoProvider, CryptoRequest, CryptoResponse, CryptoServiceError,
};

use crate::auth::PeerCred;

/// The tracing target of every audit event.
pub const AUDIT_TARGET: &str = "harw_auth_hub::audit";

/// A provider wrapper that audits every operation and its outcome.
#[derive(Debug)]
pub struct AuditProvider<P> {
    inner: P,
}

impl<P> AuditProvider<P> {
    /// Audit every operation executed by `inner`.
    pub fn new(inner: P) -> Self {
        Self { inner }
    }

    /// Borrow the wrapped provider.
    pub fn inner(&self) -> &P {
        &self.inner
    }
}

impl<P: CryptoProvider> CryptoProvider for AuditProvider<P> {
    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), CryptoServiceError>> {
        self.inner.poll_ready(cx)
    }

    fn execute(&mut self, request: CryptoRequest) -> Result<CryptoResponse, CryptoServiceError> {
        // Everything the record needs is copied out *before* the request
        // (possibly carrying secrets) is moved into the provider.
        let record = AuditRecord::of(&request);
        let result = self.inner.execute(request);
        record.emit(&result);
        result
    }
}

/// Non-secret summary of one crypto request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditRecord {
    /// Request id, lowercase hex (matches `x-request-id`).
    pub request_id: String,
    /// Authenticated principal, or `-` for anonymous.
    pub principal: String,
    /// Stable operation name (`OpKind::name`).
    pub op: &'static str,
    /// Primary key reference (`namespace/id[@version]`).
    pub key: String,
    /// Rewrap target key reference, empty otherwise.
    pub target: String,
}

impl AuditRecord {
    /// Summarize a request without touching any secret field.
    #[must_use]
    pub fn of(request: &CryptoRequest) -> Self {
        let (key, target) = key_refs(&request.operation);
        Self {
            request_id: format!("{:x}", request.context.request_id.0),
            principal: request
                .context
                .principal
                .as_ref()
                .map_or_else(|| "-".to_owned(), |p| p.as_str().to_owned()),
            op: request.operation.name(),
            key,
            target,
        }
    }

    /// Emit the audit event for `result`.
    pub fn emit(&self, result: &Result<CryptoResponse, CryptoServiceError>) {
        match result {
            Ok(response) => {
                let version = created_version(response);
                tracing::info!(
                    target: AUDIT_TARGET,
                    event = "crypto",
                    request_id = %self.request_id,
                    principal = %self.principal,
                    op = self.op,
                    key = %self.key,
                    target_key = %self.target,
                    created_version = version,
                    outcome = "ok",
                );
            }
            Err(error) => {
                let denied = matches!(
                    error,
                    CryptoServiceError::Forbidden | CryptoServiceError::Unauthenticated
                );
                if denied {
                    tracing::warn!(
                        target: AUDIT_TARGET,
                        event = "crypto",
                        request_id = %self.request_id,
                        principal = %self.principal,
                        op = self.op,
                        key = %self.key,
                        target_key = %self.target,
                        outcome = error.name(),
                    );
                } else {
                    tracing::info!(
                        target: AUDIT_TARGET,
                        event = "crypto",
                        request_id = %self.request_id,
                        principal = %self.principal,
                        op = self.op,
                        key = %self.key,
                        target_key = %self.target,
                        outcome = error.name(),
                    );
                }
            }
        }
    }
}

/// Emit the per-HTTP-request audit event.
pub fn http_event(peer: Option<PeerCred>, method: &http::Method, path: &str, status: u16) {
    let uid = peer.map(|p| p.uid);
    let pid = peer.and_then(|p| p.pid);
    tracing::info!(
        target: AUDIT_TARGET,
        event = "http",
        peer_uid = uid,
        peer_pid = pid,
        method = %method,
        path = %path,
        status,
    );
}

fn created_version(response: &CryptoResponse) -> Option<u32> {
    match response {
        CryptoResponse::KeyCreated { key, .. } => key.version.map(|v| v.get()),
        _ => None,
    }
}

fn key_refs(operation: &CryptoOperation) -> (String, String) {
    let one = |key: &crypt_guard_service::KeyRef| (key.to_string(), String::new());
    match operation {
        CryptoOperation::Generate(op) => (format!("{}/{}", op.namespace, op.id), String::new()),
        CryptoOperation::Rotate(op) => one(&op.key),
        CryptoOperation::Disable(op) => one(&op.key),
        CryptoOperation::Enable(op) => one(&op.key),
        CryptoOperation::Destroy(op) => one(&op.key),
        CryptoOperation::Describe(op) => one(&op.key),
        CryptoOperation::PublicKey(op) => one(&op.key),
        CryptoOperation::Encrypt(op) => one(&op.key),
        CryptoOperation::Decrypt(op) => one(&op.key),
        CryptoOperation::Sign(op) => one(&op.key),
        CryptoOperation::Verify(op) => one(&op.key),
        CryptoOperation::WrapKey(op) => one(&op.key),
        CryptoOperation::UnwrapKey(op) => one(&op.key),
        CryptoOperation::RewrapKey(op) => (op.from.to_string(), op.to.to_string()),
        // `CryptoOperation` is `#[non_exhaustive]`.
        _ => (String::new(), String::new()),
    }
}

#[cfg(test)]
mod tests {
    use crypt_guard_service::{
        CryptoOperation, CryptoProvider, CryptoRequest, CryptoServiceError, DescribeKey, Encrypt,
        KeyId, KeyNamespace, KeyRef, NullProvider, Principal as CgPrincipal, RequestContext,
        RequestId, SecretBytes,
    };

    use super::{AuditProvider, AuditRecord};
    use crate::test_support::{TestError, TestResult, ctx};

    fn key() -> TestResult<KeyRef> {
        Ok(KeyRef::latest(
            KeyNamespace::new("app").map_err(ctx("namespace"))?,
            KeyId::new("k1").map_err(ctx("key id"))?,
        ))
    }

    #[test]
    fn record_contains_ids_but_no_payload() -> TestResult {
        let request = CryptoRequest::with_context(
            RequestContext {
                request_id: RequestId(0x2a),
                principal: Some(CgPrincipal::new("harw-web")),
            },
            CryptoOperation::Encrypt(Encrypt {
                key: key()?,
                plaintext: SecretBytes::copy_from_slice(b"top-secret-plaintext"),
                context: crypt_guard_service::CryptoContext::default(),
            }),
        );
        let record = AuditRecord::of(&request);
        assert_eq!(record.request_id, "2a");
        assert_eq!(record.principal, "harw-web");
        assert_eq!(record.op, "encrypt");
        assert_eq!(record.key, "app/k1");
        assert!(!format!("{record:?}").contains("top-secret"));
        Ok(())
    }

    #[test]
    fn provider_passes_result_through() -> TestResult {
        let mut provider = AuditProvider::new(NullProvider);
        let request = CryptoRequest::new(
            RequestId(1),
            CryptoOperation::Describe(DescribeKey { key: key()? }),
        );
        match provider.execute(request) {
            Err(CryptoServiceError::Unsupported) => Ok(()),
            other => Err(TestError::Unexpected(format!("{other:?}"))),
        }
    }
}
