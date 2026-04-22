//! Native Messaging Host frame codec.
//!
//! Both Chrome and Firefox speak the same protocol on the bridge's
//! stdio: a 4-byte **native-endian** `u32` length header, followed by
//! that many bytes of UTF-8 JSON. On every supported platform
//! (x86-64, aarch64) native endianness is little-endian, but the spec
//! says native — use `u32::to_ne_bytes` / `u32::from_ne_bytes`.
//!
//! Chromium caps host→browser frames at 1 MiB. Browser→host is much
//! larger (documented up to 64 MiB) but we don't need that — our
//! biggest legitimate payload is a signal report, capped at ~300 KiB
//! in practice.

use anyhow::{anyhow, Context, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Hard cap for any frame we read or write. Matches Chromium's
/// host→browser limit on the write side; on the read side we apply
/// the same limit as a DoS guard against a misbehaving peer.
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Result<serde_json::Value> {
    let mut len_buf = [0u8; 4];
    reader
        .read_exact(&mut len_buf)
        .await
        .context("reading NMH frame length")?;
    let len = u32::from_ne_bytes(len_buf) as usize;
    if len == 0 {
        return Err(anyhow!("NMH frame with zero length"));
    }
    if len > MAX_FRAME_BYTES {
        return Err(anyhow!(
            "NMH frame length {} exceeds limit {}",
            len,
            MAX_FRAME_BYTES
        ));
    }
    let mut body = vec![0u8; len];
    reader
        .read_exact(&mut body)
        .await
        .context("reading NMH frame body")?;
    serde_json::from_slice(&body).context("parsing NMH frame as JSON")
}

pub async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    value: &serde_json::Value,
) -> Result<()> {
    let body = serde_json::to_vec(value).context("serializing NMH frame")?;
    if body.len() > MAX_FRAME_BYTES {
        return Err(anyhow!(
            "NMH frame length {} exceeds limit {}",
            body.len(),
            MAX_FRAME_BYTES
        ));
    }
    let len = (body.len() as u32).to_ne_bytes();
    writer.write_all(&len).await.context("writing NMH length")?;
    writer.write_all(&body).await.context("writing NMH body")?;
    writer.flush().await.context("flushing NMH frame")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::BufWriter;

    #[tokio::test]
    async fn round_trip_preserves_value() {
        let original = serde_json::json!({ "kind": "ping", "nested": [1, 2, 3] });

        let mut buf = Vec::new();
        {
            let mut writer = BufWriter::new(&mut buf);
            write_frame(&mut writer, &original).await.unwrap();
            writer.flush().await.unwrap();
        }

        let mut cursor = std::io::Cursor::new(buf);
        let decoded = read_frame(&mut cursor).await.unwrap();
        assert_eq!(decoded, original);
    }

    #[tokio::test]
    async fn rejects_oversize_frame() {
        // Craft a header claiming 2 MiB, then 16 bytes of garbage.
        let mut buf = Vec::new();
        let fake_len = ((MAX_FRAME_BYTES + 1) as u32).to_ne_bytes();
        buf.extend_from_slice(&fake_len);
        buf.extend_from_slice(&[0u8; 16]);
        let mut cursor = std::io::Cursor::new(buf);
        let err = read_frame(&mut cursor).await.unwrap_err();
        assert!(err.to_string().contains("exceeds limit"));
    }

    #[tokio::test]
    async fn zero_length_is_rejected() {
        let buf = [0u8; 4];
        let mut cursor = std::io::Cursor::new(buf.to_vec());
        let err = read_frame(&mut cursor).await.unwrap_err();
        assert!(err.to_string().contains("zero length"));
    }
}
