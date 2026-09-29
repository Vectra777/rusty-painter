//! Quality checks run over every filter in the menu (and a strong setting
//! of each adjustment): the same result every time, the same result when
//! only part of the picture is filtered (as a selection is: a region plus
//! the filter's [`Filter::reach`]), alpha kept by colour adjustments, and
//! nothing appearing where there was nothing unless the filter makes it.

use super::filters::{Filter, ToneCurve};
use eframe::egui::Color32;

const W: usize = 160;
const H: usize = 120;

/// Gradients, hard edges, a half-transparent band and a transparent hole.
fn picture() -> Vec<Color32> {
    (0..W * H)
        .map(|i| {
            let (x, y) = (i % W, i / W);
            if (60..90).contains(&x) && (50..70).contains(&y) {
                return Color32::TRANSPARENT;
            }
            let r = (x * 255 / W) as u8;
            let g = (y * 255 / H) as u8;
            let b = if (x / 20 + y / 20) % 2 == 0 { 220 } else { 30 };
            let a = if (20..40).contains(&y) { 128 } else { 255 };
            Color32::from_rgba_unmultiplied(r, g, b, a)
        })
        .collect()
}

/// Everything in the menu, the adjustments also at a strong setting.
fn every_filter() -> Vec<Filter> {
    let mut all: Vec<Filter> = Filter::MENU
        .iter()
        .flat_map(|g| g.iter().copied())
        .collect();
    all.extend([
        Filter::BrightnessContrast {
            brightness: 0.3,
            contrast: 0.5,
        },
        Filter::HueSaturation {
            hue: 90.0,
            saturation: 0.5,
            lightness: -0.2,
        },
        Filter::Levels {
            black: 0.1,
            white: 0.8,
            gamma: 1.4,
        },
        Filter::Curves {
            rgb: ToneCurve::from_points(&[[0.0, 0.0], [0.4, 0.7], [1.0, 1.0]]),
            red: ToneCurve::from_points(&[[0.0, 0.1], [1.0, 0.9]]),
            green: ToneCurve::default(),
            blue: ToneCurve::default(),
        },
        Filter::Exposure { stops: 1.0 },
        Filter::Temperature {
            temperature: 0.6,
            tint: -0.3,
        },
        Filter::Vibrance { amount: 0.8 },
    ]);
    all.into_iter().map(|f| f.fitted(W, H)).collect()
}

fn diff(a: Color32, b: Color32) -> i32 {
    (0..4)
        .map(|c| (a.to_array()[c] as i32 - b.to_array()[c] as i32).abs())
        .max()
        .unwrap()
}

#[test]
fn every_filter_gives_the_same_result_every_time() {
    let src = picture();
    for f in every_filter() {
        assert_eq!(
            f.apply(&src, W, H, (0, 0)),
            f.apply(&src, W, H, (0, 0)),
            "{}",
            f.name()
        );
    }
}

#[test]
fn filtering_a_region_matches_filtering_everything() {
    // A selection is filtered as its bounds plus the filter's reach: the
    // part inside must come out as if the whole picture had been.
    let src = picture();
    let (x0, y0, x1, y1) = (50usize, 30usize, 120usize, 90usize);
    for f in every_filter() {
        let whole = f.apply(&src, W, H, (0, 0));
        let r = f.reach().max(0) as usize;
        let (cx0, cy0) = (x0.saturating_sub(r), y0.saturating_sub(r));
        let (cx1, cy1) = ((x1 + r).min(W), (y1 + r).min(H));
        let (cw, ch) = (cx1 - cx0, cy1 - cy0);
        let crop: Vec<Color32> = (0..cw * ch)
            .map(|i| src[(cy0 + i / cw) * W + cx0 + i % cw])
            .collect();
        let part = f.apply(&crop, cw, ch, (cx0 as i32, cy0 as i32));
        let mut worst = (0, 0, 0);
        for y in y0..y1 {
            for x in x0..x1 {
                let d = diff(whole[y * W + x], part[(y - cy0) * cw + x - cx0]);
                if d > worst.0 {
                    worst = (d, x, y);
                }
            }
        }
        // Blurs cut off at three sigma: a level or two of difference.
        assert!(
            worst.0 <= 2,
            "{}: differs by {} at {:?}",
            f.name(),
            worst.0,
            (worst.1, worst.2)
        );
    }
}

#[test]
fn colour_adjustments_keep_alpha_and_leave_the_transparent_alone() {
    let src = picture();
    for f in every_filter() {
        // Colour-only: nothing read from neighbours, nothing generated
        // (line art turns light into transparency on purpose).
        if f.reach() != 0
            || matches!(
                f,
                Filter::Clouds { .. } | Filter::Vignette { .. } | Filter::LineArt { .. }
            )
        {
            continue;
        }
        let out = f.apply(&src, W, H, (0, 0));
        for (i, (s, o)) in src.iter().zip(&out).enumerate() {
            assert_eq!(s.a(), o.a(), "{} changed alpha at {i}", f.name());
            if s.a() == 0 {
                assert_eq!(
                    *o,
                    Color32::TRANSPARENT,
                    "{} painted a hole at {i}",
                    f.name()
                );
            }
        }
    }
}

#[test]
fn spatial_filters_do_not_paint_far_from_the_paint() {
    // A dot in the middle of an empty picture: what the filter makes of it
    // stays within its reach (the region filtering relies on this too).
    let mut src = vec![Color32::TRANSPARENT; W * H];
    for y in 58..62 {
        for x in 78..82 {
            src[y * W + x] = Color32::from_rgb(200, 60, 30);
        }
    }
    for f in every_filter() {
        if matches!(f, Filter::Clouds { .. } | Filter::Invert)
            || matches!(f, Filter::ZoomBlur { .. } | Filter::SpinBlur { .. })
        {
            continue;
        }
        let r = f.reach();
        let out = f.apply(&src, W, H, (0, 0));
        for (i, o) in out.iter().enumerate() {
            let (x, y) = ((i % W) as i32, (i / W) as i32);
            let far = x < 78 - r - 1 || x > 81 + r + 1 || y < 58 - r - 1 || y > 61 + r + 1;
            if far {
                assert_eq!(
                    o.a(),
                    0,
                    "{} painted at {:?}, beyond its reach {r}",
                    f.name(),
                    (x, y)
                );
            }
        }
    }
}
