//! Sandboxed image reads for model-initiated native vision input.
use harw_authority::Permission;
use harw_media::{MediaLimits, MemorySource, tool_image::ImageToolResult};
use harw_protocol::ImageDetail;
use harw_tools::{
    ToolCall, ToolExecutionContext, ToolExecutor, ToolExecutorFuture, ToolOutput, ToolsError,
};
use serde::Deserialize;
use std::io::Read;
use std::sync::{Arc, OnceLock};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    path: String,
    #[serde(default)]
    detail: Option<ImageDetail>,
}

/// Reads workspace image bytes under the same permission and path policy as fs.read.
pub struct FsViewImageExecutor;

static SOURCE: OnceLock<Arc<MemorySource>> = OnceLock::new();

fn source() -> Arc<MemorySource> {
    let source = Arc::clone(SOURCE.get_or_init(|| Arc::new(MemorySource::new())));
    harw_media::install_media_source(Arc::clone(&source) as _);
    source
}

impl FsViewImageExecutor {
    /// Descriptive read activity; never an authority grant.
    pub const ACTIVITY: Option<harw_tools::ActivityDescriptor> =
        Some(harw_tools::ActivityDescriptor::new(
            "filesystem",
            "view_image",
            Some("path"),
            harw_tools::Effect::Observe,
        ));

    fn read(context: &ToolExecutionContext, call: &ToolCall) -> Result<ToolOutput, ToolsError> {
        if let Some(error) = harw_tools::sandbox_guard::require_permission(
            context,
            Permission::ReadWorkspace,
            "fs.view_image",
        ) {
            return Ok(error);
        }
        let args: Args = serde_json::from_value(call.arguments.clone()).map_err(|error| {
            ToolsError::InvalidArguments {
                name: "fs.view_image".to_owned(),
                reason: error.to_string(),
            }
        })?;
        let result = (|| -> Result<ToolOutput, String> {
            let relative = crate::tree::normalize_relative(&args.path)?;
            let start = crate::symlink::open_start(
                context.sandbox().workspace().canonical_root(),
                &relative,
                "fs.view_image",
                &args.path,
            )
            .map_err(|error| error.to_string())?;
            let mut file = start
                .workspace
                .open_any(&start.rel)
                .map_err(|error| error.to_string())?;
            let metadata = file.metadata().map_err(|error| error.to_string())?;
            let limit = MediaLimits::default().max_bytes;
            if !metadata.is_file() {
                return Err("expected a regular image file".to_owned());
            }
            if metadata.len() > limit {
                return Err(format!("image exceeds {limit} byte media limit"));
            }
            let mut bytes = Vec::new();
            file.by_ref()
                .take(limit.saturating_add(1))
                .read_to_end(&mut bytes)
                .map_err(|error| error.to_string())?;
            // Bounded process memory: this read tool creates no files and uses no ambient home.
            // Missing/evicted media is explicitly reported on subsequent model requests.
            let media = source().put(&bytes).map_err(|error| error.to_string())?;
            let descriptor = ImageToolResult::ImageInspection {
                media,
                detail: args.detail,
            };
            serde_json::to_value(descriptor)
                .map(ToolOutput::json)
                .map_err(|error| error.to_string())
        })();
        Ok(result.unwrap_or_else(|reason| ToolOutput::error(format!("fs.view_image: {reason}"))))
    }
}

impl ToolExecutor for FsViewImageExecutor {
    fn execute<'a>(
        &'a self,
        context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        let context = context.clone();
        let call = call.clone();
        Box::pin(crate::blocking::run_blocking("fs.view_image", move || {
            Self::read(&context, &call)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Fixture, TestResult, call};

    #[test]
    fn image_read_produces_verified_native_media_without_source_paths() -> TestResult {
        let fixture = Fixture::new()?;
        let context = fixture.ctx(vec![Permission::ReadWorkspace])?;
        let chunk = |kind: &[u8; 4], data: &[u8]| {
            let mut bytes = u32::try_from(data.len())
                .unwrap_or(0)
                .to_be_bytes()
                .to_vec();
            bytes.extend_from_slice(kind);
            bytes.extend_from_slice(data);
            bytes.extend_from_slice(&[0; 4]);
            bytes
        };
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut header = 20_u32.to_be_bytes().to_vec();
        header.extend_from_slice(&10_u32.to_be_bytes());
        header.extend_from_slice(&[8, 2, 0, 0, 0]);
        png.extend(chunk(b"IHDR", &header));
        png.extend(chunk(b"IDAT", b"pixels"));
        png.extend(chunk(b"IEND", b""));
        std::fs::write(fixture.ws.join("secret-source.png"), &png)?;
        let output = FsViewImageExecutor::read(
            &context,
            &call(
                "fs.view_image",
                serde_json::json!({"path":"secret-source.png","detail":"high"}),
            ),
        )?;
        let ToolOutput::Json { content } = output else {
            return Err(crate::test_support::TestError::Missing(
                "native image descriptor",
            ));
        };
        assert!(!content.to_string().contains("secret-source"));
        let descriptor: ImageToolResult = serde_json::from_value(content)?;
        let (media, detail) = descriptor.image();
        assert_eq!(detail, Some(ImageDetail::High));
        let bytes = harw_media::load_installed_media(media)
            .map_err(crate::test_support::ctx("image source"))?;
        assert_eq!(bytes, png);
        Ok(())
    }

    #[test]
    fn image_read_requires_permission_and_rejects_paths_and_non_images() -> TestResult {
        let fixture = Fixture::new()?;
        let denied = fixture.ctx(vec![])?;
        let permitted = fixture.ctx(vec![Permission::ReadWorkspace])?;
        let image_call = |path| call("fs.view_image", serde_json::json!({"path":path}));
        assert!(matches!(
            FsViewImageExecutor::read(&denied, &image_call("a.png"))?,
            ToolOutput::Error { .. }
        ));
        assert!(matches!(
            FsViewImageExecutor::read(&permitted, &image_call("../outside/secret.txt"))?,
            ToolOutput::Error { .. }
        ));
        std::fs::write(fixture.ws.join("a.png"), b"this is not a PNG")?;
        assert!(matches!(
            FsViewImageExecutor::read(&permitted, &image_call("a.png"))?,
            ToolOutput::Error { .. }
        ));
        Ok(())
    }
}
