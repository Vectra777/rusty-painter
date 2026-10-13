use super::*;

fn solid(c: Color32, n: usize) -> Vec<Color32> {
    vec![c; n]
}

#[test]
fn neutral_settings_change_nothing() {
    let px = [
        Color32::from_rgb(200, 40, 90),
        Color32::from_rgba_unmultiplied(10, 250, 128, 77),
        Color32::TRANSPARENT,
    ];
    for group in Filter::MENU {
        for f in group.iter() {
            let neutral = match f {
                Filter::BrightnessContrast { .. }
                | Filter::HueSaturation { .. }
                | Filter::Levels { .. } => *f,
                Filter::GaussianBlur { .. } => Filter::GaussianBlur { radius: 0.0 },
                Filter::Noise { mono, size, .. } => Filter::Noise {
                    amount: 0.0,
                    mono: *mono,
                    size: *size,
                },
                Filter::Pixelate { .. } => Filter::Pixelate { size: 1 },
                Filter::Curves { .. }
                | Filter::ColourBalance { .. }
                | Filter::Exposure { .. }
                | Filter::Temperature { .. }
                | Filter::Vibrance { .. } => *f,
                Filter::Sepia { .. } => Filter::Sepia { amount: 0.0 },
                Filter::Solarize { .. } => Filter::Solarize { level: 1.0 },
                Filter::Median { .. } | Filter::OilPaint { .. } => continue,
                Filter::Glow { radius, .. } => Filter::Glow {
                    radius: *radius,
                    strength: 0.0,
                    threshold: 0.5,
                },
                Filter::Vignette { size, .. } => Filter::Vignette {
                    amount: 0.0,
                    size: *size,
                    frame: crate::canvas::effects::Frame::of_canvas(3, 1),
                },
                Filter::ZoomBlur { .. } => Filter::ZoomBlur {
                    amount: 0.0,
                    frame: crate::canvas::effects::Frame::of_canvas(3, 1),
                },
                Filter::SpinBlur { .. } => Filter::SpinBlur {
                    angle: 0.0,
                    frame: crate::canvas::effects::Frame::of_canvas(3, 1),
                },
                Filter::ChromaticAberration { .. } => Filter::ChromaticAberration {
                    amount: 0.0,
                    frame: crate::canvas::effects::Frame::of_canvas(3, 1),
                },
                _ => continue,
            };
            let out = neutral.apply(&px, 3, 1, (0, 0));
            for (a, b) in px.iter().zip(&out) {
                let d = a
                    .to_array()
                    .iter()
                    .zip(b.to_array())
                    .map(|(x, y)| x.abs_diff(y))
                    .max()
                    .unwrap();
                assert!(d <= 2, "{}: {a:?} -> {b:?}", f.name());
            }
        }
    }
}

fn rgb_of(f: Filter, c: Color32) -> [u8; 3] {
    let out = f.apply(&[c], 1, 1, (0, 0))[0].unmultiplied();
    [out[0], out[1], out[2]]
}

#[test]
fn curves_lift_what_their_curve_lifts() {
    let lift = ToneCurve::from_points(&[[0.0, 0.0], [0.5, 0.75], [1.0, 1.0]]);
    let grey = Color32::from_rgb(128, 128, 128);
    let master = Filter::Curves {
        rgb: lift,
        red: ToneCurve::default(),
        green: ToneCurve::default(),
        blue: ToneCurve::default(),
    };
    let [r, g, b] = rgb_of(master, grey);
    assert!(r > 180 && r == g && g == b, "{r} {g} {b}");
    // A red curve changes red only.
    let red = Filter::Curves {
        rgb: ToneCurve::default(),
        red: lift,
        green: ToneCurve::default(),
        blue: ToneCurve::default(),
    };
    let [r, g, b] = rgb_of(red, grey);
    assert!(r > 180 && g == 128 && b == 128, "{r} {g} {b}");
    // Its table and the maths agree (the table rounds between curves).
    let lut = red.channel_lut().expect("per channel");
    for v in (0..=255u8).step_by(5) {
        let c = Color32::from_rgb(v, 255 - v, v / 2);
        let fast = red.adjust(c, Some(&lut)).to_array();
        let slow = red.pixel(c).to_array();
        for (x, y) in fast.iter().zip(slow) {
            assert!(x.abs_diff(y) <= 1, "{fast:?} vs {slow:?}");
        }
    }
    // An identity curve is known as one.
    assert!(ToneCurve::default().is_identity() && !lift.is_identity());
}

#[test]
fn a_tone_curve_keeps_its_points_in_order_and_at_most_its_size() {
    let points: Vec<[f32; 2]> = (0..40).rev().map(|i| [i as f32 / 40.0, 0.5]).collect();
    let curve = ToneCurve::from_points(&points);
    assert_eq!(curve.points().len(), CURVE_POINTS);
    assert!(curve.points().windows(2).all(|p| p[0][0] <= p[1][0]));
}

#[test]
fn colour_balance_shifts_the_range_it_is_told_to() {
    let grey = Color32::from_rgb(128, 128, 128);
    let warm_mids = |preserve| Filter::ColourBalance {
        shadows: [0.0; 3],
        midtones: [0.5, 0.0, -0.5],
        highlights: [0.0; 3],
        preserve_luminosity: preserve,
    };
    let [r, _, b] = rgb_of(warm_mids(false), grey);
    assert!(r > 180 && b < 80, "{r} {b}");
    // Midtones leave black and white alone.
    assert_eq!(rgb_of(warm_mids(false), Color32::BLACK), [0, 0, 0]);
    assert_eq!(rgb_of(warm_mids(false), Color32::WHITE), [255, 255, 255]);
    // Preserving luminosity keeps the grey's lightness.
    let out = rgb_of(warm_mids(true), grey);
    let lightness = (*out.iter().max().unwrap() as f32 + *out.iter().min().unwrap() as f32) / 2.0;
    assert!((lightness - 128.0).abs() <= 1.5, "{out:?}");
    assert!(out[0] > out[2], "still warmer: {out:?}");
    // Shadows move dark pixels, not light ones.
    let blue_shadows = Filter::ColourBalance {
        shadows: [0.0, 0.0, 0.6],
        midtones: [0.0; 3],
        highlights: [0.0; 3],
        preserve_luminosity: false,
    };
    assert!(rgb_of(blue_shadows, Color32::from_rgb(20, 20, 20))[2] > 100);
    assert_eq!(rgb_of(blue_shadows, Color32::WHITE), [255, 255, 255]);
}

#[test]
fn a_gradient_map_colours_by_brightness_and_keeps_transparency() {
    let map = GradientMap::from_stops(&[(0.0, [200, 0, 0]), (1.0, [0, 0, 200])]);
    let f = Filter::GradientMap(map);
    assert_eq!(rgb_of(f, Color32::BLACK), [200, 0, 0]);
    assert_eq!(rgb_of(f, Color32::WHITE), [0, 0, 200]);
    let [r, g, b] = rgb_of(f, Color32::from_rgb(128, 128, 128));
    assert!(
        r.abs_diff(100) <= 2 && g == 0 && b.abs_diff(100) <= 2,
        "{r} {g} {b}"
    );
    let half = Color32::from_rgba_unmultiplied(255, 255, 255, 100);
    assert_eq!(f.apply(&[half], 1, 1, (0, 0))[0].a(), 100);
    // Reversed, the ends swap.
    assert_eq!(
        rgb_of(Filter::GradientMap(map.reversed()), Color32::BLACK),
        [0, 0, 200]
    );
    // The default is black to white: a grey stays grey.
    let [r, g, b] = rgb_of(
        Filter::GradientMap(GradientMap::default()),
        Color32::from_rgb(90, 90, 90),
    );
    assert!(r.abs_diff(90) <= 1 && r == g && g == b);
}

#[test]
fn the_new_colour_adjustments_do_what_they_say() {
    let grey = Color32::from_rgb(128, 128, 128);
    let brighter = rgb_of(Filter::Exposure { stops: 1.0 }, grey);
    assert!(
        brighter[0] > 170 && brighter[0] < 185,
        "one stop: twice the light {brighter:?}"
    );
    let warm = rgb_of(
        Filter::Temperature {
            temperature: 1.0,
            tint: 0.0,
        },
        grey,
    );
    assert!(warm[0] > 140 && warm[2] < 110, "{warm:?}");
    let dull = Color32::from_rgb(140, 120, 110);
    let rich = Color32::from_rgb(230, 20, 20);
    let sat = |c: [u8; 3]| c.iter().max().unwrap() - c.iter().min().unwrap();
    let v = |c| rgb_of(Filter::Vibrance { amount: 1.0 }, c);
    let (d0, d1) = (sat([140, 120, 110]), sat(v(dull)));
    let (r0, r1) = (sat([230, 20, 20]), sat(v(rich)));
    assert!(
        d1 as f32 / d0 as f32 > r1 as f32 / r0 as f32,
        "dull colours gain most"
    );
    let sepia = rgb_of(
        Filter::Sepia { amount: 1.0 },
        Color32::from_rgb(40, 90, 200),
    );
    assert!(
        sepia[0] > sepia[1] && sepia[1] > sepia[2],
        "brown: {sepia:?}"
    );
    assert_eq!(
        rgb_of(Filter::Solarize { level: 0.5 }, Color32::WHITE),
        [0, 0, 0]
    );
    assert_eq!(
        rgb_of(Filter::Solarize { level: 0.5 }, Color32::from_gray(60)),
        [60; 3]
    );
    // The per-channel ones have tables that agree with the maths.
    for f in [
        Filter::Exposure { stops: -1.3 },
        Filter::Temperature {
            temperature: -0.6,
            tint: 0.4,
        },
        Filter::Solarize { level: 0.3 },
    ] {
        let lut = f.channel_lut().expect("per channel");
        for v in (0..=255u8).step_by(3) {
            let c = Color32::from_rgb(v, 255 - v, v / 3);
            assert_eq!(f.adjust(c, Some(&lut)), f.pixel(c), "{}", f.name());
        }
    }
    // All of them can be adjustment layers.
    for f in [
        Filter::Exposure { stops: 0.0 },
        Filter::Vibrance { amount: 0.0 },
        Filter::Sepia { amount: 1.0 },
    ] {
        assert!(Filter::ADJUSTMENTS.iter().any(|a| a.name() == f.name()));
    }
}

#[test]
fn every_filter_keeps_a_transparent_pixel_transparent_or_says_why() {
    // Only clouds (which paint over everything) and glow (whose light
    // spreads) may fill transparent pixels.
    let clear = vec![Color32::TRANSPARENT; 9];
    for group in Filter::MENU {
        for f in group.iter() {
            let f = f.fitted(3, 3);
            let out = f.apply(&clear, 3, 3, (0, 0));
            let spreads = matches!(f, Filter::Clouds { .. } | Filter::Glow { .. });
            if !spreads {
                assert!(out.iter().all(|c| c.a() == 0), "{}", f.name());
            }
            assert_eq!(out.len(), 9, "{}", f.name());
        }
    }
}

#[test]
fn new_filters_round_trip_as_project_json() {
    for f in [
        Filter::Curves {
            rgb: ToneCurve::from_points(&[[0.0, 0.1], [0.4, 0.6], [1.0, 0.9]]),
            red: ToneCurve::default(),
            green: ToneCurve::default(),
            blue: ToneCurve::from_points(&[[0.0, 1.0], [1.0, 0.0]]),
        },
        Filter::ColourBalance {
            shadows: [0.1, -0.2, 0.3],
            midtones: [0.0; 3],
            highlights: [-0.5, 0.0, 0.5],
            preserve_luminosity: false,
        },
        Filter::GradientMap(GradientMap::from_stops(GradientMap::PRESETS[2].1)),
    ] {
        let json = serde_json::to_string(&Some(f)).unwrap();
        let back: Option<Filter> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, Some(f));
        assert!(Filter::ADJUSTMENTS.iter().any(|a| a.name() == f.name()));
    }
}

#[test]
fn invert_and_desaturate_do_what_they_say() {
    let out = Filter::Invert.apply(&[Color32::from_rgb(255, 0, 100)], 1, 1, (0, 0));
    assert_eq!(out[0], Color32::from_rgb(0, 255, 155));
    let out = Filter::Desaturate.apply(&[Color32::from_rgb(255, 0, 0)], 1, 1, (0, 0));
    let [r, g, b, _] = out[0].to_array();
    assert!(r == g && g == b && (74..=78).contains(&r), "{:?}", out[0]);
}

#[test]
fn hue_turns_red_to_green_and_back() {
    let red = Color32::from_rgb(255, 0, 0);
    let f = |hue| Filter::HueSaturation {
        hue,
        saturation: 0.0,
        lightness: 0.0,
    };
    assert_eq!(
        f(120.0).apply(&[red], 1, 1, (0, 0))[0],
        Color32::from_rgb(0, 255, 0)
    );
    assert_eq!(
        f(-120.0).apply(&[red], 1, 1, (0, 0))[0],
        Color32::from_rgb(0, 0, 255)
    );
}

#[test]
fn blur_spreads_a_dot_and_keeps_the_total() {
    let (w, h) = (41, 41);
    let mut src = solid(Color32::TRANSPARENT, w * h);
    src[20 * w + 20] = Color32::WHITE;
    let out = Filter::GaussianBlur { radius: 2.0 }.apply(&src, w, h, (0, 0));
    assert!(out[20 * w + 20].a() < 255, "the centre fades");
    assert!(out[20 * w + 22].a() > 0, "its neighbours gain");
    assert_eq!(out[20 * w + 38].a(), 0, "far pixels stay empty");
    // Box blurs round each pass; the total alpha stays about the same.
    let total: u32 = out.iter().map(|c| c.a() as u32).sum();
    assert!((200..=300).contains(&total), "{total}");
}

#[test]
fn blur_leaves_a_flat_colour_flat() {
    let c = Color32::from_rgb(30, 120, 200);
    let out = Filter::GaussianBlur { radius: 5.0 }.apply(&solid(c, 30 * 20), 30, 20, (0, 0));
    assert!(out.iter().all(|&p| p == c));
}

#[test]
fn motion_blur_smears_along_its_angle_only() {
    let (w, h) = (41, 41);
    let mut src = solid(Color32::TRANSPARENT, w * h);
    src[20 * w + 20] = Color32::WHITE;
    let out = Filter::MotionBlur {
        angle: 0.0,
        distance: 10.0,
    }
    .apply(&src, w, h, (0, 0));
    assert!(out[20 * w + 24].a() > 0, "along the line");
    assert_eq!(out[24 * w + 20].a(), 0, "not across it");
}

#[test]
fn sharpen_raises_contrast_at_an_edge() {
    let (w, h) = (20, 1);
    let src: Vec<Color32> = (0..w)
        .map(|x| {
            if x < 10 {
                Color32::from_gray(100)
            } else {
                Color32::from_gray(150)
            }
        })
        .collect();
    let out = Filter::Sharpen {
        radius: 1.5,
        amount: 1.0,
    }
    .apply(&src, w, h, (0, 0));
    assert!(
        out[9].r() < 100 && out[10].r() > 150,
        "{:?} {:?}",
        out[9],
        out[10]
    );
    assert_eq!(out[0], src[0], "flat areas are unchanged");
}

/// A soft white line on a clear layer, blurred: its faded edge must stay
/// white, not turn grey (stored pixels are premultiplied in linear
/// light, so averaging them directly darkens).
#[test]
fn blurs_keep_the_colour_of_soft_edges() {
    let (w, h) = (32, 8);
    let src: Vec<Color32> = (0..w * h)
        .map(|i| {
            let x = i % w;
            if (12..20).contains(&x) {
                Color32::WHITE
            } else {
                Color32::TRANSPARENT
            }
        })
        .collect();
    for f in [
        Filter::GaussianBlur { radius: 3.0 },
        Filter::MotionBlur {
            angle: 0.0,
            distance: 10.0,
        },
        Filter::Pixelate { size: 5 },
        Filter::ZoomBlur {
            amount: 0.3,
            frame: Frame::of_canvas(w, h),
        },
    ] {
        let out = f.apply(&src, w, h, (0, 0));
        for c in out.iter().filter(|c| c.a() > 8) {
            let [r, g, b, _] = c.unmultiplied();
            assert!(
                r.min(g).min(b) >= 245,
                "{}: faded edge turned {:?}",
                f.name(),
                c.unmultiplied()
            );
        }
    }
}

#[test]
fn mixable_round_trip_leaves_untouched_pixels_exact() {
    let src: Vec<Color32> = (0..=255u8)
        .map(|a| Color32::from_rgba_unmultiplied(200, 90, 30, a))
        .collect();
    let mixed = to_mixable(&src);
    assert_eq!(from_mixable(&mixed, &mixed, &src), src);
    // And a changed buffer decodes to the colour it holds.
    let moved: Vec<Color32> = mixed.iter().rev().copied().collect();
    let back = from_mixable(&moved, &mixed, &src);
    for c in back.iter().filter(|c| c.a() > 40) {
        let [r, g, b, _] = c.unmultiplied();
        assert!(
            r.abs_diff(200) <= 4 && g.abs_diff(90) <= 4 && b.abs_diff(30) <= 4,
            "{c:?}"
        );
    }
}

#[test]
fn bigger_noise_grain_changes_slowly_across_pixels() {
    let src = solid(Color32::from_gray(128), 64 * 64);
    let roughness = |size: f32| {
        let f = Filter::Noise {
            amount: 0.5,
            mono: true,
            size,
        };
        let out = f.apply(&src, 64, 64, (0, 0));
        out.windows(2)
            .map(|p| p[0].r().abs_diff(p[1].r()) as u32)
            .sum::<u32>()
    };
    assert!(roughness(8.0) * 3 < roughness(1.0));
    assert!(roughness(8.0) > 0);
}

#[test]
fn noise_is_repeatable_and_stays_in_range() {
    let src = solid(Color32::from_rgba_unmultiplied(128, 128, 128, 128), 64);
    let f = Filter::Noise {
        amount: 0.5,
        mono: false,
        size: 1.0,
    };
    let a = f.apply(&src, 8, 8, (3, 5));
    assert_eq!(a, f.apply(&src, 8, 8, (3, 5)));
    assert!(a.iter().all(|c| c.a() == 128));
    assert!(a.iter().any(|&c| c != src[0]));
}

#[test]
fn pixelate_blocks_follow_the_canvas_grid() {
    let (w, h) = (8, 4);
    let src: Vec<Color32> = (0..w * h)
        .map(|i| Color32::from_gray((i % w) as u8 * 30))
        .collect();
    // The buffer starts at canvas x = 2: blocks of 4 end at buffer x = 2, 6.
    let out = Filter::Pixelate { size: 4 }.apply(&src, w, h, (2, 0));
    assert_eq!(out[0], out[1]);
    assert_ne!(out[1], out[2]);
    assert_eq!(out[2], out[5]);
    assert_ne!(out[5], out[6]);
}

#[test]
fn line_art_keeps_the_dark_lines_and_drops_the_paper() {
    let src = [
        Color32::from_gray(20),
        Color32::from_gray(245),
        Color32::from_gray(140),
    ];
    let out = Filter::LineArt {
        black: 0.25,
        white: 0.85,
        keep_color: false,
    }
    .apply(&src, 3, 1, (0, 0));
    assert_eq!(out[0], Color32::BLACK, "ink stays, black");
    assert_eq!(out[1].a(), 0, "paper goes");
    assert!(
        (50..200).contains(&out[2].a()),
        "grey is half-opaque: {:?}",
        out[2]
    );
}

#[test]
fn a_channel_table_gives_the_same_pixels_as_the_maths() {
    let px: Vec<Color32> = (0..4096u32)
        .map(|i| {
            Color32::from_rgba_unmultiplied(
                (i * 7) as u8,
                (i * 13) as u8,
                (i * 29) as u8,
                (i % 256) as u8,
            )
        })
        .collect();
    for f in [
        Filter::BrightnessContrast {
            brightness: 0.2,
            contrast: 0.4,
        },
        Filter::Levels {
            black: 0.1,
            white: 0.8,
            gamma: 1.7,
        },
        Filter::Invert,
        Filter::Posterize { levels: 5 },
    ] {
        let lut = f.channel_lut().expect("per-channel");
        for &c in &px {
            assert_eq!(f.adjust(c, Some(&lut)), f.pixel(c), "{}: {c:?}", f.name());
        }
    }
    assert!(Filter::Desaturate.channel_lut().is_none(), "mixes channels");
    // On float values (gamma compositing) they agree with the 8-bit path.
    for f in [
        Filter::Levels {
            black: 0.1,
            white: 0.8,
            gamma: 1.7,
        },
        Filter::HueSaturation {
            hue: 50.0,
            saturation: 0.3,
            lightness: -0.1,
        },
        Filter::Threshold { level: 0.4 },
    ] {
        let lut = f.channel_lut();
        for &c in &px[..512] {
            if c.a() == 0 {
                continue;
            }
            let [r, g, b, _] = c.unmultiplied();
            let rgb = [r, g, b].map(|v| v as f32 / 255.0);
            let fast = f
                .adjust_rgb(rgb, lut.as_deref())
                .map(|v| (v * 255.0).round() as u8);
            let [er, eg, eb, _] = f.pixel(Color32::from_rgb(r, g, b)).to_array();
            for (x, y) in fast.iter().zip([er, eg, eb]) {
                assert!(
                    x.abs_diff(y) <= 1,
                    "{}: {fast:?} vs {:?}",
                    f.name(),
                    [er, eg, eb]
                );
            }
        }
    }
}

#[test]
fn box_radii_grow_with_sigma() {
    let small: usize = box_radii(1.0).iter().sum();
    let large: usize = box_radii(10.0).iter().sum();
    assert!(small < large);
}
