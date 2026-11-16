//! The project file's blob area: payloads stored zstd-compressed (in
//! parallel) or raw, addressed by offset and length.

use serde::{Deserialize, Serialize};

const ZSTD_LEVEL: i32 = 6;

/// The most one blob can hold: a selection mask over the largest canvas.
const MAX_BLOB: u64 = crate::app::document::MAX_CANVAS_PIXELS as u64;

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

/// A payload for [`push_payloads`]: raw bytes to compress, or a zstd frame
/// made already (an undo snapshot kept compressed) with its raw length.
pub(super) enum Payload<'a> {
    Raw(&'a [u8]),
    Packed(&'a [u8], usize),
}

/// Store zstd-compressed payloads: compressed in parallel, stored in
/// order (the file is the same as pushing them one by one).
pub(super) fn push_blobs(blobs: &mut Vec<u8>, raws: &[Vec<u8>]) -> Result<Vec<StoredBlob>, String> {
    let payloads: Vec<Payload<'_>> = raws.iter().map(|r| Payload::Raw(r)).collect();
    push_payloads(blobs, &payloads)
}

/// [`push_blobs`], with frames compressed already stored as they are
/// (recompressing a long undo history was most of a save).
pub(super) fn push_payloads(
    blobs: &mut Vec<u8>,
    payloads: &[Payload<'_>],
) -> Result<Vec<StoredBlob>, String> {
    use rayon::prelude::*;
    use std::borrow::Cow;
    // One compressor per thread, reused: making one per tile (MBs of
    // state) cost more than compressing.
    let compressed: Vec<(Cow<'_, [u8]>, usize)> = payloads
        .par_iter()
        .map_init(
            || zstd::bulk::Compressor::new(ZSTD_LEVEL),
            |compressor, payload| match (payload, compressor) {
                (Payload::Packed(frame, raw_len), _) => Ok((Cow::Borrowed(*frame), *raw_len)),
                (Payload::Raw(raw), Ok(c)) => c
                    .compress(raw)
                    .map(|frame| (Cow::Owned(frame), raw.len()))
                    .map_err(|err| format!("Compression failed: {err}")),
                (Payload::Raw(_), Err(err)) => Err(format!("Compression failed: {err}")),
            },
        )
        .collect::<Result<_, _>>()?;
    Ok(compressed
        .into_iter()
        .map(|(payload, raw_len)| {
            let offset = blobs.len() as u64;
            blobs.extend_from_slice(&payload);
            StoredBlob {
                offset,
                len: payload.len() as u64,
                raw_len: raw_len as u64,
                compressed: true,
            }
        })
        .collect())
}

/// Where `blob` is in `blobs`.
fn range(blobs: &[u8], blob: &StoredBlob) -> Result<(usize, usize), String> {
    let start = usize::try_from(blob.offset).map_err(|_| "Blob offset is too large")?;
    let len = usize::try_from(blob.len).map_err(|_| "Blob length is too large")?;
    let end = start
        .checked_add(len)
        .ok_or_else(|| "Blob range overflow".to_string())?;
    if end > blobs.len() {
        return Err("Project blob is out of range".to_string());
    }
    Ok((start, end))
}

/// A compressed blob's zstd payload as it is, for keeping it compressed
/// (undo history), when it holds `raw_len` bytes; `None` for a raw blob or
/// one of another size.
pub(super) fn packed_blob<'a>(
    blobs: &'a [u8],
    blob: &StoredBlob,
    raw_len: usize,
) -> Result<Option<&'a [u8]>, String> {
    let (start, end) = range(blobs, blob)?;
    let payload = &blobs[start..end];
    let frame_len = zstd::zstd_safe::get_frame_content_size(payload);
    Ok((blob.compressed
        && blob.raw_len == raw_len as u64
        && matches!(frame_len, Ok(Some(n)) if n == blob.raw_len))
    .then_some(payload))
}

pub(super) fn read_blob(blobs: &[u8], blob: &StoredBlob) -> Result<Vec<u8>, String> {
    let (start, end) = range(blobs, blob)?;
    let raw_len = usize::try_from(blob.raw_len)
        .ok()
        .filter(|_| blob.raw_len <= MAX_BLOB)
        .ok_or("Blob raw length is too large")?;
    let raw = if blob.compressed {
        let payload = &blobs[start..end];
        // The length is the file's word, and room for it is made before
        // decompressing: a damaged one claiming gigabytes must not get
        // them. zstd's own frame says how long it really is.
        match zstd::zstd_safe::get_frame_content_size(payload) {
            Ok(Some(n)) if n != blob.raw_len => {
                return Err("Decompressed blob size mismatch".to_string());
            }
            Err(_) => return Err("Damaged compressed blob".to_string()),
            _ => {}
        }
        zstd::bulk::decompress(payload, raw_len)
            .map_err(|err| format!("Decompression failed: {err}"))?
    } else {
        blobs[start..end].to_vec()
    };
    if raw.len() as u64 != blob.raw_len {
        return Err("Decompressed blob size mismatch".to_string());
    }
    Ok(raw)
}
