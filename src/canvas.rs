//! GPU-Canvas-Renderpipeline mit wgpu und Low-Latency-Verwaltung.
//!
//! Features:
//! - Geringe Bildverzögerung durch `PresentMode::Mailbox` (Fallback: `Fifo`) und `desired_maximum_frame_latency: 1`.
//! - Strikte Pipeline-Trennung: DIN-A4-Hintergrund, permanente Striche und temporäre Live-Geometrie.
//! - NDC-Transformation der 2D-Vektor-Meshes über Vertex-Shader-Uniforms.
//! - Alpha-Blending (`wgpu::BlendState::ALPHA_BLENDING`).

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;
use winit::dpi::PhysicalSize;
use winit::window::Window;

use crate::background::{
    generate_paper_quad, generate_pattern_quad, PaperTheme, PatternColor, PatternVertex,
};
use crate::document::{Page, PageBackground, PageViewport, Stroke, StylusSample};
use crate::filter::OneEuroFilter;
use crate::spline::{self, ControlPoint, SplineConfig};
use crate::tn;
use crate::viewport::PageStack;

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
    /// x: 1.0 = dunkle Tinte aufhellen (Shader `fs_ink`), y–w: ungenutzt (Ausrichtung)
    pub ink_params: [f32; 4],
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
/// Meldung, wenn kein Grafikadapter das Fenster darstellen kann (deutsch, anzeigen mit
/// `i18n::tr`).
pub const NO_USABLE_GPU_MESSAGE: &str = tn!("Keine nutzbare Grafikschnittstelle gefunden.\n\
     Häufigste Ursache: Der Benutzer darf nicht auf die Grafikkarte zugreifen (/dev/dri).\n\
     Prüfen mit `ls -l /dev/dri` und `groups`; ggf. `sudo usermod -aG render,video $USER`\n\
     und neu anmelden. Alternativ einen Software-Renderer installieren (Paket mesa-vulkan-drivers)\n\
     oder die Schnittstelle festlegen, z. B. `WGPU_BACKEND=vulkan ink-notes`.");

/// Wählt den Darstellungsmodus: Mailbox (geringste Latenz), sonst Fifo, sonst den ersten
/// angebotenen. `None`, wenn keiner angeboten wird. `INK_FORCE_FIFO` erzwingt Fifo.
pub fn choose_present_mode(available: &[wgpu::PresentMode]) -> Option<wgpu::PresentMode> {
    let force_fifo = std::env::var("INK_FORCE_FIFO").is_ok();
    let preferred: &[wgpu::PresentMode] = if force_fifo {
        &[wgpu::PresentMode::Fifo]
    } else {
        &[wgpu::PresentMode::Mailbox, wgpu::PresentMode::Fifo]
    };
    preferred
        .iter()
        .copied()
        .find(|m| available.contains(m))
        .or_else(|| available.first().copied())
}

/// Erkennt, ob die GPU-Daten einer Nachbarseite noch zur Seite passen. Änderungen an der
/// aktuellen Seite verwerfen deren Eintrag ohnehin (siehe [`CanvasState::sync_page`]).
#[derive(Debug, Clone, PartialEq)]
struct NeighborFingerprint {
    page_id: u64,
    background: PageBackground,
    size: [u32; 2],
    paper_theme: PaperTheme,
    pattern: PatternColor,
    vertex_count: usize,
    stroke_count: usize,
    last_stroke_id: Option<u64>,
    /// Neuaufbau des Caches (z. B. verschobener Inhalt bei gleicher Strichzahl)
    cache_generation: u64,
}

impl NeighborFingerprint {
    fn of(page: &Page, paper_theme: PaperTheme, pattern: PatternColor) -> Self {
        Self {
            page_id: page.id,
            background: page.background,
            size: page.size.to_array().map(f32::to_bits),
            paper_theme,
            pattern,
            vertex_count: page.vertex_cache.len(),
            stroke_count: page.strokes.len(),
            last_stroke_id: page.strokes.last().map(|s| s.id),
            cache_generation: page.cache_generation,
        }
    }
}

/// GPU-Daten einer in der durchgehenden Ansicht sichtbaren Nachbarseite.
struct NeighborPage {
    fingerprint: NeighborFingerprint,
    /// Ecke oben links in Koordinaten der aktuellen Seite
    offset: glam::Vec2,
    camera_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    /// Papier (Dreiecke) und Muster (Rechteck für `fs_pattern`)
    paper_buffer: wgpu::Buffer,
    pattern_buffer: Option<wgpu::Buffer>,
    vertex_buffer: Option<wgpu::Buffer>,
    stroke_ranges: Vec<Range<u32>>,
}

pub struct CanvasState {
    /// Drawing surface of the window; `None` while the app is in the background on Android
    /// (the native window only exists between `Resumed` and `Suspended`)
    surface: Option<wgpu::Surface<'static>>,
    instance: wgpu::Instance,
    /// Verwendeter Grafikadapter (für erneute Abfrage der Surface-Fähigkeiten bei Größenänderung)
    adapter: wgpu::Adapter,
    /// Multisampling der Zeichenfläche (1 = aus) und das zugehörige Zwischenbild
    msaa_samples: u32,
    msaa_view: Option<wgpu::TextureView>,
    /// Stencil-Puffer: Jeder Strich deckt ein Pixel nur einmal, damit sich halbtransparente
    /// Tinte (Textmarker) dort, wo der Strich sich selbst überlappt, nicht verdunkelt
    stencil_view: wgpu::TextureView,
    stencil_reset_pipeline: wgpu::RenderPipeline,
    /// Meldung „keine Darstellungsmodi“ wurde bereits ausgegeben
    presentation_warned: bool,
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

    // Statische Hintergrund-Geometrie
    pub paper_buffer: wgpu::Buffer,
    /// Musterrechteck der aktuellen Seite (6 Ecken, leer bei blankem Papier)
    pub background_buffer: wgpu::Buffer,
    pub background_vertex_count: u32,
    pattern_pipeline: wgpu::RenderPipeline,
    pub current_background: PageBackground,
    /// Papierfarbe der Anzeige
    pub paper_theme: PaperTheme,
    /// Dunkle Tinte auf dunklem Papier aufhellen
    pub adapt_ink: bool,
    /// Farbe von Karos, Linien und Punkten
    pattern: PatternColor,
    /// Größe der aktuellen Seite und Rahmen, in den alle Seiten des Notizbuchs passen
    page_size: glam::Vec2,
    fit_box: glam::Vec2,
    /// Textfelder der sichtbaren Seiten nach Texturschlüssel (für die Rasterung)
    text_specs: HashMap<String, TextSpec>,
    /// Gerade bearbeitetes Textfeld der aktuellen Seite: nicht zeichnen (die Bearbeitung
    /// zeigt es)
    hidden_text: Option<u64>,
    /// Auflösung der Texttexturen in Pixeln je Seiteneinheit
    text_resolution: f32,

    // Pipeline-Trennung: Live-Strich (aktiver Stroke)
    pub live_vertex_buffer: wgpu::Buffer,
    pub live_vertex_capacity: usize,
    pub live_vertex_count: u32,
    pub live_samples: Vec<StylusSample>,
    /// Ungefilterte Eingabepunkte des Live-Strichs (nach dem Lineal): Grundlage des fertigen
    /// Strichs, damit kleine Schleifen nicht durch die Filterverzögerung schrumpfen
    live_raw_samples: Vec<StylusSample>,
    pub live_control_points: Vec<ControlPoint>,
    pub live_vertices: Vec<InkVertex>,
    pub filter: OneEuroFilter,
    pub spline_config: SplineConfig,

    // Pipeline-Trennung: Abgeschlossene Striche der aktuellen Seite
    pub committed_vertex_buffer: wgpu::Buffer,
    pub committed_vertex_capacity: usize,
    pub committed_vertex_count: u32,
    pub committed_strokes: Vec<Range<u32>>,

    // Eingefügte Bilder der aktuellen Seite
    image_layer: crate::image_layer::ImageLayer,

    /// Durchgehende Ansicht: sichtbare Nachbarseiten der aktuellen Seite
    neighbors: Vec<NeighborPage>,
    camera_layout: wgpu::BindGroupLayout,

    /// Eingeblendetes Lineal (Striche nahe einer Kante rasten ein)
    pub ruler: Option<crate::ruler::Ruler>,
    /// Kante, an der der aktuelle Live-Strich eingerastet ist
    ruler_lock: Option<crate::ruler::EdgeLock>,

    // egui UI-Renderer
    pub egui_renderer: egui_wgpu::Renderer,
}

impl CanvasState {
    const INITIAL_LIVE_CAPACITY: usize = 4096;
    const INITIAL_COMMITTED_CAPACITY: usize = 65536;
    pub const VIEWPORT_MARGIN: f32 = 24.0;

    /// Initialisiert den CanvasState für das übergebene Winit-Fenster.
    pub async fn new(window: Arc<Window>) -> Result<Self, Box<dyn std::error::Error>> {
        let size = window.inner_size();
        let viewport =
            PageViewport::calculate(size.width as f32, size.height as f32, Self::VIEWPORT_MARGIN);

        // 1. WGPU Instance & Surface (Grafikschnittstelle per WGPU_BACKEND wählbar, z. B. "vulkan")
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::util::backend_bits_from_env().unwrap_or(wgpu::Backends::all()),
            ..Default::default()
        });

        let surface = instance.create_surface(window.clone())?;

        // 2. Adapter auswählen: zuerst die GPU, dann der Software-Renderer (z. B. lavapipe).
        //    Ein Adapter taugt nur, wenn er für dieses Fenster Formate und Darstellungsmodi
        //    anbietet – fehlt z. B. der Zugriff auf /dev/dri, ist die Liste leer.
        let mut chosen = None;
        for force_fallback_adapter in [false, true] {
            let Some(adapter) = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    compatible_surface: Some(&surface),
                    force_fallback_adapter,
                })
                .await
            else {
                continue;
            };
            let info = adapter.get_info();
            let caps = surface.get_capabilities(&adapter);
            match choose_present_mode(&caps.present_modes) {
                Some(mode) if !caps.formats.is_empty() && !caps.alpha_modes.is_empty() => {
                    eprintln!(
                        "[Grafik] Adapter: {} ({:?}), Darstellungsmodus {:?}",
                        info.name, info.backend, mode
                    );
                    chosen = Some((adapter, caps, mode));
                    break;
                }
                _ => eprintln!(
                    "[Grafik] Adapter {} ({:?}) bietet für das Fenster keine Darstellung an, versuche den nächsten.",
                    info.name, info.backend
                ),
            }
        }
        let (adapter, surface_caps, present_mode) =
            chosen.ok_or(crate::i18n::tr(NO_USABLE_GPU_MESSAGE))?;

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

        // 4. Geringe Bildverzögerung (Mailbox, sonst Fifo) & Frame-Latency 1.
        //    Gezeichnet wird ohne sRGB-Umrechnung (siehe `render_format`); bietet die Fläche
        //    nur sRGB-Formate an, wird eine Ansicht ohne sRGB verwendet.
        let surface_format = surface_caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .unwrap_or(surface_caps.formats[0]);
        let view_formats = if surface_format.is_srgb() {
            vec![surface_format.remove_srgb_suffix()]
        } else {
            vec![]
        };

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            alpha_mode: surface_caps.alpha_modes[0],
            view_formats,
            desired_maximum_frame_latency: 1, // Minimiert Eingabelatenz auf 1 Frame
        };
        surface.configure(&device, &config);

        // Kantenglättung der Zeichenfläche (4× Multisampling), falls das Format es erlaubt
        let msaa_samples = if adapter
            .get_texture_format_features(render_format(&config))
            .flags
            .sample_count_supported(MSAA_SAMPLES)
        {
            MSAA_SAMPLES
        } else {
            1
        };
        let msaa_view = create_msaa_view(&device, &config, msaa_samples);
        let stencil_view = create_stencil_view(&device, &config, msaa_samples);

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
            ink_params: [0.0; 4],
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
                entry_point: "fs_ink",
                targets: &[Some(wgpu::ColorTargetState {
                    format: render_format(&config),
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
            depth_stencil: Some(stroke_stencil()),
            multisample: wgpu::MultisampleState {
                count: msaa_samples,
                ..Default::default()
            },
            multiview: None,
        });

        // Setzt den Stencil-Puffer zurück, wenn die Strichnummern überlaufen
        let stencil_reset_pipeline =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("Stencil Reset Pipeline"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: "vs_fullscreen",
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: "fs_none",
                    targets: &[Some(wgpu::ColorTargetState {
                        format: render_format(&config),
                        blend: None,
                        write_mask: wgpu::ColorWrites::empty(),
                    })],
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: Some(stencil_reset()),
                multisample: wgpu::MultisampleState {
                    count: msaa_samples,
                    ..Default::default()
                },
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
                    format: render_format(&config),
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
            depth_stencil: Some(passive_stencil()),
            multisample: wgpu::MultisampleState {
                count: msaa_samples,
                ..Default::default()
            },
            multiview: None,
        });

        // 6. Statische Buffer
        let paper_theme = PaperTheme::default();
        let paper_quad = generate_paper_quad(paper_theme.paper_color(), crate::document::A4_SIZE);
        let paper_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Paper Quad Buffer"),
            size: (paper_quad.len() * std::mem::size_of::<InkVertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&paper_buffer, 0, bytemuck::cast_slice(&paper_quad));

        let current_background = PageBackground::Grid5mm;
        let bg_mesh = generate_pattern_quad(
            current_background,
            paper_theme.pattern_color(),
            crate::document::A4_SIZE,
        );
        let background_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Pattern Quad Buffer"),
            size: (6 * std::mem::size_of::<PatternVertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&background_buffer, 0, bytemuck::cast_slice(&bg_mesh));

        // Karo, Linien und Punkte: pro Pixel im Shader berechnet
        let pattern_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Pattern Pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_pattern",
                buffers: &[PatternVertex::desc()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_pattern",
                targets: &[Some(wgpu::ColorTargetState {
                    format: render_format(&config),
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(passive_stencil()),
            multisample: wgpu::MultisampleState {
                count: msaa_samples,
                ..Default::default()
            },
            multiview: None,
        });

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

        let image_layer = crate::image_layer::ImageLayer::new(
            &device,
            &shader,
            &bind_group_layout,
            render_format(&config),
            msaa_samples,
        );

        // 8. egui Renderer für das schwebende UI initialisieren
        let egui_renderer = egui_wgpu::Renderer::new(&device, render_format(&config), None, 1);

        Ok(Self {
            surface: Some(surface),
            instance,
            adapter,
            msaa_samples,
            msaa_view,
            stencil_view,
            stencil_reset_pipeline,
            presentation_warned: false,
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
            paper_buffer,
            background_buffer,
            background_vertex_count: bg_mesh.len() as u32,
            pattern_pipeline,
            current_background,
            paper_theme,
            adapt_ink: false,
            pattern: PatternColor::default(),
            page_size: crate::document::A4_SIZE,
            fit_box: crate::document::A4_SIZE,
            text_specs: HashMap::new(),
            hidden_text: None,
            text_resolution: 1.0,
            live_vertex_buffer,
            live_vertex_capacity: Self::INITIAL_LIVE_CAPACITY,
            live_vertex_count: 0,
            live_samples: Vec::with_capacity(256),
            live_raw_samples: Vec::with_capacity(256),
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
            image_layer,
            neighbors: Vec::new(),
            camera_layout: bind_group_layout,
            ruler: None,
            ruler_lock: None,
            egui_renderer,
            camera: crate::viewport::ViewportState::new(),
        })
    }

    /// Aktualisiert die 2D-Kamera Uniforms (Scale & Offset im NDC-Raum) basierend auf Viewport und Kamera.
    pub fn update_uniforms(&mut self) {
        let camera_uniforms = self.camera_uniforms(glam::Vec2::ZERO);
        self.queue.write_buffer(
            &self.camera_buffer,
            0,
            bytemuck::cast_slice(&[camera_uniforms]),
        );
        for neighbor in &self.neighbors {
            self.queue.write_buffer(
                &neighbor.camera_buffer,
                0,
                bytemuck::cast_slice(&[self.camera_uniforms(neighbor.offset)]),
            );
        }
    }

    /// Kamera für eine Seite mit der Ecke `origin` (Koordinaten der aktuellen Seite).
    fn camera_uniforms(&self, origin: glam::Vec2) -> CameraUniforms {
        let (scale, mut offset) = self.camera.ndc_transform(&self.viewport);
        offset[0] += origin.x * scale[0];
        offset[1] += origin.y * scale[1];
        CameraUniforms {
            transform_scale: scale,
            transform_offset: offset,
            ink_params: [
                if self.adapt_ink && self.paper_theme.is_dark() {
                    1.0
                } else {
                    0.0
                },
                0.0,
                0.0,
                0.0,
            ],
        }
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

    /// Rahmen, in den alle Seiten des Notizbuchs passen (bestimmt den Maßstab bei 100 %).
    pub fn set_fit_box(&mut self, fit_box: glam::Vec2) {
        if self.fit_box != fit_box {
            self.fit_box = fit_box;
            self.recalculate_viewport();
        }
    }

    fn recalculate_viewport(&mut self) {
        let insets = (self.ui_inset_left, self.ui_inset_right);
        let (width, height) = (self.size.width as f32, self.size.height as f32);
        self.viewport = if self.page_size == crate::document::ENDLESS_PAGE_SIZE {
            PageViewport::calculate_endless(width, height, Self::VIEWPORT_MARGIN, insets)
        } else {
            PageViewport::calculate_for(
                width,
                height,
                Self::VIEWPORT_MARGIN,
                insets,
                self.fit_box,
                self.page_size,
            )
        };
        self.update_uniforms();
    }

    /// Releases the drawing surface: on Android the native window goes away when the app
    /// is sent to the background (`Event::Suspended`). Drawing is skipped until [`Self::resume`].
    pub fn suspend(&mut self) {
        self.surface = None;
    }

    /// Creates the drawing surface again for the window (`Event::Resumed` on Android).
    pub fn resume(&mut self, window: Arc<Window>) -> Result<(), Box<dyn std::error::Error>> {
        if self.surface.is_some() {
            return Ok(());
        }
        let surface = self.instance.create_surface(window.clone())?;
        if !surface
            .get_capabilities(&self.adapter)
            .formats
            .contains(&self.config.format)
        {
            return Err("surface format changed".into());
        }
        self.surface = Some(surface);
        // Configures the new surface (the window may have changed size meanwhile)
        self.resize(window.inner_size());
        Ok(())
    }

    /// Passt die Surface und Projektionsmatrizen an eine neue Fenstergröße an.
    pub fn resize(&mut self, new_size: PhysicalSize<u32>) {
        if new_size.width > 0 && new_size.height > 0 {
            self.size = new_size;
            self.config.width = new_size.width;
            self.config.height = new_size.height;
            let Some(surface) = &self.surface else {
                return;
            };
            // Fähigkeiten neu abfragen: Ein nicht (mehr) angebotener Modus führt in wgpu zum Absturz
            let caps = surface.get_capabilities(&self.adapter);
            if !caps.present_modes.contains(&self.config.present_mode) {
                match choose_present_mode(&caps.present_modes) {
                    Some(mode) => self.config.present_mode = mode,
                    None => {
                        if !self.presentation_warned {
                            eprintln!(
                                "[Grafik] Fenster kann derzeit nicht dargestellt werden (keine Darstellungsmodi). {}",
                                crate::i18n::tr(NO_USABLE_GPU_MESSAGE)
                            );
                            self.presentation_warned = true;
                        }
                        return;
                    }
                }
            }
            surface.configure(&self.device, &self.config);
            self.msaa_view = create_msaa_view(&self.device, &self.config, self.msaa_samples);
            self.stencil_view = create_stencil_view(&self.device, &self.config, self.msaa_samples);
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

    /// Durchgehende Ansicht: Seite unter dem Bildschirmpunkt, `None` neben den Seiten und
    /// zwischen zwei Seiten.
    pub fn page_at_screen(&self, stack: &PageStack, screen_x: f32, screen_y: f32) -> Option<usize> {
        let p = self
            .camera
            .screen_to_page_unclamped(screen_x, screen_y, &self.viewport);
        let index = stack.page_at(p.y)?;
        let x = p.x - stack.origins[index].x;
        (0.0..=stack.sizes[index].x).contains(&x).then_some(index)
    }

    /// Durchgehende Ansicht: Seite auf Höhe der Fenstermitte.
    pub fn page_at_view_center(&self, stack: &PageStack) -> Option<usize> {
        let p = self.camera.screen_to_page_unclamped(
            0.0,
            self.size.height as f32 / 2.0,
            &self.viewport,
        );
        stack.page_at(p.y)
    }

    /// Durchgehende Ansicht: sichtbare Nachbarseiten der aktuellen Seite.
    pub fn visible_neighbors(&self, stack: &PageStack) -> Vec<usize> {
        let top = self
            .camera
            .screen_to_page_unclamped(0.0, 0.0, &self.viewport);
        let bottom =
            self.camera
                .screen_to_page_unclamped(0.0, self.size.height as f32, &self.viewport);
        stack.visible_neighbors(top.y, bottom.y)
    }

    /// Durchgehende Ansicht: Hält die Fenstermitte über dem Notizbuch (von der Oberkante der
    /// ersten bis zur Unterkante der letzten Seite, waagerecht innerhalb der Seiten), damit
    /// man nicht ins Leere scrollt. `true`, wenn der Ausschnitt korrigiert wurde.
    pub fn clamp_scroll(&mut self, stack: &PageStack) -> bool {
        let center = [
            self.viewport.page_x + self.viewport.page_width / 2.0,
            self.size.height as f32 / 2.0,
        ];
        let changed = self
            .camera
            .clamp_continuous_center(center, stack, &self.viewport);
        if changed {
            self.update_uniforms();
        }
        changed
    }

    /// Endless page: keeps the view right of and below `limit` (see
    /// [`crate::document::Page::endless_scroll_limit`]). `true` if the view was corrected.
    pub fn clamp_endless(&mut self, limit: glam::Vec2) -> bool {
        let changed = self
            .camera
            .clamp_top_left([self.ui_inset_left, 0.0], limit, &self.viewport);
        if changed {
            self.update_uniforms();
        }
        changed
    }

    /// Durchgehende Ansicht: Scrollt senkrecht, bis die Fenstermitte auf Höhe `y`
    /// (Koordinaten der aktuellen Seite) liegt.
    pub fn scroll_center_to(&mut self, y: f32) {
        let c = self.camera.screen_to_page_unclamped(
            self.viewport.page_x + self.viewport.page_width / 2.0,
            self.size.height as f32 / 2.0,
            &self.viewport,
        );
        self.camera.pan_offset[1] += c.y - y;
        self.update_uniforms();
    }

    /// Durchgehende Ansicht: Die Seite mit der Ecke `origin` (Koordinaten der aktuellen Seite)
    /// wird zur Bezugsseite, ohne dass sich der Bildausschnitt bewegt. Danach ist die neue
    /// Seite mit [`Self::sync_page`] zu setzen.
    pub fn shift_reference_page(&mut self, origin: glam::Vec2) {
        self.camera.shift_reference_page(origin);
        self.update_uniforms();
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

    /// Setzt die Papierfarbe und ob dunkle Tinte auf dunklem Papier aufgehellt wird.
    pub fn set_paper_theme(&mut self, theme: PaperTheme, adapt_ink: bool) {
        self.adapt_ink = adapt_ink;
        if self.paper_theme != theme {
            self.paper_theme = theme;
            self.queue.write_buffer(
                &self.paper_buffer,
                0,
                bytemuck::cast_slice(&generate_paper_quad(theme.paper_color(), self.page_size)),
            );
            self.upload_background_mesh();
        }
        self.update_uniforms();
    }

    /// Setzt die Farbe von Karos, Linien und Punkten.
    pub fn set_pattern_color(&mut self, pattern: PatternColor) {
        if self.pattern != pattern {
            self.pattern = pattern;
            self.upload_background_mesh();
        }
    }

    /// Setzt den Seitenhintergrund und aktualisiert bei Bedarf den Vertex-Puffer.
    pub fn set_background(&mut self, background: PageBackground) {
        if self.current_background == background {
            return;
        }
        self.current_background = background;
        self.upload_background_mesh();
    }

    /// Erzeugt das Muster für Hintergrund und Papierfarbe neu und lädt es auf die GPU.
    fn upload_background_mesh(&mut self) {
        let mesh = generate_pattern_quad(
            self.current_background,
            self.pattern.color(self.paper_theme),
            self.page_size,
        );
        self.background_vertex_count = mesh.len() as u32;
        if !mesh.is_empty() {
            self.queue
                .write_buffer(&self.background_buffer, 0, bytemuck::cast_slice(&mesh));
        }
    }

    /// Synchronisiert die GPU-Puffer mit den Daten der aktiven Seite.
    pub fn sync_page(&mut self, page: &mut Page) {
        page.rebuild_cache_if_dirty();
        // Die aktuelle Seite kann sich jetzt ändern: als Nachbarseite später neu hochladen
        self.neighbors.retain(|n| n.fingerprint.page_id != page.id);
        if self.page_size != page.size {
            self.page_size = page.size;
            self.queue.write_buffer(
                &self.paper_buffer,
                0,
                bytemuck::cast_slice(&generate_paper_quad(
                    self.paper_theme.paper_color(),
                    self.page_size,
                )),
            );
            self.upload_background_mesh();
            self.recalculate_viewport();
        }
        self.set_background(page.background);
        let quads = self.page_image_quads(page, self.hidden_text);
        self.image_layer.set_quads(quads);

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

    /// Durchgehende Ansicht: übernimmt die sichtbaren Nachbarseiten mit ihrem Versatz zur
    /// aktuellen Seite (in Seiten). Die Vertex-Caches der Seiten müssen aktuell sein
    /// ([`Page::rebuild_cache_if_dirty`]); unveränderte Seiten werden nicht erneut hochgeladen.
    /// Eine leere Liste schaltet die Nachbarseiten ab.
    pub fn set_neighbor_pages(&mut self, pages: &[(glam::Vec2, &Page)]) {
        let mut old = std::mem::take(&mut self.neighbors);
        let mut image_groups = Vec::with_capacity(pages.len());
        for &(offset, page) in pages {
            let fingerprint = NeighborFingerprint::of(page, self.paper_theme, self.pattern);
            let mut neighbor = match old.iter().position(|n| n.fingerprint == fingerprint) {
                Some(i) => old.swap_remove(i),
                None => self.upload_neighbor(page, fingerprint),
            };
            neighbor.offset = offset;
            self.queue.write_buffer(
                &neighbor.camera_buffer,
                0,
                bytemuck::cast_slice(&[self.camera_uniforms(offset)]),
            );
            self.neighbors.push(neighbor);
            image_groups.push(self.page_image_quads(page, None));
        }
        self.image_layer.set_neighbor_quads(image_groups);
    }

    /// Lädt Raster und Striche einer Nachbarseite auf die GPU.
    fn upload_neighbor(&self, page: &Page, fingerprint: NeighborFingerprint) -> NeighborPage {
        let camera_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Neighbor Camera Uniform Buffer"),
            size: std::mem::size_of::<CameraUniforms>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let camera_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Neighbor Camera Bind Group"),
            layout: &self.camera_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });
        let vertex_buffer_with = |label: &str, vertices: &[InkVertex]| {
            (!vertices.is_empty()).then(|| {
                let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(label),
                    size: std::mem::size_of_val(vertices) as u64,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                self.queue
                    .write_buffer(&buffer, 0, bytemuck::cast_slice(vertices));
                buffer
            })
        };
        // Eigenes Papier und Muster (Nachbarseiten können anders groß sein)
        let paper = generate_paper_quad(self.paper_theme.paper_color(), page.size);
        let pattern = generate_pattern_quad(
            page.background,
            self.pattern.color(self.paper_theme),
            page.size,
        );
        let paper_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Neighbor Paper Buffer"),
            size: std::mem::size_of_val(paper.as_slice()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.queue
            .write_buffer(&paper_buffer, 0, bytemuck::cast_slice(&paper));
        let pattern_buffer = (!pattern.is_empty()).then(|| {
            let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Neighbor Pattern Buffer"),
                size: std::mem::size_of_val(pattern.as_slice()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.queue
                .write_buffer(&buffer, 0, bytemuck::cast_slice(&pattern));
            buffer
        });
        NeighborPage {
            fingerprint,
            offset: glam::Vec2::ZERO,
            camera_buffer,
            camera_bind_group,
            paper_buffer,
            pattern_buffer,
            vertex_buffer: vertex_buffer_with("Neighbor Strokes Buffer", &page.vertex_cache),
            stroke_ranges: page.stroke_ranges.clone(),
        }
    }

    /// Lädt Bilder und PDF-Seiten der aktuellen Seite, die noch nicht auf der GPU liegen.
    /// Vor jedem Zeichnen aufrufen; ohne Änderungen kostet das fast nichts.
    pub fn prepare_images(
        &mut self,
        load: &mut dyn FnMut(&str) -> crate::image_layer::TextureSource,
    ) {
        // Text schärfer rastern, sobald weiter hineingezoomt wird (und gröber beim Herauszoomen)
        let resolution = text_resolution(self.viewport.scale * self.camera.zoom);
        if resolution != self.text_resolution {
            self.text_resolution = resolution;
            self.image_layer
                .forget_textures(|key| key.starts_with(TEXT_KEY_PREFIX));
        }
        // Importierte PDF-Seiten schärfer rendern, sobald die Standardauflösung vergrößert würde
        let page_long_edge = self.page_size.max_element() * self.viewport.scale * self.camera.zoom;
        self.image_layer.set_pdf_pixels(
            (page_long_edge > crate::pdf_doc::DISPLAY_RENDER_PIXELS as f32 * 1.25)
                .then_some(crate::pdf_doc::ZOOMED_RENDER_PIXELS),
        );
        let specs = &self.text_specs;
        self.image_layer
            .prepare(&self.device, &self.queue, &mut |key| match specs.get(key) {
                Some(spec) => crate::image_layer::TextureSource::Ready(crate::text::rasterize(
                    &spec.text,
                    spec.color,
                    resolution * spec.scale,
                )),
                None => load(key),
            });
        let layer = &self.image_layer;
        self.text_specs.retain(|key, _| layer.uses(key));
    }

    /// Blendet das gerade bearbeitete Textfeld aus (wirksam ab dem nächsten `sync_page`).
    pub fn set_hidden_text(&mut self, id: Option<u64>) {
        self.hidden_text = id;
    }

    /// Bilder einer Seite für die Bildebene: zuerst die PDF-Seite, darüber die eingefügten
    /// Bilder und Textfelder (außer `hidden`) in ihrer Reihenfolge.
    fn page_image_quads(
        &mut self,
        page: &Page,
        hidden: Option<u64>,
    ) -> Vec<(String, [glam::Vec2; 4])> {
        let adapt = self.adapt_ink && self.paper_theme.is_dark();
        let mut quads: Vec<_> = page
            .pdf_background
            .iter()
            .map(|pdf| (pdf.key(), pdf.corners(page.size)))
            .collect();
        for stroke in &page.strokes {
            let Some(corners) = stroke.object_corners() else {
                continue;
            };
            if let Some(asset) = &stroke.image {
                quads.push((asset.clone(), corners));
            } else if let Some(text) = &stroke.text {
                if Some(stroke.id) == hidden {
                    continue;
                }
                let color = if adapt {
                    crate::background::adapt_ink_color(stroke.color)
                } else {
                    stroke.color
                };
                let key = text_key(text, color);
                let scale = ((corners[1] - corners[0]).length() / text.wrap_width.max(text.size))
                    .max((corners[3] - corners[0]).length() / text.layout().height());
                self.text_specs.entry(key.clone()).or_insert(TextSpec {
                    text: text.clone(),
                    color,
                    scale,
                });
                quads.push((key, corners));
            }
        }
        quads
    }

    /// Wird noch eine PDF-Seite im Hintergrund gerendert?
    pub fn is_waiting_for_images(&self) -> bool {
        self.image_layer.is_waiting()
    }

    /// Startet einen neuen Live-Strich in DIN-A4-Seitenkoordinaten.
    pub fn start_stroke(&mut self, page_x: f32, page_y: f32, pressure: f32, timestamp: f64) {
        self.filter.reset();
        self.live_samples.clear();
        self.live_control_points.clear();
        self.live_vertices.clear();
        self.live_vertex_count = 0;

        // Beginnt der Strich an einer Linealkante, bleibt er bis zum Abheben auf ihr
        let half_width = self.current_stroke_width_mm * crate::document::UNITS_PER_MM / 2.0;
        self.ruler_lock = self
            .ruler
            .and_then(|r| r.edge_lock(glam::Vec2::new(page_x, page_y), half_width));
        let (page_x, page_y) = self.constrain_to_ruler(page_x, page_y);
        self.live_raw_samples.clear();
        self.push_raw_sample(page_x, page_y, pressure, timestamp);

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
        let (page_x, page_y) = self.constrain_to_ruler(page_x, page_y);
        self.push_raw_sample(page_x, page_y, pressure, timestamp);
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

    /// Merkt sich einen ungefilterten Eingabepunkt (Punkte näher als 0,03 mm am vorigen
    /// entfallen).
    fn push_raw_sample(&mut self, x: f32, y: f32, pressure: f32, timestamp: f64) {
        let position = glam::Vec2::new(x, y);
        if !position.is_finite() || !pressure.is_finite() {
            return;
        }
        if let Some(last) = self.live_raw_samples.last() {
            if (position - last.position).length_squared() < 0.09 {
                return;
            }
        }
        self.live_raw_samples.push(StylusSample {
            position,
            pressure: pressure.clamp(0.0, 1.0),
            timestamp,
        });
    }

    /// Projiziert einen Punkt auf die Linealkante, falls der Live-Strich dort eingerastet ist.
    fn constrain_to_ruler(&self, x: f32, y: f32) -> (f32, f32) {
        match self.ruler_lock {
            Some(lock) => lock.project(glam::Vec2::new(x, y)).into(),
            None => (x, y),
        }
    }

    /// Ist der aktuelle Live-Strich an einer Linealkante eingerastet?
    pub fn is_ruler_locked(&self) -> bool {
        self.ruler_lock.is_some()
    }

    /// Bricht den aktuellen Live-Strich ab und setzt Puffer sowie 1€-Filter sauber zurück.
    pub fn cancel_stroke(&mut self) {
        self.live_samples.clear();
        self.live_raw_samples.clear();
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

        // Post-Stroke Finalisierung (Glättung, RDP, Ecken, runde Kappen) aus den ungefilterten
        // Punkten: Der 1€-Filter dient nur der Anzeige während des Schreibens
        let source = if self.live_raw_samples.is_empty() {
            &self.live_samples
        } else {
            &self.live_raw_samples
        };
        let stroke = spline::finalize_stroke_with_config(
            stroke_id,
            source,
            self.current_ink_color,
            &self.spline_config,
        );
        let mut stroke = stroke;
        stroke.width = self.current_stroke_width_mm * crate::document::UNITS_PER_MM;
        stroke.finalized = true;

        // Temporären GPU-Live-Buffer vollständig leeren
        self.live_samples.clear();
        self.live_raw_samples.clear();
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

        // Gleiche Tessellierung wie beim Absetzen (runde Kappen und Ecken), damit der Strich
        // beim Abheben nicht sichtbar springt (Issue #33)
        let color = self.current_ink_color;
        let points = spline::rdp_filter_samples(&self.live_samples, spline::RDP_TOLERANCE_UNITS);
        let corners = spline::detect_corners(&points, spline::CORNER_ANGLE_THRESHOLD_DEG);
        let raw_strip =
            spline::generate_finalized_triangle_strip(&points, &corners, &self.spline_config);
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

        let Some(surface) = &self.surface else {
            for id in &textures_delta.free {
                self.egui_renderer.free_texture(id);
            }
            return Ok(());
        };
        let output = surface.get_current_texture()?;
        let view = output.texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(render_format(&self.config)),
            ..Default::default()
        });

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
                // Mit Multisampling ins Zwischenbild zeichnen und in die Fensterfläche auflösen
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: self.msaa_view.as_ref().unwrap_or(&view),
                    resolve_target: self.msaa_view.as_ref().map(|_| &view),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(self.paper_theme.desk_color()), // Tischhintergrund
                        store: if self.msaa_view.is_some() {
                            wgpu::StoreOp::Discard
                        } else {
                            wgpu::StoreOp::Store
                        },
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.stencil_view,
                    depth_ops: None,
                    stencil_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0),
                        store: wgpu::StoreOp::Discard,
                    }),
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            let mut stencil_ref = 0;

            // 0. Nachbarseiten der durchgehenden Ansicht (je mit eigener Kamera)
            for (i, neighbor) in self.neighbors.iter().enumerate() {
                render_pass.set_bind_group(0, &neighbor.camera_bind_group, &[]);
                render_pass.set_pipeline(&self.triangle_pipeline);
                render_pass.set_vertex_buffer(0, neighbor.paper_buffer.slice(..));
                render_pass.draw(0..6, 0..1);
                if let Some(buffer) = &neighbor.pattern_buffer {
                    render_pass.set_pipeline(&self.pattern_pipeline);
                    render_pass.set_vertex_buffer(0, buffer.slice(..));
                    render_pass.draw(0..6, 0..1);
                }
                self.image_layer.draw(&mut render_pass, i + 1);
                if let Some(buffer) = &neighbor.vertex_buffer {
                    render_pass.set_pipeline(&self.strip_pipeline);
                    render_pass.set_vertex_buffer(0, buffer.slice(..));
                    for range in &neighbor.stroke_ranges {
                        if range.end > range.start {
                            self.next_stroke_stencil(&mut render_pass, &mut stencil_ref);
                            render_pass.draw(range.clone(), 0..1);
                        }
                    }
                }
            }

            // 2D-Kamera Uniforms (Scale & Offset) binden
            render_pass.set_bind_group(0, &self.camera_bind_group, &[]);

            // 1. DIN-A4-Blatt in Papierfarbe zeichnen (Farbe ist direkt im Paper-Quad Vertex hinterlegt)
            render_pass.set_pipeline(&self.triangle_pipeline);
            render_pass.set_vertex_buffer(0, self.paper_buffer.slice(..));
            render_pass.draw(0..6, 0..1);

            // 2. Hintergrundmuster (5mm-Gitter, liniert oder Punktraster mit Vertex-Farbe)
            if self.background_vertex_count > 0 {
                render_pass.set_pipeline(&self.pattern_pipeline);
                render_pass.set_vertex_buffer(0, self.background_buffer.slice(..));
                render_pass.draw(0..self.background_vertex_count, 0..1);
            }

            // 3. Eingefügte Bilder (unter der Tinte)
            self.image_layer.draw(&mut render_pass, 0);

            // 4. Abgeschlossene Striche der aktiven Seite rendern (TriangleStrip mit Vertex-Farben)
            render_pass.set_pipeline(&self.strip_pipeline);

            if self.committed_vertex_count > 0 {
                render_pass.set_vertex_buffer(0, self.committed_vertex_buffer.slice(..));
                for range in &self.committed_strokes {
                    if range.end > range.start {
                        self.next_stroke_stencil(&mut render_pass, &mut stencil_ref);
                        render_pass.draw(range.clone(), 0..1);
                    }
                }
            }

            // 5. Temporäre Live-Geometrie des aktuellen Strichs rendern (Zero Latency)
            if self.live_vertex_count > 0 {
                render_pass.set_vertex_buffer(0, self.live_vertex_buffer.slice(..));
                self.next_stroke_stencil(&mut render_pass, &mut stencil_ref);
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

    /// Gibt dem nächsten Strich eine eigene Stencil-Nummer: Die Strich-Pipeline zeichnet nur
    /// Pixel, die noch nicht diese Nummer tragen. Nach 255 Strichen wird der Stencil-Puffer
    /// zurückgesetzt, damit kein späterer Strich die Nummer eines früheren wiederfindet.
    fn next_stroke_stencil<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>, stencil_ref: &mut u32) {
        if *stencil_ref == u32::from(u8::MAX) {
            pass.set_pipeline(&self.stencil_reset_pipeline);
            pass.draw(0..3, 0..1);
            pass.set_pipeline(&self.strip_pipeline);
            *stencil_ref = 0;
        }
        *stencil_ref += 1;
        pass.set_stencil_reference(*stencil_ref);
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

/// Format, in das gezeichnet wird: ohne sRGB-Umrechnung. Die Farben werden also in sRGB
/// gemischt – wie im PDF-Export, damit halbtransparente Tinte (Textmarker) am Bildschirm
/// genauso aussieht. Tintenfarben sind sRGB-Werte, Papier- und Musterfarben rechnet der
/// Shader aus linearen Werten um.
fn render_format(config: &wgpu::SurfaceConfiguration) -> wgpu::TextureFormat {
    config.format.remove_srgb_suffix()
}

/// Multisampling der Zeichenfläche gegen gezackte Strichkanten.
const MSAA_SAMPLES: u32 = 4;

/// Zwischenbild für das Multisampling in Fenstergröße; `None` ohne Multisampling.
fn create_msaa_view(
    device: &wgpu::Device,
    config: &wgpu::SurfaceConfiguration,
    samples: u32,
) -> Option<wgpu::TextureView> {
    (samples > 1).then(|| {
        device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("MSAA Color Target"),
                size: wgpu::Extent3d {
                    width: config.width.max(1),
                    height: config.height.max(1),
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: samples,
                dimension: wgpu::TextureDimension::D2,
                format: render_format(config),
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default())
    })
}

/// Format des Stencil-Puffers der Zeichenfläche.
const STENCIL_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth24PlusStencil8;

/// Stencil-Puffer in Fenstergröße (mit derselben Abtastzahl wie die Zeichenfläche).
fn create_stencil_view(
    device: &wgpu::Device,
    config: &wgpu::SurfaceConfiguration,
    samples: u32,
) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("Stroke Stencil Buffer"),
            size: wgpu::Extent3d {
                width: config.width.max(1),
                height: config.height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: samples,
            dimension: wgpu::TextureDimension::D2,
            format: STENCIL_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        })
        .create_view(&wgpu::TextureViewDescriptor::default())
}

/// Stencil-Zustand mit gleicher Prüfung und Aktion für Vorder- und Rückseiten.
fn stencil_state(
    compare: wgpu::CompareFunction,
    pass_op: wgpu::StencilOperation,
) -> wgpu::DepthStencilState {
    let face = wgpu::StencilFaceState {
        compare,
        fail_op: wgpu::StencilOperation::Keep,
        depth_fail_op: wgpu::StencilOperation::Keep,
        pass_op,
    };
    wgpu::DepthStencilState {
        format: STENCIL_FORMAT,
        depth_write_enabled: false,
        depth_compare: wgpu::CompareFunction::Always,
        stencil: wgpu::StencilState {
            front: face,
            back: face,
            read_mask: 0xff,
            write_mask: 0xff,
        },
        bias: wgpu::DepthBiasState::default(),
    }
}

/// Pipelines ohne Strichnummer (Papier, Muster, Bilder) lassen den Stencil-Puffer unberührt.
pub(crate) fn passive_stencil() -> wgpu::DepthStencilState {
    stencil_state(wgpu::CompareFunction::Always, wgpu::StencilOperation::Keep)
}

/// Striche: nur Pixel, die die eigene Nummer noch nicht tragen, und diese dann markieren.
fn stroke_stencil() -> wgpu::DepthStencilState {
    stencil_state(
        wgpu::CompareFunction::NotEqual,
        wgpu::StencilOperation::Replace,
    )
}

/// Zurücksetzen: überall 0 eintragen.
fn stencil_reset() -> wgpu::DepthStencilState {
    stencil_state(wgpu::CompareFunction::Always, wgpu::StencilOperation::Zero)
}

/// Zu rasterndes Textfeld.
struct TextSpec {
    text: crate::text::TextBox,
    /// Angezeigte (ggf. aufgehellte) Farbe
    color: [f32; 4],
    /// Vergrößerung des Felds durch Skalieren der Auswahl
    scale: f32,
}

/// Schlüssel der Texttexturen beginnen hiermit.
const TEXT_KEY_PREFIX: &str = "text:";

/// Texturschlüssel eines Textfelds: ändert sich mit Inhalt, Satz und Farbe.
fn text_key(text: &crate::text::TextBox, color: [f32; 4]) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.content.hash(&mut hasher);
    text.font.hash(&mut hasher);
    for v in [text.size, text.wrap_width].into_iter().chain(color) {
        v.to_bits().hash(&mut hasher);
    }
    format!("{TEXT_KEY_PREFIX}{:016x}", hasher.finish())
}

/// Auflösung der Texttexturen für `screen_px_per_unit` Bildschirmpixel je Seiteneinheit:
/// auf Viertel-Zweierpotenzen aufgerundet (⅛ bis 8). So wird nicht bei jedem Zoomschritt neu
/// gerastert, und die Textur ist höchstens ein Fünftel größer als dargestellt – stärker
/// verkleinert wirkt die dünne Schrift blass.
fn text_resolution(screen_px_per_unit: f32) -> f32 {
    let steps = (screen_px_per_unit.max(1e-3).log2() * 4.0).ceil();
    2.0_f32.powf(steps / 4.0).clamp(0.125, 8.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_text_resolution_is_quantized_and_never_below_screen() {
        for px in [0.2_f32, 0.27, 0.5, 1.3, 3.9] {
            let r = text_resolution(px);
            assert!(r >= px - 1e-4 && r < px * 1.2 + 1e-4, "{px} → {r}");
        }
        assert_eq!(text_resolution(0.27), text_resolution(0.28));
        assert_eq!(text_resolution(100.0), 8.0);
    }

    #[test]
    fn test_choose_present_mode() {
        use wgpu::PresentMode::{Fifo, Immediate, Mailbox};
        assert_eq!(choose_present_mode(&[Fifo, Mailbox]), Some(Mailbox));
        assert_eq!(choose_present_mode(&[Fifo, Immediate]), Some(Fifo));
        assert_eq!(choose_present_mode(&[Immediate]), Some(Immediate));
        // Keine Darstellungsmodi (z. B. kein Zugriff auf /dev/dri): kein Absturz, sondern None
        assert_eq!(choose_present_mode(&[]), None);
    }

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
        // Muss dem WGSL-Layout entsprechen: vec2 + vec2 + vec4
        assert_eq!(std::mem::size_of::<CameraUniforms>(), 32);
    }

    #[test]
    fn test_shader_is_valid_and_matches_uniforms() {
        // Ein Shaderfehler fiele sonst erst beim Programmstart auf
        let module = naga::front::wgsl::parse_str(include_str!("shader.wgsl"))
            .unwrap_or_else(|e| panic!("WGSL-Fehler: {e}"));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::empty(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("Shader ungültig: {e:?}"));

        for name in ["vs_main", "fs_main", "fs_ink", "vs_image", "fs_image"] {
            assert!(
                module.entry_points.iter().any(|ep| ep.name == name),
                "Einstiegspunkt {name} fehlt"
            );
        }
        let camera = module
            .types
            .iter()
            .find(|(_, ty)| ty.name.as_deref() == Some("CameraUniforms"))
            .expect("CameraUniforms fehlt im Shader")
            .1;
        match camera.inner {
            naga::TypeInner::Struct { span, .. } => {
                assert_eq!(span as usize, std::mem::size_of::<CameraUniforms>())
            }
            ref other => panic!("CameraUniforms ist kein Struct: {other:?}"),
        }
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
