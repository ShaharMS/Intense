use crate::TileFrameHeader;
use thiserror::Error;

const MAGIC: &[u8; 4] = b"RIVR";

#[derive(Debug, Error)]
pub enum FrameError {
    #[error("tile frame is truncated")]
    Truncated,
    #[error("tile frame magic is invalid")]
    InvalidMagic,
    #[error("tile frame header is invalid: {0}")]
    InvalidHeader(#[from] serde_json::Error),
}

/// Binary WebSocket frame layout: `RIVR | u32le header length | JSON header | WebP`.
pub fn encode_tile_frame(header: &TileFrameHeader, webp: &[u8]) -> Result<Vec<u8>, FrameError> {
    let header_bytes = serde_json::to_vec(header)?;
    let mut frame = Vec::with_capacity(MAGIC.len() + 4 + header_bytes.len() + webp.len());
    frame.extend_from_slice(MAGIC);
    frame.extend_from_slice(&(header_bytes.len() as u32).to_le_bytes());
    frame.extend_from_slice(&header_bytes);
    frame.extend_from_slice(webp);
    Ok(frame)
}

pub fn decode_tile_frame(frame: &[u8]) -> Result<(TileFrameHeader, &[u8]), FrameError> {
    if frame.len() < 8 {
        return Err(FrameError::Truncated);
    }
    if &frame[..4] != MAGIC {
        return Err(FrameError::InvalidMagic);
    }
    let header_length =
        u32::from_le_bytes(frame[4..8].try_into().expect("fixed-length slice")) as usize;
    let header_end = 8usize
        .checked_add(header_length)
        .ok_or(FrameError::Truncated)?;
    let header_bytes = frame.get(8..header_end).ok_or(FrameError::Truncated)?;
    let header = serde_json::from_slice(header_bytes)?;
    Ok((header, &frame[header_end..]))
}
