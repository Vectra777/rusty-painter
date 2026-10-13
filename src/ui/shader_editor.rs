//! Shader layer editors: one floating window per shader layer being
//! edited, with its code (compiled as it's typed; the canvas keeps showing
//! the last version that worked), its errors, playback and templates.

use crate::PainterApp;
use crate::canvas::shader::{ShaderError, TEMPLATES};
use crate::canvas::storage::LayerId;
use crate::ui::style::*;
use crate::ui::widgets::FitScreen;
use eframe::egui::{self, Color32, RichText, text::LayoutJob};

const KEYWORD: Color32 = Color32::from_rgb(198, 120, 221);
const TYPE: Color32 = Color32::from_rgb(86, 182, 194);
const BUILTIN: Color32 = Color32::from_rgb(97, 175, 239);
const NUMBER: Color32 = Color32::from_rgb(209, 154, 102);
const COMMENT: Color32 = Color32::from_gray(110);
const PREPROCESSOR: Color32 = Color32::from_rgb(224, 108, 117);
const ERROR: Color32 = Color32::from_rgb(235, 90, 90);
const ERROR_LINE: Color32 = Color32::from_rgba_premultiplied(90, 20, 20, 90);

const KEYWORDS: &[&str] = &[
    "if",
    "else",
    "for",
    "while",
    "do",
    "return",
    "break",
    "continue",
    "discard",
    "const",
    "in",
    "out",
    "inout",
    "uniform",
    "struct",
    "switch",
    "case",
    "default",
    "true",
    "false",
    "precision",
    "highp",
    "mediump",
    "lowp",
];
const TYPES: &[&str] = &[
    "void",
    "bool",
    "int",
    "uint",
    "float",
    "vec2",
    "vec3",
    "vec4",
    "ivec2",
    "ivec3",
    "ivec4",
    "bvec2",
    "bvec3",
    "bvec4",
    "mat2",
    "mat3",
    "mat4",
    "sampler2D",
];
const BUILTINS: &[&str] = &[
    "iTime",
    "iTimeDelta",
    "iFrame",
    "iFrameRate",
    "iResolution",
    "iMouse",
    "iDate",
    "iChannel0",
    "mainImage",
    "fragColor",
    "fragCoord",
    "texture",
    "textureLod",
    "sin",
    "cos",
    "tan",
    "asin",
    "acos",
    "atan",
    "pow",
    "exp",
    "log",
    "exp2",
    "log2",
    "sqrt",
    "inversesqrt",
    "abs",
    "sign",
    "floor",
    "ceil",
    "fract",
    "mod",
    "min",
    "max",
    "clamp",
    "mix",
    "step",
    "smoothstep",
    "length",
    "distance",
    "dot",
    "cross",
    "normalize",
    "reflect",
    "refract",
];

/// Syntax-coloured layout of GLSL `code`, error lines tinted, no wrapping
/// (so the line numbers beside it stay aligned).
fn highlight(ui: &egui::Ui, code: &str, error_lines: &[usize]) -> LayoutJob {
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let mut job = LayoutJob::default();
    job.wrap.max_width = f32::INFINITY;
    for (n, line) in code.split_inclusive('\n').enumerate() {
        let background = if error_lines.contains(&(n + 1)) {
            ERROR_LINE
        } else {
            Color32::TRANSPARENT
        };
        let mut push = |text: &str, color: Color32| {
            job.append(
                text,
                0.0,
                egui::TextFormat {
                    font_id: font.clone(),
                    color,
                    background,
                    ..Default::default()
                },
            );
        };
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            push(line, PREPROCESSOR);
            continue;
        }
        let mut rest = line;
        while !rest.is_empty() {
            if let Some(comment) = rest.strip_prefix("//").map(|_| rest) {
                push(comment, COMMENT);
                break;
            }
            let c = rest.chars().next().unwrap_or(' ');
            let len = if c.is_ascii_alphabetic() || c == '_' {
                let end = rest
                    .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
                    .unwrap_or(rest.len());
                let word = &rest[..end];
                let color = if KEYWORDS.contains(&word) {
                    KEYWORD
                } else if TYPES.contains(&word) {
                    TYPE
                } else if BUILTINS.contains(&word) {
                    BUILTIN
                } else {
                    TEXT
                };
                push(word, color);
                end
            } else if c.is_ascii_digit()
                || (c == '.' && rest[1..].starts_with(|d: char| d.is_ascii_digit()))
            {
                let end = rest
                    .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '.'))
                    .unwrap_or(rest.len());
                push(&rest[..end], NUMBER);
                end
            } else {
                let end = rest
                    .char_indices()
                    .skip(1)
                    .find(|&(i, ch)| {
                        ch.is_ascii_alphanumeric() || ch == '_' || rest[i..].starts_with("//")
                    })
                    .map_or(rest.len(), |(i, _)| i);
                push(&rest[..end], TEXT);
                end
            };
            rest = &rest[len..];
        }
    }
    job
}

/// Character index where 1-based `line` starts in `text`.
fn line_start(text: &str, line: usize) -> usize {
    text.split_inclusive('\n')
        .take(line.saturating_sub(1))
        .map(|l| l.chars().count())
        .sum()
}

/// Every open shader editor window.
pub fn shader_editors(app: &mut PainterApp, ctx: &egui::Context) {
    let ids: Vec<LayerId> = app.workspace.shaders.editors.iter().map(|e| e.id).collect();
    for id in ids {
        shader_editor(app, ctx, id);
    }
}

fn shader_editor(app: &mut PainterApp, ctx: &egui::Context, id: LayerId) {
    let Some(idx) = app.canvas.layer_index_of(id) else {
        return;
    };
    let layer = &app.canvas.layers[idx];
    let Some(shader) = layer.shader.as_deref() else {
        return;
    };
    let name = layer.name.clone();
    let speed = shader.speed;
    let shaders = &app.workspace.shaders;
    let clock = shaders.clock(id);
    let errors: Vec<ShaderError> = shaders
        .programs
        .get(&id)
        .map(|p| p.errors.clone())
        .unwrap_or_default();
    let has_program = shaders.programs.get(&id).is_some_and(|p| p.good.is_some());
    let not_live = shaders.not_live;
    let Some(editor_idx) = shaders.editors.iter().position(|e| e.id == id) else {
        return;
    };

    let mut open = true;
    let mut new_source: Option<String> = None;
    let mut set_playing = None;
    let mut set_time = None;
    let mut new_speed = speed;
    let mut bake = false;
    egui::Window::new(name)
        .fit_screen_size(ctx)
        .id(egui::Id::new(("shader editor", id.0)))
        .open(&mut open)
        .resizable(true)
        .collapsible(true)
        .default_size([540.0, 460.0])
        .min_size([280.0, 200.0])
        .show(ctx, |ui| {
            // Playback, speed, templates.
            ui.horizontal(|ui| {
                if let Some(clock) = clock {
                    let (label, tip) = if clock.playing {
                        ("⏸", "Pause")
                    } else {
                        ("▶", "Play")
                    };
                    if ui.button(label).on_hover_text(tip).clicked() {
                        set_playing = Some(!clock.playing);
                    }
                    if ui
                        .button("⏮")
                        .on_hover_text("Back to the start (iTime = 0)")
                        .clicked()
                    {
                        set_time = Some(0.0);
                    }
                    ui.label(
                        RichText::new(format!("{:.1} s", clock.time))
                            .monospace()
                            .color(TEXT_DIM),
                    );
                }
                ui.add(
                    egui::DragValue::new(&mut new_speed)
                        .range(0.0..=10.0)
                        .speed(0.01)
                        .prefix("speed ")
                        .suffix("×"),
                )
                .on_hover_text("Shader seconds per second");
                ui.menu_button("Templates", |ui| {
                    for (template, source) in TEMPLATES {
                        if ui.button(*template).clicked() {
                            new_source = Some(source.to_string());
                            ui.close_menu();
                        }
                    }
                });
                if ui
                    .add_enabled(has_program, egui::Button::new("Bake"))
                    .on_hover_text(
                        "Render this frame into the layer's pixels now (export, merging and \
                         saving do this themselves)",
                    )
                    .clicked()
                {
                    bake = true;
                }
            });
            if let Some(why) = not_live {
                ui.label(
                    RichText::new(format!(
                        "Showing a still frame (updated every second): {why}."
                    ))
                    .small()
                    .color(TEXT_DIM),
                );
            }

            // Errors, clickable to jump to their line.
            let mut goto = None;
            if !errors.is_empty() {
                egui::ScrollArea::vertical()
                    .id_salt("errors")
                    .max_height(72.0)
                    .show(ui, |ui| {
                        for error in &errors {
                            let text = RichText::new(error.to_string()).small().color(ERROR);
                            let response =
                                ui.add(egui::Label::new(text).sense(egui::Sense::click()));
                            if error.line > 0 && response.on_hover_text("Go to this line").clicked()
                            {
                                goto = Some(error.line);
                            }
                        }
                    });
                if has_program {
                    ui.label(
                        RichText::new("The canvas shows the last version that worked.")
                            .small()
                            .color(TEXT_DIM),
                    );
                }
            }

            // The code, with line numbers.
            let editor = &mut app.workspace.shaders.editors[editor_idx];
            if let Some(source) = &new_source {
                editor.text = source.clone();
            }
            if goto.is_some() {
                editor.goto_line = goto;
            }
            let error_lines: Vec<usize> = errors.iter().map(|e| e.line).collect();
            egui::ScrollArea::both()
                .id_salt("code")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.horizontal_top(|ui| {
                        let lines = editor.text.split('\n').count().max(1);
                        let numbers: String = (1..=lines)
                            .map(|n| format!("{n:>3}\n"))
                            .collect::<String>()
                            .trim_end()
                            .to_string();
                        ui.add(
                            egui::Label::new(RichText::new(numbers).monospace().color(COMMENT))
                                .selectable(false),
                        );
                        let mut layouter = |ui: &egui::Ui, text: &str, _wrap: f32| {
                            ui.fonts(|f| f.layout_job(highlight(ui, text, &error_lines)))
                        };
                        let output = egui::TextEdit::multiline(&mut editor.text)
                            .code_editor()
                            .desired_width(f32::INFINITY)
                            .desired_rows(24)
                            .lock_focus(true)
                            .layouter(&mut layouter)
                            .show(ui);
                        if output.response.changed() {
                            new_source = Some(editor.text.clone());
                        }
                        if let Some(line) = editor.goto_line.take() {
                            let mut state = output.state;
                            let at = egui::text::CCursor::new(line_start(&editor.text, line));
                            state
                                .cursor
                                .set_char_range(Some(egui::text::CCursorRange::one(at)));
                            state.store(ui.ctx(), output.response.id);
                            output.response.request_focus();
                        }
                    });
                });
        });

    if let Some(source) = new_source {
        app.set_shader_source(id, &source);
    }
    if new_speed != speed {
        app.set_shader_speed(id, new_speed);
    }
    if let Some(playing) = set_playing {
        app.set_shader_playing(id, playing);
    }
    if let Some(time) = set_time {
        app.set_shader_time(id, time);
    }
    if bake {
        if let Err(err) = app.bake_shader_layer(idx) {
            log::warn!("baking a shader layer: {err}");
        }
        if app.render_cache.live.is_none() {
            app.mark_all_tiles_dirty();
        }
    }
    if !open {
        app.workspace.shaders.editors.retain(|e| e.id != id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_starts_count_characters() {
        let text = "ab\ncdé\nf";
        assert_eq!(line_start(text, 1), 0);
        assert_eq!(line_start(text, 2), 3);
        assert_eq!(line_start(text, 3), 7);
    }

    #[test]
    fn highlighting_keeps_every_character() {
        let ctx = egui::Context::default();
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                for (_, source) in TEMPLATES {
                    let job = highlight(ui, source, &[2]);
                    assert_eq!(job.text, *source);
                }
            });
        });
    }
}
