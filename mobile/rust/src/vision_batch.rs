//! Pure validation of model-supplied captions against request-local positions.
//!
//! Assign ordinals when building each request (not when replies arrive). The
//! caller owns the mapping from those ordinals to actual assets; this module
//! never receives identifiers, paths, or bytes from the source images. It
//! expects already parsed rows: the transport must bound and validate its
//! response body before decoding it. Successful captions remain untrusted
//! text and must not be written to logs.

use std::ops::RangeInclusive;

/// Bound on one grouped provider request, not on the queued one-image jobs.
pub(crate) const MAX_VISION_BATCH_IMAGES: usize = 40;
pub(crate) const MAX_VISION_CAPTION_BYTES: usize = 2_048;

/// One decoded response row. Do not derive `Debug`: captions may be private.
pub(crate) struct CaptionReply {
    pub(crate) ordinal: u64,
    pub(crate) caption: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum VisionBatchError {
    Empty,
    TooManyImages,
    /// Reject zero/out-of-range values without echoing the supplied number.
    UnknownOrdinal,
    DuplicateOrdinal(usize),
    MissingOrdinals(Vec<usize>),
    BlankCaption(usize),
    CaptionTooLong(usize),
    InvalidControl(usize),
}

/// Immutable expected layout of one ordered request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct VisionBatch {
    image_count: usize,
}

impl VisionBatch {
    pub(crate) fn new(image_count: usize) -> Result<Self, VisionBatchError> {
        match image_count {
            0 => Err(VisionBatchError::Empty),
            1..=MAX_VISION_BATCH_IMAGES => Ok(Self { image_count }),
            _ => Err(VisionBatchError::TooManyImages),
        }
    }

    /// These are the only ordinals the provider should receive for this batch.
    pub(crate) fn request_ordinals(&self) -> RangeInclusive<usize> {
        1..=self.image_count
    }

    /// Returns exactly one caption per requested ordinal, in request order.
    /// A malformed batch produces no successful partial result.
    pub(crate) fn validate_replies(
        &self,
        replies: impl IntoIterator<Item = CaptionReply>,
    ) -> Result<Vec<String>, VisionBatchError> {
        let mut slots = vec![None; self.image_count];
        for CaptionReply { ordinal, caption } in replies {
            let ordinal = usize::try_from(ordinal).map_err(|_| VisionBatchError::UnknownOrdinal)?;
            let slot = ordinal
                .checked_sub(1)
                .and_then(|index| slots.get_mut(index))
                .ok_or(VisionBatchError::UnknownOrdinal)?;
            if slot.is_some() {
                return Err(VisionBatchError::DuplicateOrdinal(ordinal));
            }
            *slot = Some(validate_caption(ordinal, &caption)?.to_owned());
        }

        let missing: Vec<_> = self
            .request_ordinals()
            .zip(&slots)
            .filter_map(|(ordinal, slot)| slot.is_none().then_some(ordinal))
            .collect();
        if !missing.is_empty() {
            return Err(VisionBatchError::MissingOrdinals(missing));
        }
        Ok(slots.into_iter().flatten().collect())
    }
}

/// Validate text from a request with one image. The caller keeps its immutable
/// ticket, so no model-supplied index is needed or trusted. Error ordinal 1
/// belongs to this request, not the image's position in the job queue.
pub(crate) fn validate_single_caption(caption: &str) -> Result<&str, VisionBatchError> {
    validate_caption(1, caption)
}

fn validate_caption(ordinal: usize, caption: &str) -> Result<&str, VisionBatchError> {
    if caption.len() > MAX_VISION_CAPTION_BYTES {
        return Err(VisionBatchError::CaptionTooLong(ordinal));
    }
    if caption
        .chars()
        .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t'))
    {
        return Err(VisionBatchError::InvalidControl(ordinal));
    }
    let caption = caption.trim();
    if caption.is_empty() {
        return Err(VisionBatchError::BlankCaption(ordinal));
    }
    Ok(caption)
}

#[cfg(test)]
mod tests {
    use super::{CaptionReply, VisionBatch, VisionBatchError, validate_single_caption};
    use std::cell::Cell;

    fn reply(ordinal: u64, caption: &str) -> CaptionReply {
        CaptionReply {
            ordinal,
            caption: caption.to_owned(),
        }
    }

    #[test]
    fn requests_must_have_between_one_and_forty_images() {
        assert_eq!(VisionBatch::new(0).unwrap_err(), VisionBatchError::Empty);
        assert_eq!(
            VisionBatch::new(41).unwrap_err(),
            VisionBatchError::TooManyImages
        );
        assert_eq!(VisionBatch::new(50), Err(VisionBatchError::TooManyImages));
    }

    #[test]
    fn request_ordinals_are_one_based_and_stay_in_request_order() {
        let batch = VisionBatch::new(3).unwrap();
        assert_eq!(batch.request_ordinals().collect::<Vec<_>>(), vec![1, 2, 3]);
        let largest = VisionBatch::new(40).unwrap();
        let rows = (1..=40).rev().map(|n| CaptionReply {
            ordinal: n,
            caption: format!("image {n}"),
        });
        let result = largest.validate_replies(rows).unwrap();
        assert_eq!(result.len(), 40);
        assert_eq!(result.first().map(String::as_str), Some("image 1"));
        assert_eq!(result.last().map(String::as_str), Some("image 40"));
    }

    #[test]
    fn scrambled_reply_positions_do_not_swap_images() {
        let ordered = VisionBatch::new(3)
            .unwrap()
            .validate_replies([reply(3, "third"), reply(1, " first "), reply(2, "second")])
            .unwrap();
        assert_eq!(ordered, ["first", "second", "third"]);
    }

    #[test]
    fn duplicate_reply_never_overwrites_a_valid_caption() {
        let result = VisionBatch::new(2)
            .unwrap()
            .validate_replies([reply(2, "first claim"), reply(2, "second claim")]);
        assert_eq!(result, Err(VisionBatchError::DuplicateOrdinal(2)));
    }

    #[test]
    fn zero_out_of_request_range_and_integer_overflow_are_unknown() {
        let batch = VisionBatch::new(2).unwrap();
        for ordinal in [0, 3, u64::MAX] {
            // Do not stop inspecting the response as soon as all slots are filled.
            let result =
                batch.validate_replies([reply(1, "one"), reply(2, "two"), reply(ordinal, "extra")]);
            assert_eq!(result, Err(VisionBatchError::UnknownOrdinal));
        }
    }

    #[test]
    fn missing_positions_are_reported_in_request_order() {
        let result = VisionBatch::new(5)
            .unwrap()
            .validate_replies([reply(4, "four"), reply(2, "two")]);
        assert_eq!(
            result,
            Err(VisionBatchError::MissingOrdinals(vec![1, 3, 5]))
        );
    }

    #[test]
    fn blank_or_control_payloads_are_rejected_without_echoing_them() {
        let batch = VisionBatch::new(1).unwrap();
        let result = batch.validate_replies([reply(1, " \n\t ")]);
        assert_eq!(result, Err(VisionBatchError::BlankCaption(1)));

        let result = batch.validate_replies([reply(1, "file:///private/photo\0")]);
        assert_eq!(result, Err(VisionBatchError::InvalidControl(1)));
        assert!(!format!("{result:?}").contains("/private/photo"));

        let result = batch.validate_replies([reply(1, "first\nsecond\tcolumn")]);
        assert_eq!(result.unwrap(), ["first\nsecond\tcolumn"]);
    }

    #[test]
    fn caption_bound_counts_utf8_bytes_before_trimming() {
        let batch = VisionBatch::new(1).unwrap();
        let fits = "é".repeat(1024); // 2048 UTF-8 bytes.
        assert_eq!(
            batch.validate_replies([reply(1, &fits)]).unwrap(),
            [fits.clone()]
        );
        for oversize in [format!("{fits} "), "é".repeat(1025)] {
            let result = batch.validate_replies([reply(1, &oversize)]);
            assert_eq!(result, Err(VisionBatchError::CaptionTooLong(1)));
        }
    }

    #[test]
    fn validation_does_not_keep_consuming_an_invalid_response() {
        let consumed = Cell::new(0);
        let rows = (0..200).map(|index| {
            consumed.set(consumed.get() + 1);
            reply(if index == 0 { 2 } else { 1 }, "text")
        });
        let result = VisionBatch::new(1).unwrap().validate_replies(rows);
        assert_eq!(result, Err(VisionBatchError::UnknownOrdinal));
        assert_eq!(consumed.get(), 1);
    }

    #[test]
    fn a_single_reply_needs_no_model_supplied_ordinal_and_keeps_same_bounds() {
        assert_eq!(validate_single_caption("  caption  "), Ok("caption"));
        assert_eq!(
            validate_single_caption("  \t "),
            Err(VisionBatchError::BlankCaption(1))
        );
        assert_eq!(
            validate_single_caption("inside\0"),
            Err(VisionBatchError::InvalidControl(1))
        );
        assert_eq!(
            validate_single_caption(&"é".repeat(1025)),
            Err(VisionBatchError::CaptionTooLong(1))
        );
    }
}
