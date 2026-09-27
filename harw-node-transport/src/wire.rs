//! Length-prefixed handshake frames over the TLS stream.

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
pub(crate) async fn read_frame<R>(io: &mut R) -> Result<Vec<u8>, TransportError>
where
    R: AsyncRead + Unpin,
{
    let mut len_buf = [0u8; 4];
    match io.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
            return Err(HandshakeError::RejectedByPeer.into());
        }
        Err(err) => return Err(err.into()),
    }
    let len = usize::try_from(u32::from_be_bytes(len_buf)).unwrap_or(usize::MAX);
    if len > MAX_HANDSHAKE_FRAME {
        return Err(HandshakeError::FrameTooLarge(len).into());
    }
    let mut payload = vec![0u8; len];
    io.read_exact(&mut payload).await?;
    Ok(payload)
}
