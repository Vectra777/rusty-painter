//! Shader layers in the app: compiling their shaders, their clocks, when
//! they show live on the GPU (see `view::shader_gpu`) and baking their
//! frame into the layer's pixels for everything else (export, merging,
//! thumbnails, and the screen when they can't show live).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use eframe::egui_wgpu;

use crate::PainterApp;
use crate::canvas::shader::{
    self, FrameUniforms, LiveLayout, LiveStatus, ShaderError, ShaderLayer, ShaderProgram,
};
use crate::canvas::storage::{LayerId, LayerKind};

/// While a shader can't show live, its still frame is re-baked at most
/// this often as it plays.
const FALLBACK_BAKE_EVERY: Duration = Duration::from_secs(1);

/// How long an edited shader must stay unchanged before its thumbnail is
/// re-baked (baking the whole canvas while typing would stutter).
const SETTLED_EDIT: Duration = Duration::from_millis(1500);

/// A shader layer's compiled shader.
#[derive(Default)]
pub struct Program {
    /// Of the source last compiled.
    pub hash: u64,
    pub errors: Vec<ShaderError>,
    /// The last source that compiled (kept on screen while edits fail).
    pub good: Option<Arc<ShaderProgram>>,
    /// When `good` last changed.
    changed: Option<Instant>,
}

/// A shader layer's playback.
#[derive(Clone, Copy, Debug)]
pub struct Clock {
    pub time: f32,
    pub playing: bool,
}

/// An open shader editor window.
pub struct Editor {
    pub id: LayerId,
    /// The text being edited (written to the layer as it's typed).
    pub text: String,
    /// Put the cursor on this line (1-based) next frame.
    pub goto_line: Option<usize>,
}

#[derive(Default)]
pub struct ShaderState {
    /// The GPU, for baking (egui-wgpu's render state).
    pub gpu: Option<egui_wgpu::RenderState>,
    pub programs: HashMap<LayerId, Program>,
    pub clocks: HashMap<LayerId, Clock>,
    pub editors: Vec<Editor>,
    /// Why shader layers show as still frames, when they do.
    pub not_live: Option<&'static str>,
    /// Shader edits made (not undo steps, but unsaved work all the same:
    /// counted into `PainterApp::doc_version`).
    pub edits: u64,
    /// `iFrame`.
    frame: u32,
    last_tick: Option<Instant>,
    /// Last `(program hash, time)` baked per layer.
    baked: HashMap<LayerId, (u64, u32)>,
    last_fallback_bake: Option<Instant>,
    /// Pointer on the canvas (canvas pixels), and whether a button is down.
    mouse: Option<(egui::Vec2, bool)>,
    click: egui::Vec2,
}

impl ShaderState {
    /// Drop everything tied to the document's layers (a new one replaces
    /// it; its layer ids may be the same).
    pub fn forget_document(&mut self) {
        self.programs.clear();
        self.clocks.clear();
        self.editors.clear();
        self.baked.clear();
        self.not_live = None;
    }

    pub fn clock(&self, id: LayerId) -> Option<Clock> {
        self.clocks.get(&id).copied()
    }

    /// `iMouse`: the pointer (canvas pixels, origin bottom-left), and where
    /// it was pressed (negative when released), as on Shadertoy.
    fn mouse_uniform(&self, height: f32) -> [f32; 4] {
        let (pos, down) = self.mouse.unwrap_or((egui::Vec2::ZERO, false));
        let click = egui::vec2(self.click.x, height - self.click.y);
        let sign = if down { 1.0 } else { -1.0 };
        [pos.x, height - pos.y, click.x * sign, click.y * sign]
    }
}

/// `iDate`: year, month, day aren't known without a calendar here; the
/// seconds into the (UTC) day are.
fn date_uniform() -> [f32; 4] {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    [0.0, 0.0, 0.0, (secs % 86_400.0) as f32]
}

impl PainterApp {
    pub(crate) fn is_shader_layer(&self, idx: usize) -> bool {
        self.canvas
            .layers
            .get(idx)
            .is_some_and(|l| l.shader.is_some())
    }

    /// Add a shader layer above the selected one, playing `source`.
    pub(crate) fn add_shader_layer(&mut self, name: &str, source: &str) -> Option<usize> {
        let idx = self.add_layer_with_tiles(format!("Shader: {name}"), Vec::new(), |layer| {
            layer.shader = Some(Box::new(ShaderLayer::new(source)));
            // Its pixels come from the shader: no painting on it.
            layer.locked = true;
        })?;
        let id = self.canvas.layers[idx].id;
        self.open_shader_editor(id);
        Some(idx)
    }

    pub(crate) fn open_shader_editor(&mut self, id: LayerId) {
        let shaders = &mut self.workspace.shaders;
        if shaders.editors.iter().any(|e| e.id == id) {
            return;
        }
        let Some(layer) = self.canvas.layers.iter().find(|l| l.id == id) else {
            return;
        };
        let Some(shader) = &layer.shader else {
            return;
        };
        shaders.editors.push(Editor {
            id,
            text: shader.source.clone(),
            goto_line: None,
        });
    }

    /// Replace shader layer `id`'s source (from its editor).
    pub(crate) fn set_shader_source(&mut self, id: LayerId, source: &str) {
        let Some(idx) = self.canvas.layer_index_of(id) else {
            return;
        };
        if self.canvas.layers[idx]
            .shader
            .as_ref()
            .is_some_and(|s| s.source != source)
            && let Some(s) = self.canvas_mut().layers[idx].shader.as_mut()
        {
            s.source = source.to_string();
            self.workspace.shaders.edits += 1;
        }
    }

    /// Set shader layer `id`'s playback speed.
    pub(crate) fn set_shader_speed(&mut self, id: LayerId, speed: f32) {
        if let Some(idx) = self.canvas.layer_index_of(id)
            && let Some(s) = self.canvas_mut().layers[idx].shader.as_mut()
        {
            s.speed = speed;
            self.workspace.shaders.edits += 1;
        }
    }

    /// Play or pause; pausing keeps the time in the document (so it saves).
    pub(crate) fn set_shader_playing(&mut self, id: LayerId, playing: bool) {
        let Some(clock) = self.workspace.shaders.clocks.get_mut(&id) else {
            return;
        };
        clock.playing = playing;
        let time = clock.time;
        if !playing
            && let Some(idx) = self.canvas.layer_index_of(id)
            && let Some(s) = self.canvas_mut().layers[idx].shader.as_mut()
        {
            s.time = time;
            self.workspace.shaders.edits += 1;
        }
    }

    pub(crate) fn set_shader_time(&mut self, id: LayerId, time: f32) {
        if let Some(clock) = self.workspace.shaders.clocks.get_mut(&id) {
            clock.time = time;
        }
        if let Some(idx) = self.canvas.layer_index_of(id)
            && let Some(s) = self.canvas_mut().layers[idx].shader.as_mut()
        {
            s.time = time;
            self.workspace.shaders.edits += 1;
        }
    }

    /// Once a frame, before the canvas is drawn: compile changed shaders,
    /// advance the clocks, and work out whether shader layers show live.
    /// Returns whether another frame is needed (something plays).
    pub(crate) fn shader_tick(
        &mut self,
        ctx: &egui::Context,
        pointer: Option<(egui::Vec2, bool)>,
    ) -> bool {
        let now = Instant::now();
        let shaders = &mut self.workspace.shaders;
        let dt = shaders
            .last_tick
            .map_or(0.0, |t| now.duration_since(t).as_secs_f32().min(0.25));
        shaders.last_tick = Some(now);
        if let Some((pos, down)) = pointer {
            if down && !shaders.mouse.is_some_and(|(_, was)| was) {
                shaders.click = pos;
            }
            shaders.mouse = Some((pos, down));
        }

        // Compile what changed; forget what's gone.
        let layers: Vec<(LayerId, &ShaderLayer, bool)> = self
            .canvas
            .layers
            .iter()
            .filter(|l| l.kind == LayerKind::Paint)
            .filter_map(|l| l.shader.as_deref().map(|s| (l.id, s, l.visible)))
            .collect();
        shaders
            .programs
            .retain(|id, _| layers.iter().any(|(i, _, _)| i == id));
        shaders
            .clocks
            .retain(|id, _| layers.iter().any(|(i, _, _)| i == id));
        shaders
            .editors
            .retain(|e| layers.iter().any(|(i, _, _)| *i == e.id));
        shaders
            .baked
            .retain(|id, _| layers.iter().any(|(i, _, _)| i == id));
        let mut playing = false;
        for &(id, layer, visible) in &layers {
            let program = shaders.programs.entry(id).or_default();
            let hash = shader::source_hash(&layer.source);
            if program.good.is_none() && program.errors.is_empty() || program.hash != hash {
                program.hash = hash;
                match shader::compile(&layer.source) {
                    Ok(p) => {
                        program.errors.clear();
                        program.good = Some(Arc::new(p));
                        program.changed = Some(now);
                    }
                    Err(errors) => program.errors = errors,
                }
            }
            let clock = shaders.clocks.entry(id).or_insert(Clock {
                time: layer.time,
                playing: true,
            });
            if clock.playing {
                clock.time += dt * layer.speed;
                playing |= visible;
            }
        }
        shaders.frame = shaders.frame.wrapping_add(1);
        let has_layers = !layers.is_empty();

        let status = if shaders.gpu.is_none() {
            if !has_layers {
                LiveStatus::None
            } else {
                LiveStatus::Unsupported("no GPU")
            }
        } else if self.workspace.select.quick_mask.is_some() {
            LiveStatus::Unsupported("the quick mask is on")
        } else {
            shader::live_layout(&self.canvas)
        };
        let live = match status {
            LiveStatus::Live(layout) => {
                self.workspace.shaders.not_live = None;
                Some(Arc::new(layout))
            }
            LiveStatus::Unsupported(why) => {
                self.workspace.shaders.not_live = Some(why);
                None
            }
            LiveStatus::None => {
                self.workspace.shaders.not_live = None;
                None
            }
        };
        let cache = &mut self.render_cache;
        let runs = |l: &Option<Arc<LiveLayout>>| l.as_ref().map(|l| l.runs.clone());
        let new_runs = runs(&cache.live) != runs(&live);
        // A shader layer's opacity or mode only changes the GPU steps.
        cache.live = live.clone();
        if new_runs {
            // Different runs, so different atlases: redraw everything.
            cache.texture_generation = cache.texture_generation.wrapping_add(1);
            self.mark_all_tiles_dirty();
        }
        if live.is_none() && has_layers {
            self.bake_for_screen();
        } else if live.is_some() && !self.brush_state.is_drawing {
            // Live layers' pixels are only for thumbnails and the like until
            // something bakes them: give new ones a first frame, and a new
            // one once an edited shader has settled for a moment.
            let shaders = &self.workspace.shaders;
            let unbaked: Vec<usize> = (0..self.canvas.layers.len())
                .filter(|&i| {
                    let l = &self.canvas.layers[i];
                    let Some(program) = shaders.programs.get(&l.id) else {
                        return false;
                    };
                    let Some(good) = &program.good else {
                        return false;
                    };
                    match shaders.baked.get(&l.id) {
                        None => true,
                        Some(&(hash, _)) => {
                            hash != good.hash
                                && program.changed.is_some_and(|t| t.elapsed() >= SETTLED_EDIT)
                        }
                    }
                })
                .collect();
            for idx in unbaked {
                if let Err(err) = self.bake_shader_layer(idx) {
                    log::warn!("baking a shader layer: {err}");
                    // Not retried until the shader changes.
                    let id = self.canvas.layers[idx].id;
                    let shaders = &mut self.workspace.shaders;
                    let hash = shaders
                        .programs
                        .get(&id)
                        .and_then(|p| p.good.as_ref())
                        .map_or(0, |p| p.hash);
                    shaders.baked.insert(id, (hash, 0));
                }
            }
        }
        if playing {
            ctx.request_repaint();
        }
        playing
    }

    /// Shader layers showing as still frames: bake the visible ones whose
    /// frame changed (at most every [`FALLBACK_BAKE_EVERY`] while playing).
    fn bake_for_screen(&mut self) {
        if self.brush_state.is_drawing {
            return;
        }
        let shaders = &self.workspace.shaders;
        let stale: Vec<usize> = (0..self.canvas.layers.len())
            .filter(|&i| {
                let l = &self.canvas.layers[i];
                l.visible
                    && l.shader.is_some()
                    && shaders.baked.get(&l.id) != self.bake_key(l.id).as_ref()
            })
            .collect();
        if stale.is_empty() {
            return;
        }
        let any_playing = stale.iter().any(|&i| {
            shaders
                .clock(self.canvas.layers[i].id)
                .is_some_and(|c| c.playing)
        });
        if any_playing
            && shaders
                .last_fallback_bake
                .is_some_and(|t| t.elapsed() < FALLBACK_BAKE_EVERY)
        {
            return;
        }
        self.workspace.shaders.last_fallback_bake = Some(Instant::now());
        for idx in stale {
            if let Err(err) = self.bake_shader_layer(idx) {
                log::warn!("baking a shader layer: {err}");
            }
        }
        self.mark_all_tiles_dirty();
    }

    /// What a bake of layer `id` depends on besides the layers below.
    fn bake_key(&self, id: LayerId) -> Option<(u64, u32)> {
        let shaders = &self.workspace.shaders;
        let program = shaders.programs.get(&id)?.good.as_ref()?;
        let time = shaders.clock(id)?.time;
        Some((program.hash, time.to_bits()))
    }

    /// Bake every shader layer's current frame into its pixels (before
    /// export, saving and merging).
    pub(crate) fn bake_shader_layers(&mut self) {
        let indices: Vec<usize> = (0..self.canvas.layers.len())
            .filter(|&i| self.is_shader_layer(i))
            .collect();
        if indices.is_empty() {
            return;
        }
        // Their clocks and programs, in case no frame ran since they came.
        for &idx in &indices {
            let id = self.canvas.layers[idx].id;
            let layer = self.canvas.layers[idx].shader.as_deref().cloned();
            let Some(layer) = layer else { continue };
            let shaders = &mut self.workspace.shaders;
            shaders.clocks.entry(id).or_insert(Clock {
                time: layer.time,
                playing: true,
            });
            let program = shaders.programs.entry(id).or_default();
            if program.good.is_none()
                && let Ok(p) = shader::compile(&layer.source)
            {
                program.hash = p.hash;
                program.good = Some(Arc::new(p));
            }
        }
        for idx in indices {
            if let Err(err) = self.bake_shader_layer(idx) {
                log::warn!("baking a shader layer: {err}");
            }
        }
        if self.render_cache.live.is_none() {
            self.mark_all_tiles_dirty();
        }
    }

    /// Render shader layer `idx`'s current frame into its tiles (no undo:
    /// the tiles only mirror the shader).
    pub(crate) fn bake_shader_layer(&mut self, idx: usize) -> Result<(), String> {
        let id = self.canvas.layers[idx].id;
        let key = self.bake_key(id).ok_or("the shader doesn't compile")?;
        let shaders = &self.workspace.shaders;
        let program = shaders
            .programs
            .get(&id)
            .and_then(|p| p.good.clone())
            .ok_or("the shader doesn't compile")?;
        let clock = shaders.clock(id).ok_or("no clock")?;
        let gpu = shaders.gpu.clone().ok_or("no GPU")?;
        let (w, h) = (self.canvas.width(), self.canvas.height());
        let below = shader::below_view(&self.canvas, idx).flatten();
        let frame = FrameUniforms {
            mouse: shaders.mouse_uniform(h as f32),
            date: date_uniform(),
            time: [clock.time, 0.0, shaders.frame as f32, 60.0],
            ..Default::default()
        };
        let pixels = {
            let mut renderer = gpu.renderer.write();
            let canvas_gpu = renderer
                .callback_resources
                .get_mut::<crate::app::view::gpu_canvas::GpuCanvas>()
                .ok_or("no GPU canvas")?;
            canvas_gpu.shaders_mut().bake(
                &gpu.device,
                &gpu.queue,
                id,
                &program,
                frame,
                w,
                h,
                &below.pixels,
            )?
        };
        let ts = self.canvas.tile_size();
        for ty in 0..h.div_ceil(ts) {
            for tx in 0..w.div_ceil(ts) {
                let mut data = vec![egui::Color32::TRANSPARENT; ts * ts];
                for row in 0..ts.min(h - ty * ts) {
                    let src = (ty * ts + row) * w + tx * ts;
                    let n = ts.min(w - tx * ts);
                    data[row * ts..row * ts + n].copy_from_slice(&pixels[src..src + n]);
                }
                self.canvas
                    .set_layer_tile_data(idx, tx as i32, ty as i32, data);
            }
        }
        self.workspace.shaders.baked.insert(id, key);
        self.layer_state.thumbnails_dirty = true;
        Ok(())
    }

    /// This frame's live composite, for the canvas paint callback.
    pub(crate) fn shader_compose_steps(
        &self,
        layout: &LiveLayout,
    ) -> Vec<crate::app::view::shader_gpu::ComposeStep> {
        use crate::app::view::shader_gpu::ComposeStep;
        let shaders = &self.workspace.shaders;
        layout
            .steps
            .iter()
            .filter_map(|step| match step {
                shader::LiveStep::Run { run, blend } => Some(ComposeStep::Run {
                    run: *run,
                    blend: *blend,
                }),
                shader::LiveStep::Shader { id, blend, opacity } => {
                    let program = shaders.programs.get(id)?.good.clone()?;
                    let clock = shaders.clock(*id)?;
                    Some(ComposeStep::Shader {
                        id: *id,
                        program,
                        time: [clock.time, 1.0 / 60.0, shaders.frame as f32, 60.0],
                        blend: *blend,
                        opacity: *opacity,
                    })
                }
            })
            .collect()
    }

    /// Frame uniforms shared by the live shaders (mouse, date).
    pub(crate) fn shader_frame_base(&self) -> FrameUniforms {
        let h = self.canvas.height() as f32;
        FrameUniforms {
            resolution: [self.canvas.width() as f32, h, 1.0, 0.0],
            mouse: self.workspace.shaders.mouse_uniform(h),
            date: date_uniform(),
            ..Default::default()
        }
    }
}
