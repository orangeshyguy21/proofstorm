//! Length-prefixed JSON over an authenticated Kubernetes port-forward stream.
use crate::MAX_FRAME_BYTES;
use serde::{Serialize, de::DeserializeOwned};
use std::io;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// # Errors
/// Rejects oversized frames before allocating their body, and rejects malformed JSON.
pub async fn read<R: AsyncRead + Unpin, T: DeserializeOwned>(reader: &mut R) -> io::Result<T> {
    let length = reader.read_u32().await? as usize;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid prober frame length",
        ));
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes).await?;
    serde_json::from_slice(&bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid prober frame"))
}

/// # Errors
/// Rejects responses beyond the transport budget without writing a partial frame.
pub async fn write<W: AsyncWrite + Unpin, T: Serialize>(
    writer: &mut W,
    value: &T,
) -> io::Result<()> {
    // Keep the header and body in one write. A tiny separate header can make
    // small replies wait for TCP delayed acknowledgments on reused connections.
    let mut bytes = vec![0; 4];
    serde_json::to_writer(&mut bytes, value).map_err(io::Error::other)?;
    let length = bytes.len() - 4;
    if length > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "prober frame too large",
        ));
    }
    let length = u32::try_from(length)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "prober frame too large"))?;
    bytes[..4].copy_from_slice(&length.to_be_bytes());
    writer.write_all(&bytes).await?;
    writer.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn oversized_length_is_rejected_without_waiting_for_body() {
        let (mut writer, mut reader) = tokio::io::duplex(8);
        writer.write_u32(u32::MAX).await.unwrap();
        let error = read::<_, serde_json::Value>(&mut reader).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn oversized_output_does_not_leave_a_partial_frame() {
        let mut bytes = Vec::new();
        let error = write(&mut bytes, &"x".repeat(MAX_FRAME_BYTES))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(bytes.is_empty());
    }
}
