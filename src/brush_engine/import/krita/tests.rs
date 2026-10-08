use super::*;
use crate::canvas::blend_modes::LayerBlend;

/// A preset PNG with `xml` in its `preset` chunk.
pub fn kpp(xml: &str) -> Vec<u8> {
    let mut png_bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png_bytes, 2, 2);
        encoder.set_color(png::ColorType::Rgba);
        encoder
            .add_ztxt_chunk("version".into(), "5.0".into())
            .unwrap();
        encoder.add_ztxt_chunk("preset".into(), xml.into()).unwrap();
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[0; 16]).unwrap();
    }
    png_bytes
}

const AUTO: &str = r#"<Preset paintopid="paintbrush" name="Soft Ink" embedded_resources="0">
    <param type="string" name="brush_definition"><![CDATA[<Brush type="auto_brush" spacing="0.08" angle="0.5">
        <MaskGenerator diameter="36" ratio="0.5" hfade="0.25" vfade="0.25" type="circle"/></Brush>]]></param>
    <param type="string" name="PressureSize"><![CDATA[true]]></param>
    <param type="string" name="SizeSensor"><![CDATA[<!DOCTYPE params><params id="pressure"><curve>0,0;0.5,0.2;1,1;</curve></params>]]></param>
    <param type="string" name="OpacityValue"><![CDATA[0.7]]></param>
    </Preset>"#;

#[test]
fn a_round_krita_preset_brings_its_settings() {
    let imported = import_kpp(&kpp(AUTO), "file").unwrap();
    let p = &imported.presets[0];
    assert_eq!(p.name, "Soft Ink");
    let o = &p.brush.brush_options;
    assert_eq!(o.diameter, 36.0);
    assert!((o.spacing - 8.0).abs() < 1e-3);
    // Fade 0.25: solid to a quarter of the way out, then the
    // falloff on the squared distance, `1 - (n - f²) / (1 - f²)`.
    assert_eq!(
        o.softness_selector,
        crate::brush_engine::hardness::SoftnessSelector::Curve
    );
    let c = &o.softness_curve;
    assert!((c.eval(0.2) - 1.0).abs() < 1e-3);
    assert!((c.eval(0.5) - 0.8).abs() < 0.01, "{}", c.eval(0.5));
    assert!(c.eval(1.0).abs() < 1e-3);
    assert!((o.opacity - 0.7).abs() < 1e-6);
    assert!(o.pressure_size);
    assert_eq!(o.pressure_curves.size.as_ref().unwrap().points.len(), 3);
    assert_eq!(p.brush.dynamics.tip.ratio, 0.5);
    assert!((p.brush.dynamics.tip.angle - 0.5f32.to_degrees()).abs() < 1e-3);
    assert!(imported.notes.is_empty());
}

/// `AUTO` with `params` added (name, value pairs) and its engine `engine`.
fn with_params(engine: &str, params: &[(&str, &str)]) -> Vec<u8> {
    let extra: String = params
        .iter()
        .map(|(k, v)| format!(r#"<param type="string" name="{k}"><![CDATA[{v}]]></param>"#))
        .collect();
    kpp(&AUTO
        .replace("paintbrush", engine)
        .replace("</Preset>", &format!("{extra}</Preset>")))
}

const PRESSURE: &str = r#"<!DOCTYPE params><params id="pressure"/>"#;

/// Standard base64, for embedded resources.
fn base64(bytes: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for k in 0..4 {
            if k <= c.len() {
                s.push(T[(n >> (18 - 6 * k) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

#[test]
fn a_texture_missing_from_the_bundle_comes_from_the_preset_s_copy() {
    let mut png = Vec::new();
    image::GrayImage::from_pixel(4, 4, image::Luma([128]))
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    let brush = with_params(
        "paintbrush",
        &[
            ("Texture/Pattern/Enabled", "true"),
            (
                "Texture/Pattern/PatternFileName",
                "C:/krita/patterns/paper3.pat",
            ),
            ("Texture/Pattern/Pattern", &base64(base64(&png).as_bytes())),
        ],
    );
    let imported = import_kpp(&brush, "file").unwrap();
    let texture = imported.presets[0].brush.texture.as_ref();
    assert_eq!(texture.map(|t| t.pattern.name.as_str()), Some("paper3.pat"));
    assert!(imported.notes.is_empty(), "{:?}", imported.notes);
}

#[test]
fn mix_moves_toward_the_secondary_colour_and_rate_only_times_the_airbrush() {
    let mix = |extra: &[(&str, &str)]| {
        let mut params = vec![("PressureMix", "true"), ("MixValue", "0.8")];
        params.extend_from_slice(extra);
        import_kpp(&with_params("paintbrush", &params), "file").unwrap()
    };
    // By pressure: a full press is the strength's share of the brush
    // colour, none the secondary.
    let imported = mix(&[("MixSensor", PRESSURE)]);
    let m = &imported.presets[0].brush.inputs[0];
    assert_eq!(
        (m.sensor, m.setting),
        (Sensor::Pressure, DabSetting::ColorMix)
    );
    assert!((m.curve.eval(0.0) - 1.0).abs() < 1e-3);
    assert!((m.curve.eval(1.0) - 0.2).abs() < 1e-3);
    assert!(imported.notes.is_empty(), "{:?}", imported.notes);
    // Its curve off: the strength alone, whatever the pen.
    let imported = mix(&[("MixSensor", PRESSURE), ("MixUseCurve", "false")]);
    let m = &imported.presets[0].brush.inputs[0];
    assert!((m.curve.eval(0.0) - 0.2).abs() < 1e-3);
    assert!((m.curve.eval(1.0) - 0.2).abs() < 1e-3);
    // A gradient source: not read.
    let imported = mix(&[("ColorSource/Type", "gradient")]);
    assert!(imported.notes[0].contains("Mix"), "{:?}", imported.notes);
    // Rate: nothing without the airbrush.
    let rate = |airbrush: &str| {
        let params = [
            ("PressureRate", "true"),
            ("PaintOpSettings/isAirbrushing", airbrush),
        ];
        import_kpp(&with_params("paintbrush", &params), "file")
            .unwrap()
            .notes
    };
    assert!(rate("false").is_empty());
    assert!(rate("true")[0].contains("Rate"));
}

#[test]
fn pressure_in_is_the_highest_pressure_so_far() {
    let brush = with_params(
        "paintbrush",
        &[(
            "SizeSensor",
            r#"<!DOCTYPE params><params id="sensorslist"><ChildSensor id="pressurein"/></params>"#,
        )],
    );
    let imported = import_kpp(&brush, "file").unwrap();
    let b = &imported.presets[0].brush;
    assert_eq!(b.inputs[0].sensor, Sensor::PressureIn);
    assert!(imported.notes.is_empty(), "{:?}", imported.notes);
}

#[test]
fn wash_random_sensors_auto_spacing_scatter_and_texture_options_come_across() {
    const FUZZY: &str = r#"<!DOCTYPE params><params id="fuzzy"/>"#;
    let xml = AUTO
        .replace(r#"spacing="0.08""#, r#"spacing="0.08" useAutoSpacing="1" autoSpacingCoeff="0.5""#)
        .replace(
            r#"<param type="string" name="SizeSensor"><![CDATA[<!DOCTYPE params><params id="pressure"><curve>0,0;0.5,0.2;1,1;</curve></params>]]></param>"#,
            &format!(r#"<param type="string" name="SizeSensor"><![CDATA[{FUZZY}]]></param>"#),
        );
    let brush = with_params_xml(
        &xml,
        &[
            ("PaintOpAction", "2"),
            ("PressureScatter", "true"),
            ("ScatterValue", "0.5"),
            ("PressureRatio", "true"),
            ("RatioSensor", FUZZY),
            ("Pressureh", "true"),
            ("hSensor", FUZZY),
            ("hValue", "0.5"),
            ("PaintOpSettings/isAirbrushing", "true"),
            ("PressureRate", "true"),
        ],
    );
    let imported = import_kpp(&brush, "file").unwrap();
    let b = &imported.presets[0].brush;
    let o = &b.brush_options;
    assert_eq!(
        o.painting_mode,
        crate::brush_engine::brush_options::PaintingMode::Wash
    );
    // Random size, not pressure.
    assert!(!o.pressure_size);
    let mapped: Vec<_> = b.inputs.iter().map(|m| (m.sensor, m.setting)).collect();
    assert!(
        mapped.contains(&(Sensor::RandomDab, DabSetting::Size)),
        "{mapped:?}"
    );
    assert!(
        mapped.contains(&(Sensor::RandomDab, DabSetting::Squash)),
        "{mapped:?}"
    );
    // Auto spacing: 0.5 × √size (3 px of 36).
    assert_eq!(o.auto_spacing, Some(0.5));
    assert!(
        (o.spacing - 100.0 * 3.0 / 36.0).abs() < 1e-3,
        "{}",
        o.spacing
    );
    // Half a brush width either way.
    assert_eq!(b.jitter, 50.0);
    // Hue by a random input, either way, by its strength.
    let hue = b
        .inputs
        .iter()
        .find(|m| m.setting == DabSetting::Hue)
        .expect("hue");
    assert_eq!(
        (hue.sensor, hue.amount, hue.both_ways),
        (Sensor::RandomDab, 0.5, true)
    );
    // An option with no counterpart is named in the report.
    assert!(
        imported.notes.iter().any(|n| n.contains("Rate option")),
        "{:?}",
        imported.notes
    );
}

/// A preset's XML with `params` added.
#[test]
fn spikes_fades_grain_tilt_flow_and_random_colour_come_across() {
    const XTILT: &str = r#"<!DOCTYPE params><params id="xtilt"/>"#;
    let xml = AUTO
        .replace(
            r#"spacing="0.08" angle="0.5""#,
            r#"spacing="0.08" angle="0.5" density="0.7" randomness="0.3""#,
        )
        .replace(
            r#"hfade="0.25" vfade="0.25""#,
            r#"hfade="0.25" vfade="0.75" spikes="5""#,
        );
    let brush = with_params_xml(
        &xml,
        &[
            ("PressureFlow", "true"),
            ("FlowSensor", XTILT),
            ("ColorSource/Type", "total_random"),
        ],
    );
    let imported = import_kpp(&brush, "file").unwrap();
    let b = &imported.presets[0].brush;
    let t = b.brush_options.auto_tip;
    assert_eq!(
        (t.spikes, t.fade, t.density, t.randomness),
        (5, [0.25, 0.75], 0.7, 0.3)
    );
    assert_eq!(
        b.brush_options.color_source,
        crate::brush_engine::brush_options::ColorSource::TotalRandom
    );
    assert!(
        b.inputs
            .iter()
            .any(|m| (m.sensor, m.setting) == (Sensor::XTilt, DabSetting::Flow)),
        "{:?}",
        b.inputs
    );
}

fn with_params_xml(xml: &str, params: &[(&str, &str)]) -> Vec<u8> {
    let extra: String = params
        .iter()
        .map(|(k, v)| format!(r#"<param type="string" name="{k}"><![CDATA[{v}]]></param>"#))
        .collect();
    kpp(&xml.replace("</Preset>", &format!("{extra}</Preset>")))
}

#[test]
fn sensors_follow_krita_s_use_curve_and_common_curve() {
    const CURVED: &str =
        r#"<!DOCTYPE params><params id="pressure"><curve>0,0;0.5,0.2;1,1;</curve></params>"#;
    let base = AUTO.replace(
        r#"<param type="string" name="SizeSensor"><![CDATA[<!DOCTYPE params><params id="pressure"><curve>0,0;0.5,0.2;1,1;</curve></params>]]></param>"#,
        "",
    );
    let import = |params: &[(&str, &str)]| {
        let imported = import_kpp(&with_params_xml(&base, params), "file").unwrap();
        imported.presets[0].brush.brush_options.clone()
    };
    // Its curve off: the option's value alone, no pressure.
    let o = import(&[("SizeSensor", CURVED), ("SizeUseCurve", "false")]);
    assert!(!o.pressure_size);
    // The same curve for all: the common one, not the sensor's.
    let o = import(&[
        ("SizeSensor", CURVED),
        ("SizeUseSameCurve", "true"),
        ("SizecommonCurve", "0,0;0.5,0.8;1,1;"),
    ]);
    let c = o.pressure_curves.size.expect("a curve");
    assert!((c.eval(0.5) - 0.8).abs() < 1e-3, "{}", c.eval(0.5));
    // Not the same: the sensor's own.
    let o = import(&[
        ("SizeSensor", CURVED),
        ("SizeUseSameCurve", "false"),
        ("SizecommonCurve", "0,0;0.5,0.8;1,1;"),
    ]);
    let c = o.pressure_curves.size.expect("a curve");
    assert!((c.eval(0.5) - 0.2).abs() < 1e-3, "{}", c.eval(0.5));
}

#[test]
fn a_lightness_map_tip_comes_across() {
    let png = {
        let mut bytes = Vec::new();
        let img = image::RgbaImage::from_pixel(8, 8, image::Rgba([128, 128, 128, 255]));
        image::DynamicImage::ImageRgba8(img)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        bytes
    };
    let xml = r#"<Preset paintopid="paintbrush" name="Map" embedded_resources="0">
        <param type="string" name="brush_definition"><![CDATA[<Brush type="png_brush" filename="tip.png" spacing="0.1" brushApplication="2"/>]]></param>
        </Preset>"#;
    let bundle: HashMap<String, Vec<u8>> = [("tip.png".to_string(), png)].into();
    let mut notes = Vec::new();
    let p = read_kpp(&kpp(xml), "file", &bundle, &mut notes).unwrap();
    let o = &p.brush.brush_options;
    assert!(o.tip_colors, "{notes:?}");
    assert_eq!(
        o.tip_mapping,
        crate::brush_engine::brush_options::TipMapping::Lightness
    );
}

#[test]
fn the_anti_aliasing_box_comes_across() {
    let aa = |xml: &str| {
        import_kpp(&kpp(xml), "file").unwrap().presets[0]
            .brush
            .anti_aliasing
    };
    // Krita reads a missing flag as off.
    assert!(!aa(AUTO));
    assert!(aa(&AUTO.replace(
        r#"type="circle""#,
        r#"antialiasEdges="1" type="circle""#
    )));
}

#[test]
fn a_soft_circle_keeps_its_falloff_curve() {
    // An airbrush: faint in the middle, nothing at the edge, and
    // no fade (which alone would read as a hard tip).
    let xml = AUTO.replace(
        r#"hfade="0.25" vfade="0.25" type="circle""#,
        r#"hfade="0" vfade="0" id="soft" softness_curve="0,0.4;0.43,0.12;1,0;" type="circle""#,
    );
    let imported = import_kpp(&kpp(&xml), "file").unwrap();
    let o = &imported.presets[0].brush.brush_options;
    assert_eq!(
        o.softness_selector,
        crate::brush_engine::hardness::SoftnessSelector::Curve
    );
    assert!(
        (o.softness_curve.eval(0.0) - 0.4).abs() < 1e-3,
        "faint centre"
    );
    assert!(o.softness_curve.eval(1.0).abs() < 1e-3, "gone at the edge");
    // Its curve is read at the squared distance: √0.43 of the way
    // out is its point at 0.43 (0.12).
    let at = o.softness_curve.eval(0.43f32.sqrt());
    assert!((at - 0.12).abs() < 0.01, "{at}");
    // A circle with a full fade is a hard tip: hard here too.
    let hard = AUTO.replace(r#"hfade="0.25" vfade="0.25""#, r#"hfade="1" vfade="1""#);
    let hard = import_kpp(&kpp(&hard), "file").unwrap();
    let o = &hard.presets[0].brush.brush_options;
    assert_eq!(
        o.softness_selector,
        crate::brush_engine::hardness::SoftnessSelector::Gaussian
    );
    assert_eq!(o.hardness, 100.0);
}

#[test]
fn a_gaussian_circle_falls_off_like_krita_s() {
    let xml = AUTO.replace(
        r#"hfade="0.25" vfade="0.25" type="circle""#,
        r#"hfade="0.5" vfade="0.5" id="gauss" type="circle""#,
    );
    let imported = import_kpp(&kpp(&xml), "file").unwrap();
    let c = &imported.presets[0].brush.brush_options.softness_curve;
    // alphafactor·(erf(d + c) − erf(d − c)), fade 0.5.
    let (fade, sqrt2) = (0.5f64, std::f64::consts::SQRT_2);
    let center = 2.5 * (6761.0 * fade - 10000.0) / (sqrt2 * 6761.0 * fade);
    let krita = |r: f64| {
        let d = r * sqrt2 * 12500.0 / (6761.0 * fade);
        (erf(d + center) - erf(d - center)) / (2.0 * erf(center))
    };
    for r in [0.0, 0.3, 0.6, 0.9] {
        let got = c.eval(r as f32) as f64;
        assert!((got - krita(r)).abs() < 0.02, "{r}: {got} vs {}", krita(r));
    }
    assert!(c.eval(0.0) > c.eval(0.5) && c.eval(0.5) > c.eval(0.9));
}

#[test]
fn softness_mirror_fade_and_perspective_come_across() {
    let brush = with_params(
        "paintbrush",
        &[
            ("PressureSoftness", "true"),
            ("SoftnessSensor", PRESSURE),
            ("PressureMirror", "true"),
            ("VerticalMirrorEnabled", "true"),
            ("MirrorSensor", PRESSURE),
            ("PressureSize", "true"),
            (
                "SizeSensor",
                r#"<!DOCTYPE params><params id="sensorslist"><ChildSensor id="fade" length="300" periodic="1"/><ChildSensor id="perspective"/><ChildSensor id="time" duration="1500"/></params>"#,
            ),
        ],
    );
    let imported = import_kpp(&brush, "file").unwrap();
    assert!(imported.notes.is_empty(), "{:?}", imported.notes);
    let b = &imported.presets[0].brush;
    // Its round tip's fade (a quarter) is what softness shrinks.
    assert_eq!(b.brush_options.softening, Softening::Fade(0.25));
    let by = |sensor: Sensor| b.inputs.iter().find(|m| m.sensor == sensor).unwrap();
    let settings: Vec<DabSetting> = b.inputs.iter().map(|m| m.setting).collect();
    assert!(settings.contains(&DabSetting::Softness) && settings.contains(&DabSetting::Mirror));
    assert!(b.dynamics.tip.random_flip_y && !b.dynamics.tip.random_flip_x);
    let fade = by(Sensor::Fade);
    assert_eq!(
        (fade.setting, fade.length, fade.periodic),
        (DabSetting::Size, 300.0, true)
    );
    assert_eq!(by(Sensor::Perspective).setting, DabSetting::Size);
    // The preset's time is in milliseconds.
    assert_eq!(
        (by(Sensor::Time).length, by(Sensor::Time).periodic),
        (1.5, false)
    );

    // A Gaussian tip ignores softness: nothing to map, nothing
    // missing.
    let gauss = with_params_xml(
        &AUTO.replace(r#"type="circle""#, r#"type="circle" id="gauss""#),
        &[("PressureSoftness", "true"), ("SoftnessSensor", PRESSURE)],
    );
    let imported = import_kpp(&gauss, "file").unwrap();
    assert!(imported.notes.is_empty(), "{:?}", imported.notes);
    assert!(imported.presets[0].brush.inputs.is_empty());
}

#[test]
fn speed_reads_on_krita_s_scale() {
    // Size by speed, full when still and gone at a tenth of the
    // preset's full speed (3,000 px/s): here that's past this app's fast.
    let brush = with_params(
        "paintbrush",
        &[(
            "SizeSensor",
            r#"<!DOCTYPE params><params id="speed"><curve>0,1;0.1,0;1,0;</curve></params>"#,
        )],
    );
    let imported = import_kpp(&brush, "file").unwrap();
    let m = &imported.presets[0].brush.inputs[0];
    assert_eq!(m.sensor, Sensor::Speed);
    let fast = crate::brush_engine::dynamics::FAST_SPEED;
    let at = |px_per_s: f32| m.curve.eval(px_per_s / fast);
    assert!((at(0.0) - 1.0).abs() < 1e-3);
    // 1,500 px/s reads where the preset's does: a twentieth along its curve.
    let krita = parse_curve("0,1;0.1,0;1,0;").unwrap();
    assert!(
        (at(1500.0) - krita.eval(0.05)).abs() < 0.02,
        "{}",
        at(1500.0)
    );
    assert!(at(2500.0) < 0.2);
}

#[test]
fn a_krita_mypaint_preset_brings_its_mypaint_brush() {
    let myb = r#"{"version": 3, "settings": {
        "radius_logarithmic": {"base_value": 1.61, "inputs": {}},
        "hardness": {"base_value": 0.7, "inputs": {}},
        "dabs_per_actual_radius": {"base_value": 4.0, "inputs": {}}}}"#;
    let xml = format!(
        r#"<Preset paintopid="mypaintbrush" name="Snirkel" embedded_resources="0">
        <param name="MyPaint/json" type="bytearray">{}</param>
        <param name="MyPaint/diameter" type="internal">12</param>
        <param name="MyPaint/opcity" type="internal">0.5</param>
        </Preset>"#,
        base64(myb.as_bytes())
    );
    let imported = import_kpp(&kpp(&xml), "file").unwrap();
    assert!(imported.notes.is_empty(), "{:?}", imported.notes);
    let p = &imported.presets[0];
    assert_eq!(p.name, "Snirkel");
    let o = &p.brush.brush_options;
    assert_eq!((o.diameter, o.opacity), (12.0, 0.5));
    assert!((o.hardness - 70.0).abs() < 1e-3);
    // Four dabs a radius: an eighth of the size apart.
    assert!((o.spacing - 12.5).abs() < 1e-3);
}

#[test]
fn krita_sketch_settings_and_options_come_across() {
    let brush = with_params(
        "sketchbrush",
        &[
            ("Sketch/probability", "0.6"),
            ("Sketch/lineWidth", "3"),
            ("Sketch/offset", "30"),
            ("Sketch/randomOpacity", "true"),
            ("PressureDensity", "true"),
            ("DensitySensor", PRESSURE),
            ("PressureLine width", "true"),
            ("Line widthSensor", PRESSURE),
            ("PressureOffset scale", "true"),
            ("Offset scaleSensor", PRESSURE),
        ],
    );
    let imported = import_kpp(&brush, "file").unwrap();
    assert!(imported.notes.is_empty(), "{:?}", imported.notes);
    let b = &imported.presets[0].brush;
    let s = b.sketch;
    assert_eq!(
        (s.density, s.thickness, s.offset, s.opacity),
        (0.6, 3.0, 0.3, 0.5)
    );
    assert_eq!(s.reach, 18.0, "the tip's radius");
    let settings: Vec<DabSetting> = b.inputs.iter().map(|m| m.setting).collect();
    assert_eq!(
        settings,
        [
            DabSetting::SketchDensity,
            DabSetting::SketchWidth,
            DabSetting::SketchOffset
        ]
    );
}

#[test]
fn tilt_and_drawing_angle_read_on_krita_s_scales() {
    let one = |id: &str| {
        let mut inputs = Vec::new();
        let read = SensorRead {
            curve: None,
            length: None,
        };
        assert!(map_sensor(&mut inputs, id, DabSetting::Size, false, read));
        inputs.pop().unwrap()
    };
    let at = |m: &InputMapping, x: f32| m.curve.eval(x);
    // Elevation: 1 upright, 0 from 60° of lean (this app's lean is its
    // sine), two thirds at 30°.
    let tilt = one("declination");
    assert!((at(&tilt, 0.0) - 1.0).abs() < 1e-3);
    assert!(at(&tilt, 0.87) < 0.02);
    assert!(
        (at(&tilt, 0.5) - 2.0 / 3.0).abs() < 0.02,
        "{}",
        at(&tilt, 0.5)
    );
    // Tilt direction: leaning right (0 here) is the preset's quarter,
    // leaning down the screen (three quarters here) its half.
    let dir = one("ascension");
    assert!((at(&dir, 0.0) - 0.25).abs() < 0.01);
    assert!((at(&dir, 0.75) - 0.5).abs() < 0.01);
    // Drawing angle on size: going right reads a half, going up (a
    // quarter turn counter-clockwise here) a quarter.
    let angle = one("drawingangle");
    assert!((at(&angle, 0.0) - 0.5).abs() < 0.01);
    assert!((at(&angle, 0.25) - 0.25).abs() < 0.01);
    assert!(!angle.both_ways);
}

#[test]
fn krita_paint_thickness_and_grey_levels_come_across() {
    let brush = with_params(
        "colorsmudge",
        &[
            ("PressurePaintThickness", "true"),
            ("PaintThicknessValue", "0.6"),
            ("PaintThicknessThicknessMode", "1"),
            ("PaintThicknessSensor", PRESSURE),
        ],
    );
    let imported = import_kpp(&brush, "file").unwrap();
    assert!(imported.notes.is_empty(), "{:?}", imported.notes);
    let b = &imported.presets[0].brush;
    let k = b.mixing.unwrap().krita.unwrap();
    assert_eq!((k.thickness, k.overwrite), (0.6, true));
    assert!(
        b.inputs
            .iter()
            .any(|m| (m.sensor, m.setting) == (Sensor::Pressure, DabSetting::PaintThickness))
    );
    // Krita's levels: contrast halved about mid grey 127.
    let tip = TipMask::from_colored(3, 1, vec![255; 3], vec![[0; 3], [127; 3], [255; 3]]);
    let levels = GreyLevels {
        brightness: 0.0,
        contrast: -0.5,
        mid: Some(127.0),
    };
    let out = levels.apply(&tip);
    let greys: Vec<u8> = out.colors.as_ref().unwrap().iter().map(|c| c[0]).collect();
    assert_eq!(greys, [64, 127, 191]);
    // By its own average: a picture all at one grey stays at mid.
    let flat = TipMask::from_colored(2, 1, vec![255; 2], vec![[200; 3]; 2]);
    let auto = GreyLevels {
        mid: None,
        ..levels
    };
    assert_eq!(auto.apply(&flat).colors.as_ref().unwrap()[0][0], 127);
}

#[test]
fn escaped_text_and_attributes_read_as_they_stand_for() {
    let root = parse_xml(
        r#"<Preset name="Ink &amp; Wash"><param>a &lt; b &#38; c<![CDATA[ <raw> ]]></param></Preset>"#,
    )
    .unwrap();
    let preset = &root.children[0];
    assert_eq!(preset.attr("name"), Some("Ink & Wash"));
    assert_eq!(preset.children[0].text, "a < b & c <raw> ");
    assert!(parse_xml("<p>&nope;</p>").is_err(), "an unknown entity");
}

#[test]
fn several_sensors_on_an_option_come_together_as_the_preset_says() {
    // Like Pesi's watercolours: rotation adding randomness, speed and
    // the stroke direction; scatter adding speed and pressure; the
    // smudge length adding randomness and pressure.
    let list = |ids: &[&str]| {
        let children: String = ids
            .iter()
            .map(|id| format!(r#"<ChildSensor id="{id}"/>"#))
            .collect();
        format!(r#"<!DOCTYPE params><params id="sensorslist">{children}</params>"#)
    };
    let rotation = list(&["fuzzy", "speed", "drawingangle"]);
    let scatter = list(&["speed", "pressure"]);
    let smudge = list(&["fuzzy", "pressure"]);
    let brush = with_params(
        "colorsmudge",
        &[
            ("PressureRotation", "true"),
            ("RotationValue", "0.3"),
            ("RotationSensor", &rotation),
            ("RotationcurveMode", "1"),
            ("PressureScatter", "true"),
            ("ScatterValue", "0.5"),
            ("ScatterSensor", &scatter),
            ("ScattercurveMode", "1"),
            ("PressureSmudgeRate", "true"),
            ("SmudgeRateValue", "1"),
            ("SmudgeRateSensor", &smudge),
            ("SmudgeRatecurveMode", "1"),
        ],
    );
    let imported = import_kpp(&brush, "file").unwrap();
    assert!(imported.notes.is_empty(), "{:?}", imported.notes);
    let b = &imported.presets[0].brush;
    for setting in [
        DabSetting::Angle,
        DabSetting::Scatter,
        DabSetting::SmudgeLength,
    ] {
        assert!(
            b.input_combine.contains(&(setting, Combine::Add)),
            "{setting:?}: {:?}",
            b.input_combine
        );
    }
    let sensors = |setting| {
        (b.inputs.iter())
            .filter(|m| m.setting == setting)
            .map(|m| (m.sensor, m.amount))
            .collect::<Vec<_>>()
    };
    // The stroke direction a fixed offset (following the stroke), the
    // rest swinging it by the option's strength.
    assert!(b.dynamics.tip.follow_stroke && b.dynamics.tip.random_angle == 0.0);
    assert_eq!(
        sensors(DabSetting::Angle),
        [(Sensor::RandomDab, 0.3), (Sensor::Speed, 0.3)]
    );
    assert_eq!(
        sensors(DabSetting::Scatter),
        [(Sensor::Speed, 0.5), (Sensor::Pressure, 0.5)]
    );
    assert_eq!(
        sensors(DabSetting::SmudgeLength),
        [(Sensor::RandomDab, 1.0), (Sensor::Pressure, 1.0)]
    );
    assert!(
        !b.mixing.unwrap().pressure_length,
        "pressure is an input here"
    );
}

#[test]
fn option_strengths_scale_what_their_sensors_read() {
    let brush = with_params(
        "paintbrush",
        &[
            ("SizeValue", "0.5"),
            ("PressureRotation", "true"),
            ("RotationValue", "0.25"),
            ("RotationSensor", PRESSURE),
            ("PressureScatter", "true"),
            ("ScatterValue", "1.5"),
            ("ScatterSensor", PRESSURE),
            ("PressureSpacing", "true"),
            ("SpacingValue", "2"),
            ("SpacingSensor", PRESSURE),
            ("PressureOpacity", "true"),
            ("OpacitycurveMode", "2"),
            (
                "OpacitySensor",
                r#"<!DOCTYPE params><params id="sensorslist"><ChildSensor id="pressure"/><ChildSensor id="speed"/></params>"#,
            ),
        ],
    );
    let imported = import_kpp(&brush, "file").unwrap();
    let b = &imported.presets[0].brush;
    // The tip is 36 across; its size option's strength halves it.
    assert_eq!(b.brush_options.diameter, 18.0);
    // Rotation by pressure: either way, up to a quarter of 180°.
    let turn = b
        .inputs
        .iter()
        .find(|m| m.setting == DabSetting::Angle)
        .unwrap();
    assert!(turn.both_ways && turn.amount == 0.25);
    // Scatter by pressure, up to one and a half widths: the inputs add.
    let scatter: f32 = b
        .inputs
        .iter()
        .filter(|m| m.setting == DabSetting::Scatter)
        .map(|m| m.amount)
        .sum();
    assert_eq!((scatter, b.jitter), (1.5, 0.0));
    // Spacing 8% (the tip's), doubled.
    assert_eq!(b.brush_options.spacing, 16.0);
    // Opacity takes the higher of its sensors: both of them inputs,
    // combined so (nothing left to say about it).
    assert!(
        b.input_combine
            .contains(&(DabSetting::Opacity, Combine::Highest))
    );
    let opacity: Vec<Sensor> = (b.inputs.iter())
        .filter(|m| m.setting == DabSetting::Opacity)
        .map(|m| m.sensor)
        .collect();
    assert_eq!(opacity, [Sensor::Pressure, Sensor::Speed]);
    assert!(
        !b.brush_options.pressure_opacity,
        "pressure is an input here"
    );
    assert!(imported.notes.is_empty(), "{:?}", imported.notes);
}

#[test]
fn erf_matches_known_values() {
    for (x, want) in [
        (0.0, 0.0),
        (0.5, 0.5204999),
        (1.0, 0.8427008),
        (-2.0, -0.9953223),
    ] {
        assert!((erf(x) - want).abs() < 1e-6, "{x}");
    }
}

#[test]
fn colour_smudge_mirror_rotation_spacing_and_sharpness_come_across() {
    let smudge = with_params(
        "colorsmudge",
        &[
            ("SmudgeRateValue", "0.6"),
            ("PressureSmudgeRate", "true"),
            ("SmudgeRateSensor", PRESSURE),
            ("PressureColorRate", "true"),
            ("ColorRateValue", "0.5"),
            (
                "ColorRateSensor",
                r#"<!DOCTYPE params><params id="fuzzy"/>"#,
            ),
            ("CompositeOp", "parallel"),
        ],
    );
    let imported = import_kpp(&smudge, "file").unwrap();
    let b = &imported.presets[0].brush;
    let m = b.mixing.expect("colour mixing");
    assert_eq!((m.smudge_length, m.color_rate), (0.6, 0.5));
    assert!(m.pressure_length && !m.pressure_color);
    assert_eq!(b.paint_blend, LayerBlend::Parallel);
    assert!(imported.notes.is_empty(), "{:?}", imported.notes);

    let brush = with_params(
        "paintbrush",
        &[
            ("PressureMirror", "true"),
            ("HorizontalMirrorEnabled", "true"),
            ("MirrorSensor", r#"<!DOCTYPE params><params id="fuzzy"/>"#),
            ("PressureRotation", "true"),
            (
                "RotationSensor",
                r#"<!DOCTYPE params><params id="sensorslist"><ChildSensor id="drawingangle"/><ChildSensor id="tangentialpressure"/></params>"#,
            ),
            ("PressureSpacing", "true"),
            ("SpacingSensor", PRESSURE),
            ("PressureSharpness", "true"),
            ("SharpnessValue", "0.6"),
        ],
    );
    let imported = import_kpp(&brush, "file").unwrap();
    let b = &imported.presets[0].brush;
    let tip = &b.dynamics.tip;
    assert!(tip.random_flip_x && !tip.random_flip_y);
    assert!(tip.follow_stroke);
    assert_eq!(b.inputs.len(), 1);
    assert_eq!(
        (b.inputs[0].sensor, b.inputs[0].setting),
        (Sensor::Wheel, DabSetting::Angle)
    );
    assert!(b.brush_options.pressure_spacing);
    assert!((b.sharpness - 0.4).abs() < 1e-6);
    assert!(b.mixing.is_none());
    assert!(imported.notes.is_empty(), "{:?}", imported.notes);
}

#[test]
fn a_picture_tip_comes_from_the_preset_or_its_bundle() {
    let pixels = vec![255u8; 12 * 6];
    let gbr = gimp::tests::gbr("t", 12, 6, 1, 25, &pixels);
    let b64 = base64(&gbr);
    let xml = format!(
        r#"<Preset paintopid="paintbrush" name="Pic" embedded_resources="1">
        <resources><resource type="brushes" filename="tip.gbr" name="tip">{b64}</resource></resources>
        <param type="string" name="brush_definition"><![CDATA[<Brush type="gbr_brush" filename="tip.gbr" spacing="0.2" scale="2"/>]]></param>
        </Preset>"#
    );
    let imported = import_kpp(&kpp(&xml), "file").unwrap();
    let o = &imported.presets[0].brush.brush_options;
    let PixelBrushShape::Custom(tip) = &o.pixel_shape else {
        panic!("the picture: {:?}", imported.notes);
    };
    assert_eq!((tip.width, tip.height), (12, 6));
    assert_eq!(o.diameter, 24.0);
    // Not embedded and not in a bundle: a note, still a preset.
    let lonely = xml.replace("tip.gbr\" name", "other.gbr\" name");
    let imported = import_kpp(&kpp(&lonely), "file").unwrap();
    assert_eq!(imported.notes.len(), 1);
}

#[test]
fn an_svg_tip_is_drawn_from_its_picture_at_its_own_size() {
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100"><rect width="200" height="100"/></svg>"#;
    let b64 = base64(svg);
    let xml = format!(
        r#"<Preset paintopid="paintbrush" name="Leaves" embedded_resources="1">
        <resources><resource type="brushes" filename="leaves.svg" name="leaves">{b64}</resource></resources>
        <param type="string" name="brush_definition"><![CDATA[<Brush scale="0.5" type="svg_brush" spacing="0.24" filename="leaves.svg"/>]]></param>
        </Preset>"#
    );
    let imported = import_kpp(&kpp(&xml), "file").unwrap();
    assert!(imported.notes.is_empty(), "{:?}", imported.notes);
    let o = &imported.presets[0].brush.brush_options;
    let PixelBrushShape::Custom(tip) = &o.pixel_shape else {
        panic!("the picture");
    };
    assert!(tip.svg.is_some());
    assert_eq!((tip.width, tip.height), (1024, 512));
    assert_eq!(o.diameter, 100.0, "200 px at half size, as Krita draws it");
    // Saved and loaded again: the same picture.
    let preset = &imported.presets[0];
    let file = crate::brush_engine::preset_file::encode(std::slice::from_ref(preset)).unwrap();
    let back = crate::brush_engine::preset_file::decode(&file).unwrap();
    assert_eq!(back[0].brush.brush_options.pixel_shape, o.pixel_shape);
}

#[test]
fn other_engines_come_with_a_note_and_damage_is_refused() {
    let dyna = AUTO.replace("paintbrush", "dynabrush");
    let imported = import_kpp(&kpp(&dyna), "file").unwrap();
    assert!(imported.notes[0].contains("dynabrush"));
    assert!(import_kpp(b"not a png", "f").is_err());
    assert!(import_kpp(&kpp("<Preset><param>"), "f").is_err());
}

#[test]
fn krita_engines_come_across_as_this_apps() {
    use crate::brush_engine::brush::BrushType;
    let with = |engine: &str, params: &str| {
        let xml = AUTO
            .replace("paintbrush", engine)
            .replace("</Preset>", &format!("{params}</Preset>"));
        import_kpp(&kpp(&xml), "file")
            .unwrap()
            .presets
            .remove(0)
            .brush
    };
    let p =
        |k: &str, v: &str| format!(r#"<param type="string" name="{k}"><![CDATA[{v}]]></param>"#);
    let spray = with(
        "spraybrush",
        &[
            p("Spray/particleCount", "120"),
            p("Spray/gaussianDistribution", "true"),
        ]
        .concat(),
    );
    assert_eq!(spray.brush_type, BrushType::Spray);
    assert_eq!(spray.engines.spray.amount, 120);
    assert_eq!(
        spray.engines.spray.distribution,
        crate::brush_engine::engines::Distribution::Gaussian
    );
    // Shape off: the particles are the (36 px) tip, scaled.
    let stamp = with(
        "spraybrush",
        &[
            p("Spray/diameter", "200"),
            p("Spray/scale", "2"),
            p("SprayShape/enabled", "false"),
            p("SprayShape/width", "6"),
        ]
        .concat(),
    );
    assert!((stamp.engines.spray.particle_size - 0.36).abs() < 1e-6);
    let chalk = with("chalkbrush", &p("Chalk/radius", "12"));
    assert_eq!(
        (chalk.brush_type, chalk.brush_options.diameter),
        (BrushType::Chalk, 24.0)
    );
    let curve = with(
        "curvebrush",
        &[
            p("Curve/lineWidth", "3"),
            p("Curve/strokeHistorySize", "40"),
            p("Curve/makeConnection", "true"),
        ]
        .concat(),
    );
    assert_eq!(curve.brush_type, BrushType::Curve);
    assert_eq!(
        (curve.engines.curve.line_width, curve.engines.curve.history),
        (3.0, 40)
    );
    assert!(curve.engines.curve.connection);
    let grid = with(
        "gridbrush",
        &[
            p("Grid/gridWidth", "24"),
            p("Grid/scale", "2"),
            p("Grid/verticalBorder", "12"),
        ]
        .concat(),
    );
    assert_eq!(
        (grid.brush_type, grid.engines.grid.cell),
        (BrushType::Grid, 48.0)
    );
    assert_eq!(grid.engines.grid.scale, 0.5, "the border either side");
    let hatch = with(
        "hatchingbrush",
        &[
            p("Hatching/angle", "30"),
            p("Hatching/separation", "8"),
            p("Hatching/bool_nocrosshatching", "false"),
        ]
        .concat(),
    );
    assert_eq!(
        (
            hatch.hatching.angle,
            hatch.hatching.separation,
            hatch.hatching.crosshatch
        ),
        (-30.0, 8.0, true)
    );
    let normal = with("tangentnormal", &p("Tangent/swizzleGreen", "3"));
    assert_eq!(normal.brush_type, BrushType::TangentNormal);
    assert!(normal.engines.normal.flip_y);
    let particle = with(
        "particlebrush",
        &[p("Particle/count", "60"), p("Particle/gravity", "0.9")].concat(),
    );
    assert!((particle.engines.particles.drag - 0.1).abs() < 1e-6);
    assert_eq!(particle.engines.particles.gravity, [0.0, 0.0], "no fall");
    assert_eq!(
        (particle.brush_type, particle.engines.particles.count),
        (BrushType::Particle, 60)
    );
}

/// The options of Krita's spray, grid, particle and bristle engines this
/// app has too.
#[test]
fn krita_engine_options_come_across() {
    let brush = |engine: &str, params: &[(&str, &str)]| {
        import_kpp(&with_params(engine, params), "file")
            .unwrap()
            .presets
            .remove(0)
            .brush
    };
    let spray = brush(
        "spraybrush",
        &[
            ("Spray/useDensity", "true"),
            ("Spray/coverage", "0.4"),
            ("Spray/aspect", "2"),
            ("Spray/rotation", "30"),
            ("Spray/jitterMovement", "true"),
            ("Spray/jitterMoveAmount", "0.5"),
            ("ColorOption/useRandomHSV", "true"),
            ("ColorOption/hue", "15"),
            ("ColorOption/saturation", "-10"),
            ("ColorOption/value", "35"),
            ("ColorOption/useRandomOpacity", "true"),
            ("ColorOption/mixBgColor", "true"),
        ],
    )
    .engines
    .spray;
    assert_eq!(
        (spray.coverage, spray.aspect, spray.rotation, spray.jitter),
        (0.4, 2.0, 30.0, 0.5)
    );
    assert_eq!(spray.random_hsv, [15.0, 0.1, 0.35]);
    assert!(spray.random_opacity && spray.mix_secondary);
    let grid = brush(
        "gridbrush",
        &[
            ("Grid/gridWidth", "20"),
            ("Grid/gridHeight", "40"),
            ("Grid/divisionLevel", "3"),
            ("Grid/pressureDivision", "true"),
            ("Grid/randomBorder", "true"),
        ],
    )
    .engines
    .grid;
    assert_eq!(
        (grid.cell, grid.cell_height, grid.divisions),
        (20.0, 40.0, 3)
    );
    assert!(grid.divide_by_pressure && grid.random_border > 0.0);
    let particles = brush("particlebrush", &[("Particle/iterations", "15")])
        .engines
        .particles;
    assert_eq!(particles.iterations, 15);
    assert!(particles.dots && particles.spread == 0.0 && particles.weight_spread > 0.0);
    let hairy = brush(
        "hairybrush",
        &[
            ("HairyBristle/shear", "0.5"),
            ("HairyBristle/density", "60"),
            ("HairyInk/enabled", "true"),
            ("HairyBristle/random", "5"),
            ("HairyInk/inkAmount", "300"),
            ("HairyInk/useSaturation", "true"),
            ("HairyInk/inkDepletionCurve", "0,0;0.666667,0.629956;1,1;"),
            ("HairyInk/soak", "true"),
        ],
    );
    let b = hairy.bristles;
    assert!(b.from_tip && b.deplete_saturation && b.soak);
    assert_eq!((b.shear, b.density, b.ink), (0.5, 0.6, 300.0));
    assert_eq!(b.random_offset, 0.5);
    let curve = b.depletion.expect("its depletion curve");
    assert!((curve.eval(0.666667) - 0.63).abs() < 0.01);
}

#[test]
fn a_bundle_brings_its_presets() {
    let mut zip = crate::project::zip::ZipWriter::default();
    zip.add("mimetype", b"application/x-krita-resourcebundle")
        .unwrap();
    zip.add("paintoppresets/a.kpp", &kpp(AUTO)).unwrap();
    zip.add(
        "paintoppresets/b.kpp",
        &kpp(&AUTO.replace("Soft Ink", "Second")),
    )
    .unwrap();
    let imported = import_bundle(&zip.finish().unwrap()).unwrap();
    let names: Vec<&str> = imported.presets.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["Soft Ink", "Second"]);
}

#[test]
#[ignore = "fuzzing"]
fn fuzz_krita_presets() {
    // The preset XML (the PNG around it has checksums).
    crate::fuzz::fuzz(
        "kpp-xml",
        AUTO.as_bytes(),
        std::time::Duration::from_secs(2),
        |b| {
            let mut notes = Vec::new();
            // Latin-1, as PNG text must be.
            let xml: String = b.iter().map(|&c| c as char).collect();
            let _ = read_kpp(&kpp(&xml), "fuzz", &HashMap::new(), &mut notes);
        },
    );
}
