//! GPU-Canvas-Renderpipeline mit wgpu und Low-Latency-Verwaltung.
//!
//! Features:
//! - Zero-Latency-Pufferung via `PresentMode::Mailbox` (Fallback: `Fifo`) und `desired_maximum_frame_latency: 1`.
//! - Strikte Pipeline-Trennung: DIN-A4-Hintergrund, permanente Striche und temporäre Live-Geometrie.
//! - NDC-Transformation der 2D-Vektor-Meshes über Vertex-Shader-Uniforms.
//! - Alpha-Blending (`wgpu::BlendState::ALPHA_BLENDING`).

use std::ops::Range;
use std::sync::Arc;
use winit::dpi::PhysicalSize;
use winit::window::Window;

use crate::background::{generate_background_mesh, generate_paper_quad, DESK_COLOR};
use crate::document::{Page, PageBackground, PageViewport, Stroke, StylusSample};
use crate::filter::OneEuroFilter;
use crate::spline::{self, ControlPoint, SplineConfig};

/// 2D-Vertex für Inking-Meshes mit Positions- und Farbattributen (Multi-Color).
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct InkVertex {
    pub position: [f32; 2],
    pub color: [f32; 4],
}

impl InkVertex {
    pub fn new(x: f32, y: f32, color: [f32; 4]) -> Self {
        Self {
            position: [x, y],
            color,
        }
    }

    pub fn desc() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<InkVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 0,
                    format: wgpu::VertexFormat::Float32x2,
                },
                wgpu::VertexAttribute {
                    offset: std::mem::size_of::<[f32; 2]>() as wgpu::BufferAddress,
                    shader_location: 1,
                    format: wgpu::VertexFormat::Float32x4,
                },
            ],
        }
    }
}

/// Uniform-Daten für die 2D-Kamera (Scale und Offset im NDC-Raum).
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CameraUniforms {
    pub transform_scale: [f32; 2],
    pub transform_offset: [f32; 2],
}

/// Transformiert einen Pixel-Koordinatenpunkt in den Normalized Device Coordinate (NDC) Raum [-1.0, 1.0].
#[inline]
pub fn pixel_to_ndc(x: f32, y: f32, width: f32, height: f32) -> [f32; 2] {
    let w = width.max(1.0);
    let h = height.max(1.0);
    let x_ndc = (x / w) * 2.0 - 1.0;
    let y_ndc = 1.0 - (y / h) * 2.0;
    [x_ndc, y_ndc]
}

/// Transformiert eine Folge von Spline-Vertices aus dem Pixelraum in NDC-`InkVertex`-Elemente.
pub fn spline_vertices_to_ndc(
    vertices: &[spline::Vertex],
    width: f32,
    height: f32,
    color: [f32; 4],
) -> Vec<InkVertex> {
    vertices
        .iter()
        .map(|v| {
            let ndc = pixel_to_ndc(v.position[0], v.position[1], width, height);
            InkVertex::new(ndc[0], ndc[1], color)
        })
        .collect()
}

/// Verwaltet den GPU-Zustand, Surface, Pipelines und Buffer für das Inking.
pub struct CanvasState {
    pub surface: wgpu::Surface<'static>,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub config: wgpu::SurfaceConfiguration,
    pub size: PhysicalSize<u32>,
    pub viewport: PageViewport,
    /// Von seitlicher UI belegte Breite links/rechts in physischen Pixeln
    ui_inset_left: f32,
    ui_inset_right: f32,
    pub camera: crate::viewport::ViewportState,

    // Pipelines
    pub strip_pipeline: wgpu::RenderPipeline,
    pub triangle_pipeline: wgpu::RenderPipeline,

    // 2D-Kamera Uniforms & Bind Group (gilt einheitlich für alle Vertizes)
    pub camera_buffer: wgpu::Buffer,
    pub camera_bind_group: wgpu::BindGroup,
    pub current_ink_color: [f32; 4],
    pub current_stroke_width_mm: f32,
    pub current_tool: crate::document::StrokeTool,

    // Statische Hintergrund-Geometrie
    pub paper_buffer: wgpu::Buffer,
    pub background_buffer: wgpu::Buffer,
    pub background_vertex_capacity: usize,
    pub background_vertex_count: u32,
    pub current_background: PageBackground,

    // Pipeline-Trennung: Live-Strich (aktiver Stroke)
    pub live_vertex_buffer: wgpu::Buffer,
    pub live_vertex_capacity: usize,
    pub live_vertex_count: u32,
    pub live_samples: Vec<StylusSample>,
    pub live_control_points: Vec<ControlPoint>,
    pub live_vertices: Vec<InkVertex>,
    pub filter: OneEuroFilter,
    pub spline_config: SplineConfig,

    // Pipeline-Trennung: Abgeschlossene Striche der aktuellen Seite
    pub committed_vertex_buffer: wgpu::Buffer,
    pub committed_vertex_capacity: usize,
    pub committed_vertex_count: u32,
    pub committed_strokes: Vec<Range<u32>>,

    // egui UI-Renderer
    pub egui_renderer: egui_wgpu::Renderer,
}

impl CanvasState {
    const INITIAL_LIVE_CAPACITY: usize = 4096;
    const INITIAL_COMMITTED_CAPACITY: usize = 65536;
    const INITIAL_BACKGROUND_CAPACITY: usize = 16384;
    pub const VIEWPORT_MARGIN: f32 = 24.0;

    /// Initialisiert den CanvasState für das übergebene Winit-Fenster.
    pub async fn new(window: Arc<Window>) -> Result<Self, Box<dyn std::error::Error>> {
        let size = window.inner_size();
        let viewport =
            PageViewport::calculate(size.width as f32, size.height as f32, Self::VIEWPORT_MARGIN);

        // 1. WGPU Instance & Surface
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });

        let surface = instance.create_surface(window.clone())?;

        // 2. Adapter auswählen (hohe Performance bevorzugt)
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .ok_or("Kein passender GPU-Adapter gefunden")?;

        // 3. Device & Queue erstellen
        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("Ink Device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                },
                None,
            )
            .await?;

        // 4. Zero-Latency-Pufferung: Mailbox mit Fallback Fifo & Frame-Latency 1
        let surface_caps = surface.get_capabilities(&adapter);
        let surface_format = surface_caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(surface_caps.formats[0]);

        let present_mode = if std::env::var("INK_FORCE_FIFO").is_ok() {
            eprintln!("Erzwinge PresentMode::Fifo (via INK_FORCE_FIFO)");
            wgpu::PresentMode::Fifo
        } else if surface_caps
            .present_modes
            .contains(&wgpu::PresentMode::Mailbox)
        {
            eprintln!("Verwende Zero-Latency PresentMode::Mailbox");
            wgpu::PresentMode::Mailbox
        } else {
            eprintln!("Mailbox nicht verfügbar, Fallback auf PresentMode::Fifo");
            wgpu::PresentMode::Fifo
        };

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            alpha_mode: surface_caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 1, // Minimiert Eingabelatenz auf 1 Frame
        };
        surface.configure(&device, &config);

        // 5. Shader & Bind Group Layout
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Ink WGSL Shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Stroke Bind Group Layout"),
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
        });

        let (scale, offset) = viewport.ndc_transform();

        let default_ink_color = [0.1, 0.1, 0.15, 1.0];
        let camera_uniforms = CameraUniforms {
            transform_scale: scale,
            transform_offset: offset,
        };
        let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Camera Uniform Buffer"),
            size: std::mem::size_of::<CameraUniforms>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&camera_buffer, 0, bytemuck::cast_slice(&[camera_uniforms]));

        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Camera Bind Group"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });

        // Pipeline Layout
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Ink Pipeline Layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        // TriangleStrip-Pipeline (für Inking-Striche)
        let strip_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Ink Strip Render Pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[InkVertex::desc()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });

        // TriangleList-Pipeline (für Hintergrundpapier und Muster)
        let triangle_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Background TriangleList Pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[InkVertex::desc()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });

        // 6. Statische Buffer
        let paper_quad = generate_paper_quad();
        let paper_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Paper Quad Buffer"),
            size: (paper_quad.len() * std::mem::size_of::<InkVertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&paper_buffer, 0, bytemuck::cast_slice(&paper_quad));

        let current_background = PageBackground::Grid5mm;
        let bg_mesh = generate_background_mesh(current_background);
        let background_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Background Mesh Buffer"),
            size: (Self::INITIAL_BACKGROUND_CAPACITY * std::mem::size_of::<InkVertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&background_buffer, 0, bytemuck::cast_slice(&bg_mesh));

        // 7. GPU-Puffer für Live- und Committed-Geometrie
        let live_vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Live Stroke Vertex Buffer"),
            size: (Self::INITIAL_LIVE_CAPACITY * std::mem::size_of::<InkVertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let committed_vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Committed Strokes Vertex Buffer"),
            size: (Self::INITIAL_COMMITTED_CAPACITY * std::mem::size_of::<InkVertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // 8. egui Renderer für das schwebende UI initialisieren
        let egui_renderer = egui_wgpu::Renderer::new(&device, config.format, None, 1);

        Ok(Self {
            surface,
            device,
            queue,
            config,
            size,
            viewport,
            ui_inset_left: 0.0,
            ui_inset_right: 0.0,
            strip_pipeline,
            triangle_pipeline,
            camera_buffer,
            camera_bind_group,
            current_ink_color: default_ink_color,
            current_stroke_width_mm: 0.5,
            current_tool: crate::document::StrokeTool::Pen,
            paper_buffer,
            background_buffer,
            background_vertex_capacity: Self::INITIAL_BACKGROUND_CAPACITY,
            background_vertex_count: bg_mesh.len() as u32,
            current_background,
            live_vertex_buffer,
            live_vertex_capacity: Self::INITIAL_LIVE_CAPACITY,
            live_vertex_count: 0,
            live_samples: Vec::with_capacity(256),
            live_control_points: Vec::with_capacity(256),
            live_vertices: Vec::with_capacity(1024),
            filter: OneEuroFilter::default(),
            spline_config: SplineConfig {
                min_radius: 1.25, // 0.125 mm bei 10 Einheiten/mm (Standard 0.5 mm Strichbreite)
                max_radius: 3.75, // 0.375 mm bei vollem Druck
                samples_per_segment: 16,
            },
            committed_vertex_buffer,
            committed_vertex_capacity: Self::INITIAL_COMMITTED_CAPACITY,
            committed_vertex_count: 0,
            committed_strokes: Vec::with_capacity(128),
            egui_renderer,
            camera: crate::viewport::ViewportState::new(),
        })
    }

    /// Aktualisiert die 2D-Kamera Uniforms (Scale & Offset im NDC-Raum) basierend auf Viewport und Kamera.
    pub fn update_uniforms(&mut self) {
        let (scale, offset) = self.camera.ndc_transform(&self.viewport);
        let camera_uniforms = CameraUniforms {
            transform_scale: scale,
            transform_offset: offset,
        };
        self.queue.write_buffer(
            &self.camera_buffer,
            0,
            bytemuck::cast_slice(&[camera_uniforms]),
        );
    }

    /// Setzt die von seitlicher UI belegten Streifen (physische Pixel), damit die Seite
    /// im freien Bereich zentriert wird. Gibt `true` zurück, wenn sich etwas geändert hat.
    pub fn set_ui_insets(&mut self, left: f32, right: f32) -> bool {
        if (self.ui_inset_left - left).abs() < 0.5 && (self.ui_inset_right - right).abs() < 0.5 {
            return false;
        }
        self.ui_inset_left = left;
        self.ui_inset_right = right;
        self.recalculate_viewport();
        true
    }

    fn recalculate_viewport(&mut self) {
        self.viewport = PageViewport::calculate_with_insets(
            self.size.width as f32,
            self.size.height as f32,
            Self::VIEWPORT_MARGIN,
            self.ui_inset_left,
            self.ui_inset_right,
        );
        self.update_uniforms();
    }

    /// Passt die Surface und Projektionsmatrizen an eine neue Fenstergröße an.
    pub fn resize(&mut self, new_size: PhysicalSize<u32>) {
        if new_size.width > 0 && new_size.height > 0 {
            self.size = new_size;
            self.config.width = new_size.width;
            self.config.height = new_size.height;
            self.surface.configure(&self.device, &self.config);
            self.recalculate_viewport();
        }
    }

    /// Verschiebt den Bildausschnitt um Bildschirm-Pixel delta.
    pub fn pan(&mut self, delta_x: f32, delta_y: f32) {
        self.camera
            .pan_by_screen_delta(delta_x, delta_y, &self.viewport);
        self.update_uniforms();
    }

    /// Zoomt zentriert auf eine Cursor-Bildschirmposition.
    pub fn zoom_at(&mut self, cursor_x: f32, cursor_y: f32, factor: f32) {
        self.camera
            .zoom_at_cursor([cursor_x, cursor_y], factor, &self.viewport);
        self.update_uniforms();
    }

    /// Setzt Zoom und Pan auf 100% zentriert zurück.
    pub fn reset_view(&mut self) {
        self.camera.reset();
        self.update_uniforms();
    }

    /// Transformiert Fenster-Pixel in Seitenkoordinaten.
    pub fn screen_to_page(&self, screen_x: f32, screen_y: f32) -> Option<glam::Vec2> {
        self.camera
            .screen_to_page(screen_x, screen_y, &self.viewport)
    }

    /// Transformiert Fenster-Pixel in Seitenkoordinaten und klemmt auf die Seite.
    pub fn screen_to_page_clamped(&self, screen_x: f32, screen_y: f32) -> glam::Vec2 {
        self.camera
            .screen_to_page_clamped(screen_x, screen_y, &self.viewport)
    }

    /// Transformiert einen Punkt aus DIN-A4-Seitenkoordinaten in Fenster-Bildschirmkoordinaten.
    pub fn page_to_screen(&self, page_pt: glam::Vec2) -> [f32; 2] {
        self.camera.page_to_screen_vec2(page_pt, &self.viewport)
    }

    /// Ersetzt die Live-Vertices des aktiven Strichs durch ein bereinigtes Form-Mesh (für Draw-and-Hold).
    pub fn set_live_shape_mesh(&mut self, samples: Vec<StylusSample>, vertices: Vec<InkVertex>) {
        self.live_samples = samples;
        self.live_vertices = vertices;
        self.live_vertex_count = self.live_vertices.len() as u32;

        if self.live_vertices.len() > self.live_vertex_capacity {
            self.live_vertex_capacity =
                (self.live_vertices.len() * 2).max(Self::INITIAL_LIVE_CAPACITY);
            self.live_vertex_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Live Stroke Vertex Buffer (Reallocated)"),
                size: (self.live_vertex_capacity * std::mem::size_of::<InkVertex>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }

        if !self.live_vertices.is_empty() {
            self.queue.write_buffer(
                &self.live_vertex_buffer,
                0,
                bytemuck::cast_slice(&self.live_vertices),
            );
        }
    }

    /// Setzt die Strichfarbe (RGB + Alpha) für neue Striche.
    pub fn set_stroke_color(&mut self, color: [f32; 4]) {
        self.current_ink_color = color;
    }

    /// Konfiguriert die Strichbreite in Millimetern.
    pub fn set_stroke_width_mm(&mut self, width_mm: f32) {
        self.current_stroke_width_mm = width_mm;
        self.spline_config =
            SplineConfig::from_width_units(width_mm * crate::document::UNITS_PER_MM);
    }

    /// Setzt das aktive Zeichenwerkzeug für den Stift.
    pub fn set_tool(&mut self, tool: crate::document::StrokeTool) {
        self.current_tool = tool;
    }

    /// Setzt den Seitenhintergrund und aktualisiert bei Bedarf den Vertex-Puffer.
    pub fn set_background(&mut self, background: PageBackground) {
        if self.current_background == background {
            return;
        }
        self.current_background = background;
        let mesh = generate_background_mesh(background);
        self.background_vertex_count = mesh.len() as u32;

        if mesh.len() > self.background_vertex_capacity {
            self.background_vertex_capacity = mesh.len() * 2;
            self.background_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Background Mesh Buffer (Reallocated)"),
                size: (self.background_vertex_capacity * std::mem::size_of::<InkVertex>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }

        if !mesh.is_empty() {
            self.queue
                .write_buffer(&self.background_buffer, 0, bytemuck::cast_slice(&mesh));
        }
    }

    /// Synchronisiert die GPU-Puffer mit den Daten der aktiven Seite.
    pub fn sync_page(&mut self, page: &mut Page) {
        page.rebuild_cache_if_dirty();
        self.set_background(page.background);

        self.committed_vertex_count = page.vertex_cache.len() as u32;
        self.committed_strokes = page.stroke_ranges.clone();

        if page.vertex_cache.is_empty() {
            return;
        }

        if page.vertex_cache.len() > self.committed_vertex_capacity {
            self.committed_vertex_capacity =
                (page.vertex_cache.len() * 2).max(Self::INITIAL_COMMITTED_CAPACITY);
            self.committed_vertex_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Committed Strokes Vertex Buffer (Reallocated)"),
                size: (self.committed_vertex_capacity * std::mem::size_of::<InkVertex>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }

        self.queue.write_buffer(
            &self.committed_vertex_buffer,
            0,
            bytemuck::cast_slice(&page.vertex_cache),
        );
    }

    /// Startet einen neuen Live-Strich in DIN-A4-Seitenkoordinaten.
    pub fn start_stroke(&mut self, page_x: f32, page_y: f32, pressure: f32, timestamp: f64) {
        self.filter.reset();
        self.live_samples.clear();
        self.live_control_points.clear();
        self.live_vertices.clear();
        self.live_vertex_count = 0;

        // 1. Zwingend durch den 1€-Filter leiten
        let (fx, fy, fp) = self.filter.filter(page_x, page_y, pressure, timestamp);
        let pt = ControlPoint::new(fx, fy, fp);
        self.live_control_points.push(pt);

        let sample = StylusSample {
            position: glam::Vec2::new(fx, fy),
            pressure: fp,
            timestamp,
        };
        self.live_samples.push(sample);

        self.update_live_mesh();
    }

    /// Fügt dem aktuellen Live-Strich einen neuen Bewegungspunkt hinzu.
    pub fn extend_stroke(&mut self, page_x: f32, page_y: f32, pressure: f32, timestamp: f64) {
        if self.live_control_points.is_empty() {
            self.start_stroke(page_x, page_y, pressure, timestamp);
            return;
        }

        // 1. Zwingend durch den 1€-Filter leiten
        let (fx, fy, fp) = self.filter.filter(page_x, page_y, pressure, timestamp);
        let new_pt = ControlPoint::new(fx, fy, fp);

        // 2. Mindestabstand zu vorherigem Kontrollpunkt prüfen (0.3 Einheiten = 0.03 mm für hohe Pfadpräzision)
        if let Some(last) = self.live_control_points.last() {
            let dist_sq = (new_pt.position - last.position).length_squared();
            if dist_sq < 0.09 {
                return;
            }
        }

        // 3. Gefilterten Punkt synchron in Samples und Kontrollpunkten ablegen
        let sample = StylusSample {
            position: glam::Vec2::new(fx, fy),
            pressure: fp,
            timestamp,
        };
        self.live_samples.push(sample);
        self.live_control_points.push(new_pt);
        self.update_live_mesh();
    }

    /// Bricht den aktuellen Live-Strich ab und setzt Puffer sowie 1€-Filter sauber zurück.
    pub fn cancel_stroke(&mut self) {
        self.live_samples.clear();
        self.live_control_points.clear();
        self.live_vertices.clear();
        self.live_vertex_count = 0;
        self.filter.reset();
    }

    /// Schließt den aktuellen Live-Strich ab und erzeugt das fertige `Stroke`-Objekt nach dem Zwei-Stufen-Modell.
    pub fn finish_stroke(&mut self, stroke_id: u64) -> Option<Stroke> {
        if self.live_samples.is_empty() {
            self.live_samples.clear();
            self.live_control_points.clear();
            self.live_vertices.clear();
            self.live_vertex_count = 0;
            self.filter.reset();
            return None;
        }

        // Post-Stroke Finalisierung (Corner Preservation, Druckglättung, RDP, runde Kappen)
        let stroke = spline::finalize_stroke_with_config(
            stroke_id,
            &self.live_samples,
            self.current_ink_color,
            &self.spline_config,
        );
        let mut stroke = stroke;
        stroke.tool = self.current_tool;
        stroke.width = self.current_stroke_width_mm * crate::document::UNITS_PER_MM;
        stroke.finalized = true;

        // Temporären GPU-Live-Buffer vollständig leeren
        self.live_samples.clear();
        self.live_control_points.clear();
        self.live_vertices.clear();
        self.live_vertex_count = 0;
        self.filter.reset();

        Some(stroke)
    }

    /// Baut das Mesh des Live-Strichs in Seitenkoordinaten neu auf.
    fn update_live_mesh(&mut self) {
        if self.live_control_points.is_empty() {
            self.live_vertex_count = 0;
            self.live_vertices.clear();
            return;
        }

        let color = self.current_ink_color;
        let raw_strip =
            spline::generate_triangle_strip(&self.live_control_points, &self.spline_config);
        self.live_vertices = raw_strip
            .into_iter()
            .map(|v| InkVertex::new(v.position[0], v.position[1], color))
            .collect();
        self.live_vertex_count = self.live_vertices.len() as u32;

        if self.live_vertex_count == 0 {
            return;
        }

        if self.live_vertices.len() > self.live_vertex_capacity {
            self.live_vertex_capacity =
                (self.live_vertices.len() * 2).max(Self::INITIAL_LIVE_CAPACITY);
            self.live_vertex_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Live Stroke Vertex Buffer (Reallocated)"),
                size: (self.live_vertex_capacity * std::mem::size_of::<InkVertex>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }

        self.queue.write_buffer(
            &self.live_vertex_buffer,
            0,
            bytemuck::cast_slice(&self.live_vertices),
        );
    }

    /// Rendert den gesamten Viewport:
    /// - Pass 1: Schreibtisch, DIN-A4-Papier, Hintergrundraster, permanente Striche und Live-Strich.
    /// - Pass 2: egui UI (Toolbar, Menüs, Toast) über `LoadOp::Load` auf dieselbe Surface-Textur.
    pub fn render(
        &mut self,
        clipped_primitives: &[egui::ClippedPrimitive],
        screen_descriptor: &egui_wgpu::ScreenDescriptor,
        textures_delta: &egui::TexturesDelta,
    ) -> Result<(), wgpu::SurfaceError> {
        // Vor dem Renderpass: egui-Texturen aktualisieren
        for (id, image_delta) in &textures_delta.set {
            self.egui_renderer
                .update_texture(&self.device, &self.queue, *id, image_delta);
        }

        let output = self.surface.get_current_texture()?;
        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Ink & UI Render Encoder"),
            });

        // egui Vertex- und Index-Puffer aktualisieren
        self.egui_renderer.update_buffers(
            &self.device,
            &self.queue,
            &mut encoder,
            clipped_primitives,
            screen_descriptor,
        );

        // PASS 1: Canvas (Hintergrund, Papierquadrat, Gittermuster, Striche)
        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Ink Canvas Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(DESK_COLOR), // Dunkler Tischhintergrund
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            // 2D-Kamera Uniforms (Scale & Offset) binden
            render_pass.set_bind_group(0, &self.camera_bind_group, &[]);

            // 1. Weißes DIN-A4-Blatt zeichnen (Farbe ist direkt im Paper-Quad Vertex hinterlegt)
            render_pass.set_pipeline(&self.triangle_pipeline);
            render_pass.set_vertex_buffer(0, self.paper_buffer.slice(..));
            render_pass.draw(0..6, 0..1);

            // 2. Hintergrundmuster (5mm-Gitter, liniert oder Punktraster mit Vertex-Farbe)
            if self.background_vertex_count > 0 {
                render_pass.set_vertex_buffer(0, self.background_buffer.slice(..));
                render_pass.draw(0..self.background_vertex_count, 0..1);
            }

            // 3. Abgeschlossene Striche der aktiven Seite rendern (TriangleStrip mit Vertex-Farben)
            render_pass.set_pipeline(&self.strip_pipeline);

            if self.committed_vertex_count > 0 {
                render_pass.set_vertex_buffer(0, self.committed_vertex_buffer.slice(..));
                for range in &self.committed_strokes {
                    if range.end > range.start {
                        render_pass.draw(range.clone(), 0..1);
                    }
                }
            }

            // 4. Temporäre Live-Geometrie des aktuellen Strichs rendern (Zero Latency)
            if self.live_vertex_count > 0 {
                render_pass.set_vertex_buffer(0, self.live_vertex_buffer.slice(..));
                render_pass.draw(0..self.live_vertex_count, 0..1);
            }
        }

        // PASS 2: egui UI (LoadOp::Load auf dieselbe Surface-Textur!)
        if !clipped_primitives.is_empty() {
            let mut egui_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("egui UI Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            self.egui_renderer
                .render(&mut egui_pass, clipped_primitives, screen_descriptor);
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        output.present();

        // egui Texturen freigeben
        for id in &textures_delta.free {
            self.egui_renderer.free_texture(id);
        }

        Ok(())
    }

    /// Rendert nur die Canvas-Fläche ohne UI (z. B. für Headless-Tests).
    pub fn render_canvas_only(&mut self) -> Result<(), wgpu::SurfaceError> {
        let dummy_delta = egui::TexturesDelta::default();
        let dummy_screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [self.size.width, self.size.height],
            pixels_per_point: 1.0,
        };
        self.render(&[], &dummy_screen, &dummy_delta)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pixel_to_ndc_mapping() {
        let width = 800.0;
        let height = 600.0;

        let top_left = pixel_to_ndc(0.0, 0.0, width, height);
        assert!((top_left[0] - (-1.0)).abs() < 1e-5);
        assert!((top_left[1] - 1.0).abs() < 1e-5);

        let center = pixel_to_ndc(400.0, 300.0, width, height);
        assert!(center[0].abs() < 1e-5);
        assert!(center[1].abs() < 1e-5);
    }

    #[test]
    fn test_ink_vertex_layout() {
        assert_eq!(std::mem::size_of::<InkVertex>(), 24);
        let v = InkVertex::new(0.5, -0.5, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(v.position, [0.5, -0.5]);
        assert_eq!(v.color, [1.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn test_camera_uniforms() {
        let _u = CameraUniforms::default();
        assert_eq!(std::mem::size_of::<CameraUniforms>(), 16);
    }

    #[test]
    fn test_viewport_aspect_ratio_ndc_transform() {
        let vp = PageViewport::calculate(1920.0, 1080.0, 20.0);
        let (scale, offset) = vp.ndc_transform();

        // Zentrum der DIN-A4-Seite muss im NDC-Zentrum (0, 0) landen
        let center_x = 1050.0; // PAGE_WIDTH / 2
        let center_y = 1485.0; // PAGE_HEIGHT / 2

        let ndc_x = center_x * scale[0] + offset[0];
        let ndc_y = center_y * scale[1] + offset[1];

        assert!(ndc_x.abs() < 1e-4, "NDC X ({ndc_x}) muss nahe 0.0 sein");
        assert!(ndc_y.abs() < 1e-4, "NDC Y ({ndc_y}) muss nahe 0.0 sein");
    }
}
