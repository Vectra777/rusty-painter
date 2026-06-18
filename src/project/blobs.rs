use serde::{Deserialize, Serialize};
use std::io::Cursor;

const ZSTD_LEVEL: i32 = 6;

#[derive(Serialize, Deserialize)]
pub(super) struct StoredBlob {
    pub offset: u64,
    pub len: u64,
    pub raw_len: u64,
}

pub(super) fn push_blob(blobs: &mut Vec<u8>, raw: &[u8]) -> Result<StoredBlob, String> {
    let compressed = zstd::stream::encode_all(Cursor::new(raw), ZSTD_LEVEL)
        .map_err(|err| format!("Compression failed: {err}"))?;
    let offset = blobs.len() as u64;
    blobs.extend_from_slice(&compressed);
    Ok(StoredBlob {
        offset,
        len: compressed.len() as u64,
        raw_len: raw.len() as u64,
    })
}

pub(super) fn read_blob(blobs: &[u8], blob: &StoredBlob) -> Result<Vec<u8>, String> {
    let start = usize::try_from(blob.offset).map_err(|_| "Blob offset is too large")?;
    let len = usize::try_from(blob.len).map_err(|_| "Blob length is too large")?;
    let end = start
        .checked_add(len)
        .ok_or_else(|| "Blob range overflow".to_string())?;
    if end > blobs.len() {
        return Err("Project blob is out of range".to_string());
    }
    let raw = zstd::stream::decode_all(Cursor::new(&blobs[start..end]))
        .map_err(|err| format!("Decompression failed: {err}"))?;
    if raw.len() as u64 != blob.raw_len {
        return Err("Decompressed blob size mismatch".to_string());
    }
    Ok(raw)
}
