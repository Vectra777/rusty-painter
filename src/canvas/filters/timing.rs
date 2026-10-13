use super::*;

#[test]
#[ignore = "timing; run with --release --ignored --nocapture"]
fn filters_4k() {
    let (w, h) = (4096, 4096);
    let src: Vec<Color32> = (0..w * h)
        .map(|i| Color32::from_gray((i % 251) as u8))
        .collect();
    for f in [
        Filter::HueSaturation {
            hue: 30.0,
            saturation: 0.2,
            lightness: 0.0,
        },
        Filter::GaussianBlur { radius: 10.0 },
        Filter::GaussianBlur { radius: 60.0 },
        Filter::MotionBlur {
            angle: 30.0,
            distance: 200.0,
        },
        Filter::Sharpen {
            radius: 2.0,
            amount: 1.0,
        },
        Filter::Pixelate { size: 16 },
        Filter::ZoomBlur {
            amount: 0.1,
            frame: Frame::NONE,
        }
        .fitted(w, h),
        Filter::SpinBlur {
            angle: 10.0,
            frame: Frame::NONE,
        }
        .fitted(w, h),
        Filter::MotionBlur {
            angle: 0.0,
            distance: 20.0,
        },
    ] {
        let t = std::time::Instant::now();
        std::hint::black_box(f.apply(&src, w, h, (0, 0)));
        println!("{:>22}: {:?}", f.name(), t.elapsed());
    }
}
