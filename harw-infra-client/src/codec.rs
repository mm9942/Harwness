//! `CGK1` request/response bodies of the AuthHub KMS routes.
//!
//! The wire format is owned by `crypt_guard_hyper::codec` (module docs of
//! `codec::request` and `codec::response` are the spec). This module uses
//! its public [`FrameWriter`] / [`FrameReader`] for everything that is not
//! secret.
//!
//! `FrameWriter` is documented as "only for non-secret output" (its buffer
//! is a plain `Vec<u8>` that may reallocate). The one request that carries
//! secret bytes — `wrap`, whose `material` field is the plaintext key — is
//! therefore encoded by [`wrap_body`] here, following `codec/frame.rs`
//! byte for byte (`MAGIC` | `VERSION` | per field `len u32 BE` + bytes) and
//! using the crate's own `MAGIC`/`VERSION` constants, into a zeroizing
//! buffer sized exactly up front. A unit test pins it to `FrameWriter`'s
//! output, so a codec change upstream fails here first.

use crypt_guard_hyper::codec::frame::{MAGIC, VERSION};
use crypt_guard_hyper::codec::{CodecError, FrameReader, FrameWriter};
use zeroize::Zeroizing;

use crate::error::InfraClientError;
use crate::key::{
    CreatedKey, KeyContext, KeyDescription, KeyProfile, KeyRef, KeyState, PublicKeyBytes,
    WrappedKey,
};

// Media type of `CGK1` bodies (`application/vnd.cryptguard.kms.v1`).
pub(crate) use crypt_guard_hyper::codec::CONTENT_TYPE as FRAME_CONTENT_TYPE;

const MALFORMED: InfraClientError = InfraClientError::Protocol("malformed CGK1 frame");

fn codec_err(_: CodecError) -> InfraClientError {
    MALFORMED
}

/// `generate`: `namespace`(text) `id`(text) `profile`(text).
pub(crate) fn generate_body(
    key: &KeyRef,
    profile: KeyProfile,
) -> Result<Vec<u8>, InfraClientError> {
    let mut writer = FrameWriter::new();
    writer
        .field(key.namespace().as_bytes())
        .map_err(codec_err)?;
    writer.field(key.id().as_bytes()).map_err(codec_err)?;
    writer
        .field(profile.wire_name().as_bytes())
        .map_err(codec_err)?;
    Ok(writer.finish())
}

/// `unwrap`: `info` `aad` `wrapped`. Not secret (the blob is ciphertext).
pub(crate) fn unwrap_body(
    context: &KeyContext,
    wrapped: &WrappedKey,
) -> Result<Vec<u8>, InfraClientError> {
    let mut writer = FrameWriter::new();
    writer.field(&context.info).map_err(codec_err)?;
    writer.field(&context.aad).map_err(codec_err)?;
    writer.field(wrapped.as_bytes()).map_err(codec_err)?;
    Ok(writer.finish())
}

/// `rewrap`: `from_info` `from_aad` `to_namespace`(text) `to_id`(text)
/// `to_version`(u32, 0 = latest) `to_info` `to_aad` `wrapped`.
pub(crate) fn rewrap_body(
    from_context: &KeyContext,
    to: &KeyRef,
    to_context: &KeyContext,
    wrapped: &WrappedKey,
) -> Result<Vec<u8>, InfraClientError> {
    let mut writer = FrameWriter::new();
    writer.field(&from_context.info).map_err(codec_err)?;
    writer.field(&from_context.aad).map_err(codec_err)?;
    writer.field(to.namespace().as_bytes()).map_err(codec_err)?;
    writer.field(to.id().as_bytes()).map_err(codec_err)?;
    writer.u32(to.wire_version());
    writer.field(&to_context.info).map_err(codec_err)?;
    writer.field(&to_context.aad).map_err(codec_err)?;
    writer.field(wrapped.as_bytes()).map_err(codec_err)?;
    Ok(writer.finish())
}

/// `wrap`: `info` `aad` `material`🔒, encoded into zeroizing memory.
///
/// The buffer is allocated once with the exact final size, so no
/// reallocation leaves a stale copy of `material` behind.
pub(crate) fn wrap_body(
    context: &KeyContext,
    material: &[u8],
) -> Result<Zeroizing<Vec<u8>>, InfraClientError> {
    let fields: [&[u8]; 3] = [&context.info, &context.aad, material];
    let mut total = MAGIC.len() + 1;
    for field in fields {
        total = total
            .checked_add(4)
            .and_then(|n| n.checked_add(field.len()))
            .ok_or(MALFORMED)?;
    }
    let mut buf = Zeroizing::new(Vec::with_capacity(total));
    buf.extend_from_slice(&MAGIC);
    buf.push(VERSION);
    for field in fields {
        let len = u32::try_from(field.len()).map_err(|_| MALFORMED)?;
        buf.extend_from_slice(&len.to_be_bytes());
        buf.extend_from_slice(field);
    }
    Ok(buf)
}

/// `KeyCreated`: `namespace` `id` `version`(u32) `public`(field; empty if none).
pub(crate) fn parse_key_created(bytes: &[u8]) -> Result<CreatedKey, InfraClientError> {
    let mut reader = FrameReader::new(bytes).map_err(codec_err)?;
    let namespace = reader.text().map_err(codec_err)?;
    let id = reader.text().map_err(codec_err)?;
    let version = reader.u32().map_err(codec_err)?;
    let public = reader.field().map_err(codec_err)?;
    reader.finish().map_err(codec_err)?;
    let key = KeyRef::from_wire(namespace, id, version).map_err(|_| MALFORMED)?;
    let public = (!public.is_empty()).then(|| PublicKeyBytes::new(public.to_vec()));
    Ok(CreatedKey { key, public })
}

/// `Metadata`: `namespace` `id` `version`(u32) `state`(u8) `profile`(text; empty if unnamed).
pub(crate) fn parse_metadata(bytes: &[u8]) -> Result<KeyDescription, InfraClientError> {
    let mut reader = FrameReader::new(bytes).map_err(codec_err)?;
    let namespace = reader.text().map_err(codec_err)?;
    let id = reader.text().map_err(codec_err)?;
    let version = reader.u32().map_err(codec_err)?;
    let state = reader.u8().map_err(codec_err)?;
    let profile = reader.text().map_err(codec_err)?;
    reader.finish().map_err(codec_err)?;
    let key = KeyRef::from_wire(namespace, id, version).map_err(|_| MALFORMED)?;
    Ok(KeyDescription {
        key,
        state: KeyState::from_code(state),
        profile_name: (!profile.is_empty()).then(|| profile.to_owned()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    #[test]
    fn test_wrap_body_is_byte_identical_to_frame_writer() -> TestResult {
        let context = KeyContext::new(b"info".to_vec(), b"aad".to_vec());
        let material = b"secret-key-material";
        let secret = wrap_body(&context, material)?;

        let mut writer = FrameWriter::new();
        writer.field(b"info").map_err(ctx("info"))?;
        writer.field(b"aad").map_err(ctx("aad"))?;
        writer.field(material).map_err(ctx("material"))?;
        assert_eq!(secret.as_slice(), writer.finish().as_slice());
        // Sized exactly: no reallocation happened while encoding.
        assert_eq!(secret.len(), secret.capacity());

        let mut reader = FrameReader::new(&secret).map_err(ctx("reader"))?;
        assert_eq!(reader.field().map_err(ctx("f1"))?, b"info");
        assert_eq!(reader.field().map_err(ctx("f2"))?, b"aad");
        assert_eq!(reader.field().map_err(ctx("f3"))?, material);
        reader.finish().map_err(ctx("finish"))?;
        Ok(())
    }

    #[test]
    fn test_wrap_body_with_empty_context() -> TestResult {
        let secret = wrap_body(&KeyContext::default(), b"")?;
        assert_eq!(secret.as_slice(), b"CGK1\x01\0\0\0\0\0\0\0\0\0\0\0\0");
        Ok(())
    }

    #[test]
    fn test_parse_key_created_and_metadata() -> TestResult {
        let mut writer = FrameWriter::new();
        writer.field(b"app").map_err(ctx("ns"))?;
        writer.field(b"k1").map_err(ctx("id"))?;
        writer.u32(3);
        writer.field(b"pk").map_err(ctx("pk"))?;
        let created = parse_key_created(&writer.finish())?;
        assert_eq!(created.key, KeyRef::versioned("app", "k1", 3)?);
        assert_eq!(created.public, Some(PublicKeyBytes::new(b"pk".to_vec())));

        let mut writer = FrameWriter::new();
        writer.field(b"app").map_err(ctx("ns"))?;
        writer.field(b"k1").map_err(ctx("id"))?;
        writer.u32(2);
        writer.u8(2);
        writer.field(b"").map_err(ctx("profile"))?;
        let meta = parse_metadata(&writer.finish())?;
        assert_eq!(meta.key, KeyRef::versioned("app", "k1", 2)?);
        assert_eq!(meta.state, KeyState::Disabled);
        assert_eq!(meta.profile_name, None);
        Ok(())
    }

    #[test]
    fn test_parse_rejects_malformed_and_invalid_names() -> TestResult {
        assert_eq!(parse_metadata(b"nope"), Err(MALFORMED));

        let mut writer = FrameWriter::new();
        writer.field(b"../etc").map_err(ctx("ns"))?;
        writer.field(b"k1").map_err(ctx("id"))?;
        writer.u32(1);
        writer.field(b"").map_err(ctx("pk"))?;
        assert_eq!(parse_key_created(&writer.finish()), Err(MALFORMED));

        let mut writer = FrameWriter::new();
        writer.field(b"app").map_err(ctx("ns"))?;
        writer.field(b"k1").map_err(ctx("id"))?;
        writer.u32(1);
        writer.field(b"").map_err(ctx("pk"))?;
        let mut bytes = writer.finish();
        bytes.push(0);
        match parse_key_created(&bytes) {
            Err(InfraClientError::Protocol(_)) => Ok(()),
            other => Err(TestError::Unexpected(format!("{other:?}"))),
        }
    }
}
