use crate::canvas::Canvas;
use eframe::egui::{Color32, ColorImage};
use image::{ColorType, ImageEncoder, codecs::png::PngEncoder};
use serde::{Deserialize, Serialize};

use super::blobs::{StoredBlob, push_blob_raw};

const PREVIEW_MAX_EDGE: usize = 256;

#[derive(Serialize, Deserialize)]
pub(super) struct StoredPreview {
    pub width: usize,
    pub height: usize,
    pub blob: StoredBlob,
}

pub(super) fn preview_png_blob(
    canvas: &Canvas,
    blobs: &mut Vec<u8>,
) -> Result<Option<StoredPreview>, String> {
    let max_edge = canvas.width().max(canvas.height());
    if max_edge == 0 {
        return Ok(None);
    }
    let mut image = ColorImage::new([1, 1], Color32::TRANSPARENT);
    canvas.write_thumbnail_nearest(PREVIEW_MAX_EDGE, &mut image);

    let mut rgba = Vec::with_capacity(image.pixels.len() * 4);
    for px in &image.pixels {
        rgba.extend_from_slice(&px.to_srgba_unmultiplied());
    }
    let mut png = Vec::new();
    PngEncoder::new(&mut png)
        .write_image(
            &rgba,
            image.size[0] as u32,
            image.size[1] as u32,
            ColorType::Rgba8.into(),
        )
        .map_err(|err| format!("Preview encode failed: {err}"))?;

    Ok(Some(StoredPreview {
        width: image.size[0],
        height: image.size[1],
        blob: push_blob_raw(blobs, &png)?,
    }))
}
