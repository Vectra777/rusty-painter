//this is a POC for a GPU accelerated painting application using wgpu and egui
use eframe::egui;
use glam::{Mat4, Vec3, Vec4};
use wgpu::util::DeviceExt;

use egui_wgpu::wgpu;
use egui_winit::{
    winit::{
        self,
        event::{ElementState, Event, MouseButton, MouseScrollDelta, WindowEvent, Modifiers},
        event_loop::EventLoop,
        window::{Window, WindowAttributes},
    },
    State as EguiWinitState,
};

const BRUSH_WGSL: &str = include_str!("./shaders/brush.wgsl");
const SQUARE_BRUSH_WGSL: &str = include_str!("./shaders/square_brush.wgsl");
const QUAD_WGSL: &str = include_str!("./shaders/quad.wgsl");

#[derive(PartialEq)]
enum BrushType {
    Circle,
    Square,
}

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    position: [f32; 3],
    tex_coords: [f32; 2],
}

impl Vertex {
    fn desc<'a>() -> wgpu::VertexBufferLayout<'a> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 0,
                    format: wgpu::VertexFormat::Float32x3,
                },
                wgpu::VertexAttribute {
                    offset: std::mem::size_of::<[f32; 3]>() as wgpu::BufferAddress,
                    shader_location: 1,
                    format: wgpu::VertexFormat::Float32x2,
                },
            ],
        }
    }
}

const VERTICES: &[Vertex] = &[
    Vertex {
        position: [-1.0, -1.0, 0.0],
        tex_coords: [0.0, 1.0],
    },
    Vertex {
        position: [1.0, -1.0, 0.0],
        tex_coords: [1.0, 1.0],
    },
    Vertex {
        position: [1.0, 1.0, 0.0],
        tex_coords: [1.0, 0.0],
    },
    Vertex {
        position: [-1.0, 1.0, 0.0],
        tex_coords: [0.0, 0.0],
    },
];

const INDICES: &[u16] = &[0, 1, 2, 0, 2, 3];

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct Brush {
    color: [f32; 4],
    position: [f32; 2],
    radius: f32,
    _padding: u32,
}

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct View {
    view_proj: [[f32; 4]; 4],
}

struct History {
    textures: Vec<wgpu::Texture>,
    current_index: usize,
}

impl History {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue, initial_texture: &wgpu::Texture) -> Self {
        let mut history = History {
            textures: Vec::new(),
            current_index: 0,
        };
        history.push(device, queue, initial_texture);
        history
    }

    fn push(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture) {
        let texture_copy = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("History Texture"),
            size: texture.size(),
            mip_level_count: texture.mip_level_count(),
            sample_count: texture.sample_count(),
            dimension: texture.dimension(),
            format: texture.format(),
            usage: texture.usage(),
            view_formats: &[],
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("History Push Encoder"),
        });
        encoder.copy_texture_to_texture(texture.as_image_copy(), texture_copy.as_image_copy(), texture.size());
        queue.submit(std::iter::once(encoder.finish()));

        let len = self.textures.len();
        if self.current_index + 1 < len {
            self.textures.truncate(self.current_index + 1);
        }
        self.textures.push(texture_copy);
        self.current_index = self.textures.len() - 1;
    }

    fn undo(&mut self) -> Option<&wgpu::Texture> {
        if self.current_index > 0 {
            self.current_index -= 1;
            self.textures.get(self.current_index)
        } else {
            None
        }
    }

    fn redo(&mut self) -> Option<&wgpu::Texture> {
        if self.current_index < self.textures.len() - 1 {
            self.current_index += 1;
            self.textures.get(self.current_index)
        } else {
            None
        }
    }
}

struct State {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    size: winit::dpi::PhysicalSize<u32>,
    max_texture_dimension: u32,
    canvas_width: u32,
    canvas_height: u32,
    canvas_texture: wgpu::Texture,
    canvas_texture_view: wgpu::TextureView,
    texture_bind_group_layout: wgpu::BindGroupLayout,
    diffuse_sampler: wgpu::Sampler,
    render_pipeline: wgpu::RenderPipeline,
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    num_indices: u32,
    diffuse_bind_group: wgpu::BindGroup,
    brush_pipeline: wgpu::RenderPipeline,
    square_brush_pipeline: wgpu::RenderPipeline,
    brush_uniform_buffer: wgpu::Buffer,
    brush_bind_group: wgpu::BindGroup,
    mouse_down: bool,
    middle_mouse_down: bool,
    last_mouse_position: (f32, f32),
    mouse_position: (f32, f32),
    egui_ctx: egui::Context,
    egui_state: EguiWinitState,
    egui_renderer: egui_wgpu::Renderer,
    brush_color: [f32; 4],
    brush_radius_px: f32,
    brush_spacing: f32,
    last_paint_position: Option<[f32; 2]>,
    brush_type: BrushType,
    zoom: f32,
    pan: [f32; 2],
    rotation: f32,
    view_matrix: Mat4,
    inv_view_matrix: Mat4,
    view_uniform_buffer: wgpu::Buffer,
    view_bind_group: wgpu::BindGroup,
    history: History,
    modifiers: Modifiers,
}

impl State {
    async fn new(window: &Window) -> Self {
        let size = window.inner_size();

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });

        let surface = instance.create_surface(window).unwrap();
        let surface = unsafe { std::mem::transmute(surface) };

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::default(),
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .unwrap();

        let limits = adapter.limits();

        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: None,
                    required_features: wgpu::Features::empty(),
                    required_limits: limits.clone(),
                    memory_hints: wgpu::MemoryHints::Performance,
                },
                None,
            )
            .await
            .unwrap();

        let surface_caps = surface.get_capabilities(&adapter);
        let surface_format = surface_caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(surface_caps.formats[0]);

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: size.width,
            height: size.height,
            present_mode: surface_caps.present_modes[0],
            alpha_mode: surface_caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let max_texture_dim = limits.max_texture_dimension_2d;
        let canvas_dim = 10000u32.min(max_texture_dim);
        if canvas_dim < 10000 {
            println!("Warning: Requested canvas size of 10000x10000 exceeds the hardware limit of {}x{}. Canvas will be created with size {}x{}", max_texture_dim, max_texture_dim, canvas_dim, canvas_dim);
        }
        let canvas_width = canvas_dim;
        let canvas_height = canvas_dim;

        let canvas_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Canvas Texture"),
            size: wgpu::Extent3d {
                width: canvas_width,
                height: canvas_height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });

        let canvas_texture_view = canvas_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let diffuse_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let view_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("View Uniform Buffer"),
            size: std::mem::size_of::<View>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let view_bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
            label: Some("view_bind_group_layout"),
        });

        let view_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            layout: &view_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: view_uniform_buffer.as_entire_binding(),
            }],
            label: Some("view_bind_group"),
        });

        let texture_bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
            label: Some("texture_bind_group_layout"),
        });

        let diffuse_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            layout: &texture_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&canvas_texture_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&diffuse_sampler),
                },
            ],
            label: Some("diffuse_bind_group"),
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Shader"),
            source: wgpu::ShaderSource::Wgsl(QUAD_WGSL.into()),
        });

        let render_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Render Pipeline Layout"),
            bind_group_layouts: &[&texture_bind_group_layout, &view_bind_group_layout],
            push_constant_ranges: &[],
        });

        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Render Pipeline"),
            layout: Some(&render_pipeline_layout),
            cache: None,
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[Vertex::desc()],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: Some(wgpu::Face::Back),
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState {
                count: 1,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview: None,
        });

        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Vertex Buffer"),
            contents: bytemuck::cast_slice(VERTICES),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Index Buffer"),
            contents: bytemuck::cast_slice(INDICES),
            usage: wgpu::BufferUsages::INDEX,
        });
        let num_indices = INDICES.len() as u32;

        let brush_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Brush Shader"),
            source: wgpu::ShaderSource::Wgsl(BRUSH_WGSL.into()),
        });

        let square_brush_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Square Brush Shader"),
            source: wgpu::ShaderSource::Wgsl(SQUARE_BRUSH_WGSL.into()),
        });

        let brush_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Brush Uniform Buffer"),
            size: std::mem::size_of::<Brush>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let brush_bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
            label: Some("brush_bind_group_layout"),
        });

        let brush_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            layout: &brush_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: brush_uniform_buffer.as_entire_binding(),
            }],
            label: Some("brush_bind_group"),
        });

        let brush_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Brush Pipeline Layout"),
            bind_group_layouts: &[&brush_bind_group_layout, &view_bind_group_layout],
            push_constant_ranges: &[],
        });

        let brush_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Brush Pipeline"),
            layout: Some(&brush_pipeline_layout),
            cache: None,
            vertex: wgpu::VertexState {
                module: &brush_shader,
                entry_point: "vs_main",
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &brush_shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format: canvas_texture.format(),
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });

        let square_brush_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Square Brush Pipeline"),
            layout: Some(&brush_pipeline_layout),
            cache: None,
            vertex: wgpu::VertexState {
                module: &square_brush_shader,
                entry_point: "vs_main",
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &square_brush_shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format: canvas_texture.format(),
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });

        let egui_ctx = egui::Context::default();
        let egui_state = EguiWinitState::new(
            egui_ctx.clone(),
            egui_ctx.viewport_id(),
            window,
            Some(window.scale_factor() as f32),
            None,
            None,
        );
        let egui_renderer = egui_wgpu::Renderer::new(&device, config.format, None, 1, false);

        {
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Initial Canvas Clear Encoder"),
            });
            encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Initial Canvas Clear Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &canvas_texture_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 1.0,
                            g: 1.0,
                            b: 1.0,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            });
            queue.submit(std::iter::once(encoder.finish()));
        }

        let history = History::new(&device, &queue, &canvas_texture);

        let mut s = Self {
            surface,
            device,
            queue,
            config,
            size,
            max_texture_dimension: max_texture_dim,
            canvas_width,
            canvas_height,
            canvas_texture,
            canvas_texture_view,
            texture_bind_group_layout,
            diffuse_sampler,
            render_pipeline,
            vertex_buffer,
            index_buffer,
            num_indices,
            diffuse_bind_group,
            brush_pipeline,
            square_brush_pipeline,
            brush_uniform_buffer,
            brush_bind_group,
            mouse_down: false,
            middle_mouse_down: false,
            last_mouse_position: (0.0, 0.0),
            mouse_position: (0.0, 0.0),
            egui_ctx,
            egui_state,
            egui_renderer,
            brush_color: [1.0, 0.0, 0.0, 1.0],
            brush_radius_px: 20.0,
            brush_spacing: 0.25,
            last_paint_position: None,
            brush_type: BrushType::Circle,
            zoom: 1.0,
            pan: [0.0, 0.0],
            rotation: 0.0,
            view_matrix: Mat4::IDENTITY,
            inv_view_matrix: Mat4::IDENTITY,
            view_uniform_buffer,
            view_bind_group,
            history,
            modifiers: Modifiers::default(),
        };
        s.clear_canvas();
        s
    }

    fn resize(&mut self, new_size: winit::dpi::PhysicalSize<u32>) {
        if new_size.width > 0 && new_size.height > 0 {
            self.size = new_size;
            self.config.width = new_size.width;
            self.config.height = new_size.height;
            self.surface.configure(&self.device, &self.config);
        }
    }

    fn input(&mut self, event: &WindowEvent, window: &Window) -> bool {
        let event_response = self.egui_state.on_window_event(window, event);
        if event_response.consumed {
            return true;
        }
        match event {
            WindowEvent::ModifiersChanged(mods) => {
                self.modifiers = *mods;
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let scroll = match delta {
                    MouseScrollDelta::LineDelta(_, y) => *y,
                    MouseScrollDelta::PixelDelta(pos) => pos.y as f32,
                };
                self.zoom = (self.zoom * (1.0 + scroll * 0.05)).clamp(0.1, 20.0);
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if *button == MouseButton::Left {
                    if *state == ElementState::Released {
                        self.history.push(&self.device, &self.queue, &self.canvas_texture);
                        self.last_paint_position = None;
                    } else {
                        self.last_paint_position = None;
                    }
                    self.mouse_down = *state == ElementState::Pressed;
                }
                if *button == MouseButton::Middle {
                    self.middle_mouse_down = *state == ElementState::Pressed;
                    self.last_mouse_position = self.mouse_position;
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.mouse_position = (position.x as f32, position.y as f32);
                if self.middle_mouse_down {
                    self.pan[0] += (self.mouse_position.0 - self.last_mouse_position.0)
                        / self.size.width as f32;
                    self.pan[1] += (self.mouse_position.1 - self.last_mouse_position.1)
                        / self.size.height as f32;
                    self.last_mouse_position = self.mouse_position;
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    winit::event::KeyEvent {
                        physical_key: winit::keyboard::PhysicalKey::Code(key_code),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => match key_code {
                winit::keyboard::KeyCode::KeyZ => {
                    if self.modifiers.state().control_key() {
                        if let Some(texture) = self.history.undo() {
                            let mut encoder = self.device.create_command_encoder(
                                &wgpu::CommandEncoderDescriptor {
                                    label: Some("Undo Encoder"),
                                },
                            );
                            encoder.copy_texture_to_texture(
                                texture.as_image_copy(),
                                self.canvas_texture.as_image_copy(),
                                texture.size(),
                            );
                            self.queue.submit(std::iter::once(encoder.finish()));
                        }
                    }
                }
                winit::keyboard::KeyCode::KeyY => {
                    if self.modifiers.state().control_key() {
                        if let Some(texture) = self.history.redo() {
                            let mut encoder = self.device.create_command_encoder(
                                &wgpu::CommandEncoderDescriptor {
                                    label: Some("Redo Encoder"),
                                },
                            );
                            encoder.copy_texture_to_texture(
                                texture.as_image_copy(),
                                self.canvas_texture.as_image_copy(),
                                texture.size(),
                            );
                            self.queue.submit(std::iter::once(encoder.finish()));
                        }
                    }
                }
                _ => {}
            },
            _ => {}
        }
        false
    }

    fn update(&mut self) {
        self.zoom = self.zoom.clamp(0.1, 20.0);
        let translation = Mat4::from_translation(Vec3::new(self.pan[0], self.pan[1], 0.0));
        let rotation = Mat4::from_rotation_z(self.rotation);
        let scale = Mat4::from_scale(Vec3::new(self.zoom, self.zoom, 1.0));
        let view_proj = translation * rotation * scale;
        self.view_matrix = view_proj;
        self.inv_view_matrix = view_proj.inverse();
        let view = View {
            view_proj: view_proj.to_cols_array_2d(),
        };
        self.queue
            .write_buffer(&self.view_uniform_buffer, 0, bytemuck::cast_slice(&[view]));
    }

    fn cursor_to_canvas_coords(&self) -> Option<[f32; 2]> {
        if self.size.width == 0 || self.size.height == 0 {
            return None;
        }
        let clip_space = Vec4::new(
            self.mouse_position.0 / self.size.width as f32 * 2.0 - 1.0,
            1.0 - self.mouse_position.1 / self.size.height as f32 * 2.0,
            0.0,
            1.0,
        );
        let transformed = self.inv_view_matrix * clip_space;
        if transformed.w.abs() < f32::EPSILON {
            return None;
        }
        let base_clip = transformed / transformed.w;
        let world_x = (base_clip.x + 1.0) * 0.5;
        let world_y = (1.0 - base_clip.y) * 0.5;
        Some([world_x, world_y])
    }

    fn clear_canvas(&mut self) {
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Clear Canvas Encoder"),
        });
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Canvas Clear Pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.canvas_texture_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 1.0,
                        g: 1.0,
                        b: 1.0,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            occlusion_query_set: None,
            timestamp_writes: None,
        });
        self.queue.submit(std::iter::once(encoder.finish()));
    }

    fn recreate_canvas_texture(&mut self, width: u32, height: u32) {
        let new_width = width.clamp(1, self.max_texture_dimension);
        let new_height = height.clamp(1, self.max_texture_dimension);
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Canvas Texture"),
            size: wgpu::Extent3d {
                width: new_width,
                height: new_height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let texture_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let diffuse_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            layout: &self.texture_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&texture_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.diffuse_sampler),
                },
            ],
            label: Some("diffuse_bind_group"),
        });

        self.canvas_width = new_width;
        self.canvas_height = new_height;
        self.canvas_texture = texture;
        self.canvas_texture_view = texture_view;
        self.diffuse_bind_group = diffuse_bind_group;
        self.pan = [0.0, 0.0];
        self.zoom = 1.0;
        self.rotation = 0.0;
        self.last_paint_position = None;
        self.clear_canvas();
        self.history = History::new(&self.device, &self.queue, &self.canvas_texture);
        self.update();
    }

    fn render(&mut self, window: &Window) -> Result<(), wgpu::SurfaceError> {
        let output = self.surface.get_current_texture()?;
        let view = output.texture.create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Render Encoder"),
        });

        if self.mouse_down && !self.egui_ctx.is_pointer_over_area() {
            if let Some(mut position) = self.cursor_to_canvas_coords() {
                position[0] = position[0].clamp(0.0, 1.0);
                position[1] = position[1].clamp(0.0, 1.0);
                let denom = self.canvas_width.min(self.canvas_height) as f32;
                let radius_px = self.brush_radius_px.min(denom);
                let radius = radius_px / denom;
                let spacing_distance = radius * self.brush_spacing;
                let should_paint = match self.last_paint_position {
                    None => true,
                    Some(last) => {
                        let dx = position[0] - last[0];
                        let dy = position[1] - last[1];
                        dx * dx + dy * dy >= spacing_distance * spacing_distance
                    }
                };

                if should_paint {
                    let brush = Brush {
                        position,
                        color: self.brush_color,
                        radius,
                        _padding: 0,
                    };
                    self.queue
                        .write_buffer(&self.brush_uniform_buffer, 0, bytemuck::cast_slice(&[brush]));

                    let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("Brush Pass"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &self.canvas_texture_view,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Load,
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        occlusion_query_set: None,
                        timestamp_writes: None,
                    });

                    match self.brush_type {
                        BrushType::Circle => render_pass.set_pipeline(&self.brush_pipeline),
                        BrushType::Square => render_pass.set_pipeline(&self.square_brush_pipeline),
                    }
                    render_pass.set_bind_group(0, &self.brush_bind_group, &[]);
                    render_pass.set_bind_group(1, &self.view_bind_group, &[]);
                    render_pass.draw(0..4, 0..1);
                    self.last_paint_position = Some(position);
                }
            }
        }

        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.1,
                            g: 0.2,
                            b: 0.3,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            });

            render_pass.set_pipeline(&self.render_pipeline);
            render_pass.set_bind_group(0, &self.diffuse_bind_group, &[]);
            render_pass.set_bind_group(1, &self.view_bind_group, &[]);
            render_pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
            render_pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint16);
            render_pass.draw_indexed(0..self.num_indices, 0, 0..1);
        }

        let raw_input = self.egui_state.take_egui_input(window);
        let mut clear_canvas = false;
        let mut undo = false;
        let mut redo = false;
        let mut create_new_canvas = false;
        let full_output = self.egui_ctx.run(raw_input, |ctx| {
            egui::Window::new("Brush Settings").show(ctx, |ui| {
                ui.label("Color");
                ui.color_edit_button_rgba_unmultiplied(&mut self.brush_color);
                ui.separator();
                ui.label("Brush Size (px)");
                ui.add(egui::Slider::new(&mut self.brush_radius_px, 1.0..=1000.0));
                ui.label("Spacing (x radius)");
                ui.add(egui::Slider::new(&mut self.brush_spacing, 0.0..=2.0));
                ui.separator();
                ui.label("Brush Type");
                ui.radio_value(&mut self.brush_type, BrushType::Circle, "Circle");
                ui.radio_value(&mut self.brush_type, BrushType::Square, "Square");
                if ui.button("Clear Canvas").clicked() {
                    clear_canvas = true;
                }
                ui.separator();
                ui.label("View");
                ui.add(egui::Slider::new(&mut self.zoom, 0.1..=20.0).text("Zoom"));
                ui.horizontal(|ui| {
                    ui.label("Pan");
                    ui.add(
                        egui::DragValue::new(&mut self.pan[0])
                            .speed(0.01)
                            .clamp_range(-2.0..=2.0)
                            .prefix("X "),
                    );
                    ui.add(
                        egui::DragValue::new(&mut self.pan[1])
                            .speed(0.01)
                            .clamp_range(-2.0..=2.0)
                            .prefix("Y "),
                    );
                });
                let mut rotation_degrees = self.rotation.to_degrees();
                if ui
                    .add(egui::Slider::new(&mut rotation_degrees, -180.0..=180.0).text("Rotation (deg)"))
                    .changed()
                {
                    self.rotation = rotation_degrees.to_radians();
                }
                ui.separator();
                if ui
                    .add_enabled(self.history.current_index > 0, egui::Button::new("Undo"))
                    .clicked()
                {
                    undo = true;
                }
                if ui
                    .add_enabled(
                        self.history.current_index < self.history.textures.len() - 1,
                        egui::Button::new("Redo"),
                    )
                    .clicked()
                {
                    redo = true;
                }
                ui.separator();
                ui.label("New Canvas Size");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::DragValue::new(&mut self.canvas_width)
                            .clamp_range(1..=self.max_texture_dimension)
                            .speed(10.0)
                            .prefix("W ")
                            .suffix(" px"),
                    );
                    ui.add(
                        egui::DragValue::new(&mut self.canvas_height)
                            .clamp_range(1..=self.max_texture_dimension)
                            .speed(10.0)
                            .prefix("H ")
                            .suffix(" px"),
                    );
                });
                if ui.button("Create New Canvas").clicked() {
                    create_new_canvas = true;
                }
            });
        });

        if clear_canvas {
            self.clear_canvas();
        }
        if undo {
            if let Some(texture) = self.history.undo() {
                let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("Undo Encoder"),
                });
                encoder.copy_texture_to_texture(
                    texture.as_image_copy(),
                    self.canvas_texture.as_image_copy(),
                    texture.size(),
                );
                self.queue.submit(std::iter::once(encoder.finish()));
            }
        }
        if redo {
            if let Some(texture) = self.history.redo() {
                let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("Redo Encoder"),
                });
                encoder.copy_texture_to_texture(
                    texture.as_image_copy(),
                    self.canvas_texture.as_image_copy(),
                    texture.size(),
                );
                self.queue.submit(std::iter::once(encoder.finish()));
            }
        }
        if create_new_canvas {
            self.recreate_canvas_texture(self.canvas_width, self.canvas_height);
        }

        let tris = self.egui_ctx.tessellate(full_output.shapes, window.scale_factor() as f32);
        for (id, image_delta) in &full_output.textures_delta.set {
            self.egui_renderer
                .update_texture(&self.device, &self.queue, *id, image_delta);
        }
        let screen_descriptor = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [self.config.width, self.config.height],
            pixels_per_point: window.scale_factor() as f32,
        };
        self.egui_renderer.update_buffers(
            &self.device,
            &self.queue,
            &mut encoder,
            &tris,
            &screen_descriptor,
        );

        {
            let egui_desc = wgpu::RenderPassDescriptor {
                label: Some("Egui Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            };

            let mut egui_pass = encoder.begin_render_pass(&egui_desc);
            // The egui renderer requires a 'static render pass; transmute is safe
            // here because the pass does not outlive the encoder scope.
            let mut egui_pass: wgpu::RenderPass<'static> = unsafe { std::mem::transmute(egui_pass) };
            self.egui_renderer
                .render(&mut egui_pass, &tris, &screen_descriptor);
        }

        for id in &full_output.textures_delta.free {
            self.egui_renderer.free_texture(id);
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        output.present();

        Ok(())
    }
}

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let event_loop = EventLoop::new()?;
    let window = event_loop.create_window(WindowAttributes::default())?;
    window.set_title("Rusty Painter (GPU backend)");

    let mut state = pollster::block_on(State::new(&window));

    event_loop
        .run(move |event, elwt| match event {
            Event::WindowEvent { ref event, window_id } if window_id == window.id() => {
                if !state.input(event, &window) {
                    match event {
                        WindowEvent::CloseRequested => elwt.exit(),
                        WindowEvent::Resized(physical_size) => {
                            state.resize(*physical_size);
                        }
                        WindowEvent::RedrawRequested => {
                            state.update();
                            match state.render(&window) {
                                Ok(_) => {}
                                Err(wgpu::SurfaceError::Lost) => state.resize(state.size),
                                Err(wgpu::SurfaceError::OutOfMemory) => elwt.exit(),
                                Err(e) => eprintln!("{e:?}"),
                            }
                        }
                        _ => {}
                    }
                }
            }
            Event::AboutToWait => {
                window.request_redraw();
            }
            _ => {}
        })
        .unwrap();

    Ok(())
}
