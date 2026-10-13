//! Moved layers against the rest of the document: merging, layered
//! export, masks, onion skins, deleted parents and undo.

use crate::canvas::Canvas;
use crate::canvas::motion::{Motion, Prop};
use crate::canvas::storage::{LayerId, LayerKind};
use eframe::egui::{Color32, ColorImage};

fn pixel(canvas: &Canvas, x: usize, y: usize) -> Color32 {
    let mut img = ColorImage::new([1, 1], Color32::TRANSPARENT);
    canvas.write_region_to_color_image(x, y, 1, 1, &mut img, 1);
    img.pixels[0]
}

/// A 128×64 canvas, layer 1 red on its left tile moved 64 to the right.
fn moved() -> Canvas {
    let mut canvas = Canvas::new(128, 64, Color32::WHITE, 64);
    canvas.set_layer_tile_data(1, 0, 0, vec![Color32::RED; 64 * 64]);
    let mut motion = Motion::new([32.0, 32.0]);
    motion.set(Prop::Position, 0, [64.0, 0.0]);
    canvas.layers[1].motion = Some(Box::new(motion));
    canvas.pose_motions();
    canvas
}

#[test]
fn a_moved_layer_merges_as_it_shows() {
    let mut canvas = moved();
    let blue = canvas.insert_new_layer(1, "Under".into(), LayerKind::Paint, None);
    let under = canvas.layer_index_of(blue).unwrap();
    canvas.set_layer_tile_data(under, 0, 0, vec![Color32::BLUE; 64 * 64]);
    canvas.pose_motions();
    let top = canvas.layers.len() - 1;
    assert_eq!(pixel(&canvas, 80, 10), Color32::RED);
    assert_eq!(pixel(&canvas, 10, 10), Color32::BLUE);
    canvas.merge_down(top).unwrap();
    canvas.pose_motions();
    assert_eq!(
        pixel(&canvas, 80, 10),
        Color32::RED,
        "red stays where it showed"
    );
    assert_eq!(pixel(&canvas, 10, 10), Color32::BLUE);
    assert!(
        canvas.layers.iter().all(|l| l.motion.is_none()),
        "keys baked in"
    );
}

#[test]
fn a_moved_layer_exports_to_psd_where_it_shows() {
    let canvas = moved();
    let doc = crate::project::psd::PsdDocument::from_canvas(&canvas);
    let bytes = crate::project::psd::encode_psd(&doc).unwrap();
    let back = crate::project::psd::decode_psd(&bytes)
        .unwrap()
        .into_canvas()
        .unwrap();
    assert_eq!(pixel(&back, 80, 10), Color32::RED);
    assert_eq!(pixel(&back, 10, 10), Color32::WHITE);
}

#[test]
fn a_mask_moves_with_its_layer() {
    let mut canvas = moved();
    let owner = canvas.layers[1].id;
    let mask = canvas.insert_new_layer(2, "Mask".into(), LayerKind::Mask { owner }, None);
    let m = canvas.layer_index_of(mask).unwrap();
    // The mask hides the layer's top half (where it was painted).
    let mut tile = vec![Color32::WHITE; 64 * 64];
    for p in &mut tile[..64 * 32] {
        *p = Color32::BLACK;
    }
    canvas.set_layer_tile_data(m, 0, 0, tile);
    canvas.pose_motions();
    assert_eq!(
        pixel(&canvas, 80, 10),
        Color32::WHITE,
        "hidden, moved with it"
    );
    assert_eq!(pixel(&canvas, 80, 50), Color32::RED);
}

#[test]
fn a_layer_following_a_deleted_one_stays_put() {
    let mut canvas = moved();
    let follower = canvas.insert_new_layer(2, "F".into(), LayerKind::Paint, None);
    let f = canvas.layer_index_of(follower).unwrap();
    let mut motion = Motion::new([0.0, 0.0]);
    motion.parent = Some(canvas.layers[1].id.0);
    canvas.layers[f].motion = Some(Box::new(motion));
    assert!(canvas.is_moved(f), "follows the leader 64 along");
    canvas.layers.remove(1);
    let f = canvas.layer_index_of(follower).unwrap();
    assert!(!canvas.is_moved(f), "the leader's gone: as painted");
    assert!(canvas.layer_index_of(LayerId(9999)).is_none());
}

#[test]
fn onion_skins_of_a_moved_animated_layer_move_too() {
    let mut canvas = Canvas::new(128, 64, Color32::WHITE, 64);
    canvas.set_layer_tile_data(1, 0, 0, vec![Color32::RED; 64 * 64]);
    let track = canvas.animate_layer(1).unwrap();
    canvas.add_frame(track, 2, false).unwrap();
    let t = canvas.layer_index_of(track).unwrap();
    let mut motion = Motion::new([32.0, 32.0]);
    motion.set(Prop::Position, 0, [64.0, 0.0]);
    canvas.layers[t].motion = Some(Box::new(motion));
    canvas.onion.enabled = true;
    canvas.set_time(2);
    canvas.pose_motions();
    // The red drawing (before) shows faintly, moved with its layer.
    let ghost = pixel(&canvas, 80, 10);
    assert!(
        ghost != Color32::WHITE && ghost.r() > ghost.g(),
        "{ghost:?}"
    );
    assert_eq!(pixel(&canvas, 10, 10), Color32::WHITE);
}

/// Times posing a 4K layer moved, turned, and blurred (run with
/// `--release --ignored`).
#[test]
#[ignore = "timing"]
fn posing_timings() {
    let mut canvas = Canvas::new(3840, 2160, Color32::WHITE, 256);
    for ty in 0..9 {
        for tx in 0..15 {
            canvas.set_layer_tile_data(1, tx, ty, vec![Color32::from_rgb(200, 40, 40); 256 * 256]);
        }
    }
    let mut time = |name: &str, edit: &dyn Fn(&mut Motion)| {
        let mut motion = Motion::new([1920.0, 1080.0]);
        edit(&mut motion);
        canvas.layers[1].motion = Some(Box::new(motion));
        let start = std::time::Instant::now();
        canvas.pose_motions();
        eprintln!("{name}: {:?}", start.elapsed());
    };
    time("moved", &|m| m.set(Prop::Position, 0, [100.0, 50.0]));
    time("turned and scaled", &|m| {
        m.set(Prop::Rotation, 0, [30.0, 0.0]);
        m.set(Prop::Scale, 0, [0.8, 0.8]);
    });
    time("tinted", &|m| m.set(Prop::Tint, 0, [0.5, 0.0]));
    time("blurred 10", &|m| m.set(Prop::Blur, 0, [10.0, 0.0]));
    time("blurred 60", &|m| m.set(Prop::Blur, 0, [60.0, 0.0]));
}

#[test]
fn extreme_or_broken_values_show_something_or_nothing_without_failing() {
    for (p, v) in [
        (Prop::Scale, [1e30, 1e30]),
        (Prop::Scale, [0.0, 0.0]),
        (Prop::Scale, [1e-6, 1e-6]),
        (Prop::Position, [f32::NAN, 3.0]),
        (Prop::Position, [1e12, -1e12]),
        (Prop::Rotation, [f32::INFINITY, 0.0]),
        (Prop::Anchor, [f32::NAN, f32::NAN]),
        (Prop::Blur, [1e9, 0.0]),
        (Prop::Hue, [f32::NAN, 0.0]),
        (Prop::Opacity, [-5.0, 0.0]),
    ] {
        let mut canvas = Canvas::new(128, 64, Color32::WHITE, 64);
        canvas.set_layer_tile_data(1, 0, 0, vec![Color32::RED; 64 * 64]);
        let mut motion = Motion::new([32.0, 32.0]);
        motion.set(p, 0, v);
        canvas.layers[1].motion = Some(Box::new(motion));
        canvas.pose_motions();
        let _ = pixel(&canvas, 10, 10);
        let _ = canvas.flatten_final();
    }
}
