//! GPU pixel operations with reused staging buffers and exact Color32 codecs.
//! CPU tiles remain authoritative; callers retain their usual history and masks.
use super::{blend::Unmultiply, filters::Filter};
use eframe::{egui::Color32, egui_wgpu::wgpu};
use std::sync::{Arc, Mutex, OnceLock};
use wgpu::util::DeviceExt;

static GPU: OnceLock<Mutex<Option<Arc<GpuOps>>>> = OnceLock::new();
/// The finishing pass's method, its parameters, lookup table and blur radii
/// (none: a per-pixel filter).
type FilterParameters = (u32, [f32; 12], Vec<u32>, Vec<usize>);
/// A filter and its reach (log2). Compared once, whatever the image size,
/// so a preview whose size changes doesn't compare again mid-drag; a size
/// where the transfers tip the balance the other way isn't told apart.
type PolicyKey = (&'static str, u32);
type Policy = std::collections::HashMap<PolicyKey, bool>;
static POLICY: OnceLock<Mutex<Policy>> = OnceLock::new();

/// The most one staging buffer holds; bigger images go in bands of rows.
/// Four are kept once used (source, two working, readback), so this bounds
/// the GPU memory held to 64 MB.
const BAND_BYTES: u64 = 16 << 20;
/// Blur passes, each with its parameters in its own slot.
const MAX_PASSES: u64 = 8;
/// Largest workgroup count per dimension every device allows.
const MAX_GROUPS: usize = 65535;

fn policy(key: PolicyKey) -> Option<bool> {
    POLICY
        .get_or_init(|| Mutex::new(Policy::new()))
        .lock()
        .ok()?
        .get(&key)
        .copied()
}

fn remember_policy(
    key: PolicyKey,
    gpu: std::time::Duration,
    cpu: std::time::Duration,
    correct: bool,
) {
    let enabled = correct && gpu.as_secs_f64() < cpu.as_secs_f64() * 0.9;
    if let Ok(mut policy) = POLICY.get_or_init(|| Mutex::new(Policy::new())).lock() {
        policy.insert(key, enabled);
    }
    log::info!(
        "GPU {} calibration (reach 2^{}): GPU {:.2?}, CPU {:.2?}, enabled={enabled}",
        key.0,
        key.1,
        gpu,
        cpu
    );
}

pub(crate) fn initialize(device: Arc<wgpu::Device>, queue: Arc<wgpu::Queue>) {
    if device.limits().max_storage_buffers_per_shader_stage < 5 {
        return;
    }
    let ops = Arc::new(GpuOps::new(device, queue));
    *GPU.get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(ops);
}

pub(crate) fn filter(
    filter: &Filter,
    source: &[Color32],
    width: usize,
    height: usize,
) -> Option<Vec<Color32>> {
    // Upload/readback overhead dominates tiny buffers. Deep-document callers
    // explicitly choose the CPU path before reaching this function.
    if source.len() < 256 * 256 {
        return None;
    }
    let key = (filter.name(), (filter.reach().max(1) as u32).ilog2());
    let decision = policy(key);
    if decision == Some(false) {
        return None;
    }
    let ops = GPU.get()?.lock().ok()?.clone()?;
    if decision == Some(true) {
        return ops.filter(filter, source, width, height).ok();
    }
    // First use: the driver's start-up and this size's buffers aren't
    // counted against the GPU.
    ops.prepare(filter, width, height).ok()?;
    let start = std::time::Instant::now();
    let result = ops.filter(filter, source, width, height).ok()?;
    let elapsed = start.elapsed();
    let start = std::time::Instant::now();
    let reference = filter.apply(source, width, height, (0, 0));
    let cpu = start.elapsed();
    let correct = reference.iter().zip(&result).all(|(a, b)| {
        a.to_array()
            .into_iter()
            .zip(b.to_array())
            .all(|(a, b)| a.abs_diff(b) <= 3)
    });
    remember_policy(key, elapsed, cpu, correct);
    Some(reference)
}

/// The kinds of pass, one entry point each (see the shader).
struct Pipelines {
    point: wgpu::ComputePipeline,
    premultiply: wgpu::ComputePipeline,
    box_rows: wgpu::ComputePipeline,
    box_columns: wgpu::ComputePipeline,
    finish: wgpu::ComputePipeline,
}

/// Buffers kept between operations, grown when an image needs more.
struct Scratch {
    /// Words (pixels) each of the four pixel buffers holds.
    words: u64,
    input: wgpu::Buffer,
    a: wgpu::Buffer,
    b: wgpu::Buffer,
    readback: wgpu::Buffer,
    lookup_words: u64,
    lookup: wgpu::Buffer,
}

pub struct GpuOps {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    layout: wgpu::BindGroupLayout,
    pipelines: Pipelines,
    codecs: wgpu::Buffer,
    /// One slot per pass, at the uniform offset alignment.
    params: wgpu::Buffer,
    slot: u64,
    /// Also one operation at a time: they share the buffers.
    scratch: Mutex<Option<Scratch>>,
    warmed: std::sync::Once,
    /// See [`BAND_BYTES`] (smaller in tests, to cross bands).
    band_bytes: u64,
}

impl GpuOps {
    pub fn new(device: Arc<wgpu::Device>, queue: Arc<wgpu::Queue>) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("pixel operations"),
            source: wgpu::ShaderSource::Wgsl(include_str!("gpu_ops.wgsl").into()),
        });
        let storage = |binding, read_only| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("pixel operations"),
            entries: &[
                storage(0, true),
                storage(1, false),
                storage(2, true),
                storage(3, true),
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                storage(5, true),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pixel operations"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = |entry_point| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry_point),
                layout: Some(&pipeline_layout),
                module: &shader,
                entry_point,
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let pipelines = Pipelines {
            point: pipeline("point"),
            premultiply: pipeline("premultiply"),
            box_rows: pipeline("box_rows"),
            box_columns: pipeline("box_columns"),
            finish: pipeline("finish"),
        };
        // Both tables reproduce egui's nonlinear premultiplication exactly.
        let mut table = Vec::with_capacity(2 * 65536);
        for alpha in 0..=255u8 {
            for channel in 0..=255u8 {
                table.push(
                    Color32::from_rgba_premultiplied(channel, channel, channel, alpha)
                        .unmultiplied()[0] as u32,
                );
            }
        }
        for alpha in 0..=255u8 {
            for channel in 0..=255u8 {
                table.push(
                    Color32::from_rgba_unmultiplied(channel, channel, channel, alpha).r() as u32,
                );
            }
        }
        let codecs = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Color32 codecs"),
            contents: bytemuck::cast_slice(&table),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let slot = u64::from(device.limits().min_uniform_buffer_offset_alignment).max(64);
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("operation parameters"),
            size: slot * MAX_PASSES,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let band_bytes = BAND_BYTES.min(u64::from(device.limits().max_storage_buffer_binding_size));
        Self {
            device,
            queue,
            layout,
            pipelines,
            codecs,
            params,
            slot,
            scratch: Mutex::new(None),
            warmed: std::sync::Once::new(),
            band_bytes,
        }
    }

    /// Before timing `filter` on this size: start the driver up (once) and
    /// size the buffers.
    fn prepare(&self, filter: &Filter, width: usize, height: usize) -> Result<(), String> {
        self.warmed.call_once(|| {
            let _ = self.filter(&Filter::Invert, &[Color32::TRANSPARENT; 64], 8, 8);
        });
        let (_, _, lookup, radii) = Self::filter_parameters(filter)?;
        let halo = radii.iter().sum::<usize>();
        let rows = self.band_rows(width, halo)?;
        let mut scratch = self
            .scratch
            .lock()
            .map_err(|_| "GPU operations unavailable")?;
        self.reserve(
            &mut scratch,
            ((rows + 2 * halo).min(height) * width) as u64,
            lookup.len() as u64,
        );
        Ok(())
    }

    /// Rows each band writes, given `halo` rows read on both sides.
    fn band_rows(&self, width: usize, halo: usize) -> Result<usize, String> {
        // (The row blur takes a workgroup per row.)
        let rows = ((self.band_bytes / 4) as usize / width).min(MAX_GROUPS);
        if width.div_ceil(8) > MAX_GROUPS {
            return Err("Image too wide for the GPU".into());
        }
        rows.checked_sub(2 * halo)
            .filter(|&n| n > 0)
            .ok_or_else(|| "Filter reach exceeds GPU staging budget".into())
    }

    fn reserve<'a>(
        &self,
        scratch: &'a mut Option<Scratch>,
        words: u64,
        lookup_words: u64,
    ) -> &'a Scratch {
        let pixel_buffer = |label, usage, words: u64| {
            self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: words * 4,
                usage,
                mapped_at_creation: false,
            })
        };
        let work = wgpu::BufferUsages::STORAGE
            | wgpu::BufferUsages::COPY_SRC
            | wgpu::BufferUsages::COPY_DST;
        let s = scratch.get_or_insert_with(|| Scratch {
            words: 0,
            input: pixel_buffer("operation source", work, 1),
            a: pixel_buffer("operation intermediate A", work, 1),
            b: pixel_buffer("operation intermediate B", work, 1),
            readback: pixel_buffer(
                "operation readback",
                wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                1,
            ),
            lookup_words: 0,
            lookup: pixel_buffer("operation lookup", work, 1),
        });
        if s.words < words {
            // Grown by doubling (to the band's size), so a preview that
            // grows a little each frame doesn't reallocate each time.
            let words = words
                .next_power_of_two()
                .min(self.band_bytes / 4)
                .max(words);
            s.input = pixel_buffer("operation source", work, words);
            s.a = pixel_buffer("operation intermediate A", work, words);
            s.b = pixel_buffer("operation intermediate B", work, words);
            s.readback = pixel_buffer(
                "operation readback",
                wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                words,
            );
            s.words = words;
        }
        if s.lookup_words < lookup_words {
            s.lookup = pixel_buffer("operation lookup", work, lookup_words);
            s.lookup_words = lookup_words;
        }
        s
    }

    /// Filter in bands of whole rows, each with the rows its neighbourhood
    /// reaches above and below. Returns an error before allocating GPU
    /// buffers for unsupported operations or invalid sizes.
    pub fn filter(
        &self,
        filter: &Filter,
        source: &[Color32],
        width: usize,
        height: usize,
    ) -> Result<Vec<Color32>, String> {
        if width.checked_mul(height) != Some(source.len()) || width == 0 || height == 0 {
            return Err("Invalid pixel buffer".into());
        }
        let (op, parameters, lookup, radii) = Self::filter_parameters(filter)?;
        let halo = radii.iter().sum::<usize>();
        // The row blur's tile holds 256 pixels and the radius either side.
        if halo > 600 || radii.iter().any(|&r| r > 384) {
            return Err("Filter radius exceeds GPU staging budget".into());
        }
        let rows = self.band_rows(width, halo)?;
        let mut scratch = self
            .scratch
            .lock()
            .map_err(|_| "GPU operations unavailable")?;
        let s = self.reserve(
            &mut scratch,
            ((rows + 2 * halo).min(height) * width) as u64,
            lookup.len() as u64,
        );
        self.queue
            .write_buffer(&s.lookup, 0, bytemuck::cast_slice(&lookup));
        let mut output = Vec::with_capacity(source.len());
        for y in (0..height).step_by(rows) {
            let (y0, y1) = (y.saturating_sub(halo), (y + rows + halo).min(height));
            let band = (y1 - y0) as u32;
            self.queue.write_buffer(
                &s.input,
                0,
                bytemuck::cast_slice(&source[y0 * width..y1 * width]),
            );
            let p = &self.pipelines;
            let w = width as u32;
            let pixels = (w.div_ceil(8), band.div_ceil(8));
            // (pipeline, source, destination, method, radius, workgroups)
            let mut passes = Vec::new();
            let result = if radii.is_empty() {
                passes.push((&p.point, &s.input, &s.a, op, 0, pixels));
                &s.a
            } else {
                passes.push((&p.premultiply, &s.input, &s.a, 0, 0, pixels));
                for &radius in &radii {
                    let r = radius as u32;
                    let rows = (w.div_ceil(256), band);
                    let columns = (w.div_ceil(64), band.div_ceil(64));
                    passes.push((&p.box_rows, &s.a, &s.b, 0, r, rows));
                    passes.push((&p.box_columns, &s.b, &s.a, 0, r, columns));
                }
                passes.push((&p.finish, &s.a, &s.b, op, 0, pixels));
                &s.b
            };
            let mut slots = vec![0u8; (self.slot as usize) * passes.len()];
            for (k, &(_, _, _, method, radius, _)) in passes.iter().enumerate() {
                let mut values = vec![w, band, method, radius];
                values.extend(parameters.map(f32::to_bits));
                let at = k * self.slot as usize;
                slots[at..at + 64].copy_from_slice(bytemuck::cast_slice(&values));
            }
            self.queue.write_buffer(&self.params, 0, &slots);
            let mut encoder = self.device.create_command_encoder(&Default::default());
            for (k, &(pipeline, from, to, _, _, (gx, gy))) in passes.iter().enumerate() {
                let group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("operation bindings"),
                    layout: &self.layout,
                    entries: &[
                        Self::entry(0, from),
                        Self::entry(1, to),
                        Self::entry(2, &self.codecs),
                        Self::entry(3, &s.lookup),
                        wgpu::BindGroupEntry {
                            binding: 4,
                            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                                buffer: &self.params,
                                offset: k as u64 * self.slot,
                                size: wgpu::BufferSize::new(64),
                            }),
                        },
                        Self::entry(5, &s.input),
                    ],
                });
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("pixel operation"),
                    timestamp_writes: None,
                });
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, &group, &[]);
                pass.dispatch_workgroups(gx, gy, 1);
            }
            // Only the band's own rows come back, not the halo.
            let written = (y + rows).min(height) - y;
            let bytes = (written * width * 4) as u64;
            encoder.copy_buffer_to_buffer(
                result,
                ((y - y0) * width * 4) as u64,
                &s.readback,
                0,
                bytes,
            );
            let submission = self.queue.submit([encoder.finish()]);
            let slice = s.readback.slice(..bytes);
            let (send, receive) = std::sync::mpsc::channel();
            slice.map_async(wgpu::MapMode::Read, move |result| {
                let _ = send.send(result);
            });
            // Waits for this work only, not whatever else the app submitted.
            self.device.poll(wgpu::Maintain::wait_for(submission));
            receive
                .recv()
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
            output.extend_from_slice(bytemuck::cast_slice(&slice.get_mapped_range()));
            s.readback.unmap();
        }
        Ok(output)
    }

    fn entry(binding: u32, buffer: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
        wgpu::BindGroupEntry {
            binding,
            resource: buffer.as_entire_binding(),
        }
    }

    fn filter_parameters(filter: &Filter) -> Result<FilterParameters, String> {
        let mut p = [0.0; 12];
        if let Some(lut) = filter.channel_lut() {
            return Ok((
                0,
                p,
                lut.iter()
                    .flat_map(|c| c.iter().map(|&v| v as u32))
                    .collect(),
                vec![],
            ));
        }
        let op = match filter {
            Filter::HueSaturation {
                hue,
                saturation,
                lightness,
            } => {
                p[..3].copy_from_slice(&[*hue / 360.0, *saturation, *lightness]);
                1
            }
            Filter::Desaturate => 2,
            Filter::ColourBalance {
                shadows,
                midtones,
                highlights,
                preserve_luminosity,
            } => {
                p[..3].copy_from_slice(shadows);
                p[4..7].copy_from_slice(midtones);
                p[8..11].copy_from_slice(highlights);
                p[3] = f32::from(u8::from(*preserve_luminosity));
                3
            }
            Filter::GradientMap(_) => {
                let lookup = (0..=65535)
                    .map(|i| {
                        let v = i as f32 / 65535.0;
                        let rgb = filter
                            .rgb([v; 3])
                            .unwrap()
                            .map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8);
                        u32::from_le_bytes([rgb[0], rgb[1], rgb[2], 255])
                    })
                    .collect();
                return Ok((4, p, lookup, vec![]));
            }
            Filter::GaussianBlur { radius } | Filter::Sharpen { radius, .. } => {
                if !radius.is_finite() || *radius < 0.3 {
                    return Err("Use CPU for identity blur".into());
                }
                if let Filter::Sharpen { amount, .. } = filter {
                    p[0] = *amount;
                }
                return Ok((
                    u32::from(matches!(filter, Filter::Sharpen { .. })),
                    p,
                    vec![0],
                    super::filters::box_radii(*radius).to_vec(),
                ));
            }
            _ => return Err("Filter is not GPU accelerated".into()),
        };
        Ok((op, p, vec![0], vec![]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headless() -> Option<GpuOps> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::default());
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))?;
        eprintln!("GPU operations tests: {:?}", adapter.get_info());
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("GPU operation tests"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: Default::default(),
            },
            None,
        ))
        .ok()?;
        Some(GpuOps::new(Arc::new(device), Arc::new(queue)))
    }

    #[test]
    #[ignore = "measures the local GPU; run explicitly with --nocapture"]
    fn gpu_operation_timings() {
        let gpu = headless().expect("GPU adapter for timings");
        let mut report = String::new();
        for side in [2048, 4096] {
            let pixels: Vec<_> = (0..side * side)
                .map(|i| {
                    Color32::from_rgba_unmultiplied(
                        (i * 17) as u8,
                        (i * 37) as u8,
                        (i * 61) as u8,
                        (i * 13) as u8,
                    )
                })
                .collect();
            for filter in [
                Filter::BrightnessContrast {
                    brightness: 0.2,
                    contrast: 0.3,
                },
                Filter::HueSaturation {
                    hue: 30.0,
                    saturation: 0.2,
                    lightness: 0.1,
                },
                Filter::GaussianBlur { radius: 4.0 },
                Filter::GaussianBlur { radius: 30.0 },
                Filter::Sharpen {
                    radius: 4.0,
                    amount: 0.8,
                },
            ] {
                let started = std::time::Instant::now();
                let reference = filter.apply(&pixels, side, side, (0, 0));
                let cpu = started.elapsed();
                let started = std::time::Instant::now();
                let result = gpu.filter(&filter, &pixels, side, side).unwrap();
                let elapsed = started.elapsed();
                let error = reference
                    .iter()
                    .zip(&result)
                    .flat_map(|(a, b)| {
                        a.to_array()
                            .into_iter()
                            .zip(b.to_array())
                            .map(|(a, b)| a.abs_diff(b))
                    })
                    .max()
                    .unwrap();
                let line = format!(
                    "{} {}x{} CPU={:.2?} GPU={:.2?} max_error={error}\n",
                    filter.name(),
                    side,
                    side,
                    cpu,
                    elapsed
                );
                eprint!("{line}");
                report.push_str(&line);
            }
        }
        std::fs::write(std::env::temp_dir().join("rusty-gpu-timings.txt"), report).unwrap();
    }

    #[test]
    fn gpu_filters_match_cpu_with_alpha_and_chunk_boundaries() {
        let Some(gpu) = headless() else {
            assert!(
                std::env::var_os("RUSTY_REQUIRE_GPU_TESTS").is_none(),
                "GPU adapter required"
            );
            eprintln!("GPU adapter unavailable; set RUSTY_REQUIRE_GPU_TESTS=1 to require it");
            return;
        };
        let (w, h) = (523, 37);
        let pixels: Vec<_> = (0..w * h)
            .map(|i| {
                Color32::from_rgba_unmultiplied(
                    (i * 17) as u8,
                    (i * 37) as u8,
                    (i * 61) as u8,
                    (i * 13) as u8,
                )
            })
            .collect();
        let filters = [
            Filter::Invert,
            Filter::Desaturate,
            Filter::BrightnessContrast {
                brightness: 0.2,
                contrast: 0.3,
            },
            Filter::Levels {
                black: 0.1,
                white: 0.8,
                gamma: 1.7,
            },
            Filter::Exposure { stops: 0.7 },
            Filter::Curves {
                rgb: super::super::filters::ToneCurve::from_points(&[
                    [0.0, 0.0],
                    [0.4, 0.7],
                    [1.0, 1.0],
                ]),
                red: Default::default(),
                green: Default::default(),
                blue: Default::default(),
            },
            Filter::GradientMap(super::super::gradient::GradientStops::from_colors(&[
                (0.0, [20, 80, 180]),
                (0.5, [255, 220, 50]),
                (1.0, [0, 30, 90]),
            ])),
            Filter::HueSaturation {
                hue: -87.0,
                saturation: 0.3,
                lightness: -0.2,
            },
            Filter::ColourBalance {
                shadows: [0.2, -0.1, 0.3],
                midtones: [0.1, 0.3, -0.2],
                highlights: [-0.1, 0.1, 0.2],
                preserve_luminosity: true,
            },
            Filter::GaussianBlur { radius: 2.7 },
            Filter::Sharpen {
                radius: 2.7,
                amount: 0.8,
            },
        ];
        for filter in filters {
            let reference = filter.apply(&pixels, w, h, (0, 0));
            let result = gpu.filter(&filter, &pixels, w, h).unwrap();
            let error = reference
                .iter()
                .zip(&result)
                .flat_map(|(a, b)| {
                    a.to_array()
                        .into_iter()
                        .zip(b.to_array())
                        .map(|(a, b)| a.abs_diff(b))
                })
                .max()
                .unwrap();
            assert!(error <= 3, "{}: max channel error {error}", filter.name());
        }
        // Bands: a small staging budget splits this image into several,
        // each blur reading its neighbours' rows; the result is the same.
        let mut banded = gpu;
        banded.band_bytes = (w * 4 * 40) as u64;
        let (tall, rows) = (&pixels[..w * 37], 37);
        for filter in [
            Filter::Invert,
            Filter::GaussianBlur { radius: 2.7 },
            Filter::Sharpen {
                radius: 1.5,
                amount: 0.8,
            },
        ] {
            let whole = filter.apply(tall, w, rows, (0, 0));
            assert!(banded.band_rows(w, 6).unwrap() < rows);
            let result = banded.filter(&filter, tall, w, rows).unwrap();
            assert_eq!(result.len(), whole.len());
            let error = whole
                .iter()
                .zip(&result)
                .flat_map(|(a, b)| {
                    a.to_array()
                        .into_iter()
                        .zip(b.to_array())
                        .map(|(a, b)| a.abs_diff(b))
                })
                .max()
                .unwrap();
            assert!(
                error <= 3,
                "banded {}: max channel error {error}",
                filter.name()
            );
        }
    }
}
