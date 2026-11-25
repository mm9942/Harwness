use bytes::{Bytes, BytesMut};

pub const FLOW_CAPACITY: usize = 128;
pub const FLOW_DIRECTION_BYTES: usize = 32 * 1024;
pub const STREAM_CHUNK_BYTES: usize = 4 * 1024;

/// Queued bytes in one direction. The caller retains the unaccepted suffix.
#[derive(Default)]
pub struct FlowBuffer {
    pending: BytesMut,
}

impl FlowBuffer {
    pub fn push(&mut self, data: &[u8]) -> usize {
        let accepted = data.len().min(FLOW_DIRECTION_BYTES - self.pending.len());
        self.pending.extend_from_slice(&data[..accepted]);
        accepted
    }

    pub fn pop_chunk(&mut self) -> Option<Bytes> {
        let count = self.pending.len().min(STREAM_CHUNK_BYTES);
        (count > 0).then(|| self.pending.split_to(count).freeze())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backpressure_preserves_byte_order() {
        let mut buffer = FlowBuffer::default();
        let input = [7; FLOW_DIRECTION_BYTES + 1];
        assert_eq!(buffer.push(&input), FLOW_DIRECTION_BYTES);
        assert_eq!(buffer.push(&[8]), 0);
        assert_eq!(
            buffer.pop_chunk().unwrap().as_ref(),
            &[7; STREAM_CHUNK_BYTES]
        );
        assert_eq!(buffer.push(&[8]), 1);
        for _ in 1..FLOW_DIRECTION_BYTES / STREAM_CHUNK_BYTES {
            assert_eq!(
                buffer.pop_chunk().unwrap().as_ref(),
                &[7; STREAM_CHUNK_BYTES]
            );
        }
        assert_eq!(buffer.pop_chunk().unwrap().as_ref(), &[8]);
        assert!(buffer.pop_chunk().is_none());
    }
}
