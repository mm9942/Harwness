//! Length-prefixed handshake frames over the TLS stream.

use bytes::{Bytes, BytesMut};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::error::{HandshakeError, TransportError};
use crate::handshake::MAX_HANDSHAKE_FRAME;

/// Writes one frame (`u32` big-endian length, then payload) and flushes.
pub(crate) async fn write_frame<W>(io: &mut W, payload: &[u8]) -> Result<(), TransportError>
where
    W: AsyncWrite + Unpin,
{
    if payload.len() > MAX_HANDSHAKE_FRAME {
        return Err(HandshakeError::FrameTooLarge(payload.len()).into());
    }
    let len =
        u32::try_from(payload.len()).map_err(|_| HandshakeError::FrameTooLarge(payload.len()))?;
    io.write_all(&len.to_be_bytes()).await?;
    io.write_all(payload).await?;
    io.flush().await?;
    Ok(())
}

/// Reads one frame, refusing lengths above [`MAX_HANDSHAKE_FRAME`] before
/// allocating. A clean EOF before the length prefix means the peer closed
/// the channel: [`HandshakeError::RejectedByPeer`].
pub(crate) async fn read_frame<R>(io: &mut R) -> Result<Bytes, TransportError>
where
    R: AsyncRead + Unpin,
{
    let mut len_buf = [0u8; 4];
    if io.read(&mut len_buf[..1]).await? == 0 {
        return Err(HandshakeError::RejectedByPeer.into());
    }
    io.read_exact(&mut len_buf[1..]).await?;
    let wire_len = u32::from_be_bytes(len_buf);
    let len = usize::try_from(wire_len).unwrap_or(usize::MAX);
    if len > MAX_HANDSHAKE_FRAME {
        return Err(HandshakeError::FrameTooLarge(len).into());
    }
    const IO_CHUNK: usize = 4 * 1024;
    let mut payload = BytesMut::with_capacity(len.min(IO_CHUNK));
    // The length prefix bounds this reader to the current frame, even when
    // the peer writes the following frame in the same transport chunk.
    let mut frame = io.take(u64::from(wire_len));
    while payload.len() < len {
        payload.reserve((len - payload.len()).min(IO_CHUNK));
        if frame.read_buf(&mut payload).await? == 0 {
            return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into());
        }
    }
    Ok(payload.freeze())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fragmented_frame_leaves_the_following_frame_available() -> Result<(), TransportError> {
        let (mut sender, mut receiver) = tokio::io::duplex(13);
        let payload = Bytes::from_static(&[42; 8193]);
        let write = async {
            write_frame(&mut sender, &payload).await?;
            write_frame(&mut sender, b"next").await
        };
        let read = async {
            assert_eq!(read_frame(&mut receiver).await?, payload);
            assert_eq!(
                read_frame(&mut receiver).await?,
                Bytes::from_static(b"next")
            );
            Ok(())
        };
        tokio::try_join!(write, read)?;
        Ok(())
    }

    #[tokio::test]
    async fn truncated_prefix_and_payload_are_io_errors() {
        for raw in [&[0, 0][..], &[0, 0, 0, 2, 1][..]] {
            assert!(matches!(
                read_frame(&mut &raw[..]).await,
                Err(TransportError::Io(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof
            ));
        }
    }
}
