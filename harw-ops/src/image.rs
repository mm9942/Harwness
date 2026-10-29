//! Explicit auxiliary vision call for models that cannot inspect pixels themselves.
use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};
use std::{future::Future, pin::Pin, sync::Arc};

/// Runtime-owned image loading and model routing boundary.
pub trait ImageInspector: Send + Sync {
    /// Reads a sandbox-scoped image and asks exactly one selected vision model.
    fn inspect<'a>(
        &'a self,
        ctx: &'a OpContext,
        path: &'a str,
        question: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<OpOutput, OpError>> + Send + 'a>>;
}
/// Bound by the production runtime; absence fails closed.
pub type SharedImageInspector = Arc<dyn ImageInspector>;

#[derive(Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageInspectArgs {
    /// Workspace-relative image path; absolute paths and traversal are rejected.
    pub path: String,
    /// Specific question about the image, at most 4096 bytes.
    pub question: String,
}

impl harw_operations::FromRawArgs for ImageInspectArgs {
    fn from_raw_args(_tokens: &[String]) -> Result<Self, OpError> {
        Err(OpError::InvalidArguments(
            "image.inspect accepts structured tool arguments".to_owned(),
        ))
    }
}

#[operation(
    name = "image.inspect",
    summary = "Ask the explicitly selected image-understanding model about a workspace image. Returns model provenance and bounded analysis. Use /models image <provider/model> to select a vision model.",
    domain = "knowledge",
    permission = "observer",
    model_tool(readonly, approval = "always")
)]
async fn image_inspect(ctx: &OpContext, args: ImageInspectArgs) -> Result<OpOutput, OpError> {
    if args.question.trim().is_empty() || args.question.len() > 4096 || args.path.len() > 4096 {
        return Err(OpError::InvalidArguments(
            "image.inspect requires a nonempty question and path of at most 4096 bytes each"
                .to_owned(),
        ));
    }
    let inspector = ctx.service::<SharedImageInspector>().ok_or_else(|| {
        OpError::NotAvailable(
            "image_understanding_unavailable: this runtime has no image-model service".to_owned(),
        )
    })?;
    inspector.inspect(ctx, &args.path, &args.question).await
}
