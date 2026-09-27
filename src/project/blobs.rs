//! The project file's blob area: payloads stored zstd-compressed (in
//! parallel) or raw, addressed by offset and length.

use serde::{Deserialize, Serialize};
use std::io::Cursor;

const ZSTD_LEVEL: i32 = 6;

#[derive(Serialize, Deserialize)]
pub(super) struct StoredBlob {
    pub offset: u64,
    pub len: u64,
    pub raw_len: u64,
    #[serde(default = "default_compressed")]
    pub compressed: bool,
}

fn default_compressed() -> bool {
    true
}

/// Store `raw` uncompressed. Use for payloads that are
/// already compressed (e.g. PNG-encoded previews), where a second zstd pass
/// only burns CPU for no size benefit.
pub(super) fn push_blob_raw(blobs: &mut Vec<u8>, raw: &[u8]) -> Result<StoredBlob, String> {
    push_blob_impl(blobs, raw, false)
}

/// Store zstd-compressed payloads: compressed in parallel, stored in
/// order (the file is the same as pushing them one by one).
pub(super) fn push_blobs(blobs: &mut Vec<u8>, raws: &[Vec<u8>]) -> Result<Vec<StoredBlob>, String> {
    use rayon::prelude::*;
    // One compressor per thread, reused: making one per tile (MBs of
    // state) cost more than compressing.
    let compressed: Vec<Vec<u8>> = raws
        .par_iter()
        .map_init(
            || zstd::bulk::Compressor::new(ZSTD_LEVEL),
            |compressor, raw| match compressor {
                Ok(c) => c
                    .compress(raw)
                    .map_err(|err| format!("Compression failed: {err}")),
                Err(err) => Err(format!("Compression failed: {err}")),
            },
        )
        .collect::<Result<_, _>>()?;
    Ok(raws
        .iter()
        .zip(compressed)
        .map(|(raw, payload)| {
            let offset = blobs.len() as u64;
            blobs.extend_from_slice(&payload);
            StoredBlob {
                offset,
                len: payload.len() as u64,
                raw_len: raw.len() as u64,
                compressed: true,
            }
        })
        .collect())
}

fn push_blob_impl(blobs: &mut Vec<u8>, raw: &[u8], compress: bool) -> Result<StoredBlob, String> {
    let payload = if compress {
        zstd::stream::encode_all(Cursor::new(raw), ZSTD_LEVEL)
            .map_err(|err| format!("Compression failed: {err}"))?
    } else {
        raw.to_vec()
    };
    let offset = blobs.len() as u64;
    blobs.extend_from_slice(&payload);
    Ok(StoredBlob {
        offset,
        len: payload.len() as u64,
        raw_len: raw.len() as u64,
        compressed: compress,
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
    let raw_len = usize::try_from(blob.raw_len).map_err(|_| "Blob raw length is too large")?;
    let raw = if blob.compressed {
        // Bounded by the stored raw_len so a corrupt/malicious blob claiming a huge
        // decompressed size errors out instead of exhausting memory.
        zstd::bulk::decompress(&blobs[start..end], raw_len)
            .map_err(|err| format!("Decompression failed: {err}"))?
    } else {
        blobs[start..end].to_vec()
    };
    if raw.len() as u64 != blob.raw_len {
        return Err("Decompressed blob size mismatch".to_string());
    }
    Ok(raw)
}
