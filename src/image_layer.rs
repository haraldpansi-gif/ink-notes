//! GPU-Darstellung eingefügter Bilder: ein texturiertes Viereck pro Bild, gezeichnet nach dem
//! Papier und vor der Tinte.
//!
//! Texturen werden beim ersten Bedarf aus dem Asset-Ordner geladen (verkleinert auf
//! `assets::MAX_DISPLAY_DIMENSION`) und mit Mipmaps versehen, damit verkleinerte Bilder
//! beim Herauszoomen nicht flimmern.

use std::collections::{HashMap, HashSet};

use crate::assets::{AssetStore, RgbaImage, MAX_DISPLAY_DIMENSION};

/// Vertex eines Bildvierecks: Seitenkoordinate und Texturkoordinate.
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ImageVertex {
    pub position: [f32; 2],
    pub uv: [f32; 2],
}

impl ImageVertex {
    const ATTRIBUTES: [wgpu::VertexAttribute; 2] =
        wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2];

    fn desc() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<ImageVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBUTES,
        }
    }
}

/// Zwei Dreiecke für ein Bild mit den Ecken oben links, oben rechts, unten rechts, unten links.
pub fn quad_vertices(corners: &[glam::Vec2; 4]) -> [ImageVertex; 6] {
    let v = |i: usize, uv: [f32; 2]| ImageVertex {
        position: corners[i].to_array(),
        uv,
    };
    let (tl, tr, br, bl) = (
        v(0, [0.0, 0.0]),
        v(1, [1.0, 0.0]),
        v(2, [1.0, 1.0]),
        v(3, [0.0, 1.0]),
    );
    [tl, tr, bl, bl, tr, br]
}

/// Mipmap-Stufen eines Bildes, jeweils halbe Kantenlänge bis 1 × 1.
pub fn mip_chain(base: &RgbaImage) -> Vec<RgbaImage> {
    let mut levels = vec![base.clone()];
    let Some(mut current) =
        image::RgbaImage::from_raw(base.width, base.height, base.pixels.clone())
    else {
        return levels;
    };
    while current.width() > 1 || current.height() > 1 {
        let (w, h) = ((current.width() / 2).max(1), (current.height() / 2).max(1));
        current = image::imageops::resize(&current, w, h, image::imageops::FilterType::Triangle);
        levels.push(RgbaImage {
            width: w,
            height: h,
            pixels: current.as_raw().clone(),
        });
    }
    levels
}

/// Name der Ersatztextur für Bilder, deren Datei fehlt oder beschädigt ist.
const MISSING: &str = "";
/// So viele Texturen bleiben höchstens im Speicher, auch wenn sie auf der aktuellen Seite
/// nicht mehr gebraucht werden (schnelles Zurückblättern).
const TEXTURE_CACHE_LIMIT: usize = 24;

pub struct ImageLayer {
    pipeline: wgpu::RenderPipeline,
    texture_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    textures: HashMap<String, wgpu::BindGroup>,
    /// Bilder der aktuellen Seite: Asset-Name und Ecken
    quads: Vec<(String, [glam::Vec2; 4])>,
    vertex_buffer: Option<wgpu::Buffer>,
    needs_upload: bool,
}

impl ImageLayer {
    pub fn new(
        device: &wgpu::Device,
        shader: &wgpu::ShaderModule,
        camera_layout: &wgpu::BindGroupLayout,
        format: wgpu::TextureFormat,
    ) -> Self {
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Image Texture Bind Group Layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
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
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Image Pipeline Layout"),
            bind_group_layouts: &[camera_layout, &texture_layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Image Render Pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: shader,
                entry_point: "vs_image",
                buffers: &[ImageVertex::desc()],
            },
            fragment: Some(wgpu::FragmentState {
                module: shader,
                entry_point: "fs_image",
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Image Sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        Self {
            pipeline,
            texture_layout,
            sampler,
            textures: HashMap::new(),
            quads: Vec::new(),
            vertex_buffer: None,
            needs_upload: false,
        }
    }

    /// Übernimmt die Bilder der aktuellen Seite (Asset-Name und Ecken in Seitenkoordinaten).
    pub fn set_quads(&mut self, quads: Vec<(String, [glam::Vec2; 4])>) {
        if quads != self.quads {
            self.quads = quads;
            self.needs_upload = true;
        }
    }

    /// Lädt fehlende Texturen und den Vertex-Puffer hoch. Vor jedem Zeichnen aufrufen;
    /// ohne Änderungen kostet das nichts.
    pub fn prepare(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, assets: &AssetStore) {
        if !self.needs_upload {
            return;
        }
        self.needs_upload = false;

        let missing: Vec<String> = self
            .quads
            .iter()
            .map(|(asset, _)| asset)
            .filter(|asset| !self.textures.contains_key(*asset))
            .cloned()
            .collect();
        for asset in missing {
            if self.textures.contains_key(&asset) {
                continue;
            }
            let bind_group = match assets.load_rgba(&asset, MAX_DISPLAY_DIMENSION) {
                Ok(image) => self.upload_texture(device, queue, &asset, &image),
                Err(err) => {
                    eprintln!("[Bilder] '{asset}' kann nicht angezeigt werden: {err}");
                    self.missing_texture(device, queue)
                }
            };
            self.textures.insert(asset, bind_group);
        }

        // Nicht mehr benötigte Texturen freigeben, wenn der Speicher voll wird
        if self.textures.len() > TEXTURE_CACHE_LIMIT {
            let used: HashSet<&String> = self.quads.iter().map(|(a, _)| a).collect();
            self.textures.retain(|name, _| used.contains(name));
        }

        let vertices: Vec<ImageVertex> = self
            .quads
            .iter()
            .flat_map(|(_, corners)| quad_vertices(corners))
            .collect();
        if vertices.is_empty() {
            return;
        }
        let bytes: &[u8] = bytemuck::cast_slice(&vertices);
        let too_small = self
            .vertex_buffer
            .as_ref()
            .map_or(true, |b| b.size() < bytes.len() as u64);
        if too_small {
            self.vertex_buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Image Vertex Buffer"),
                size: (bytes.len() as u64 * 2).max(1024),
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        if let Some(buffer) = &self.vertex_buffer {
            queue.write_buffer(buffer, 0, bytes);
        }
    }

    /// Zeichnet die Bilder der aktuellen Seite. Bind Group 0 (Kamera) muss gesetzt sein.
    pub fn draw<'a>(&'a self, render_pass: &mut wgpu::RenderPass<'a>) {
        let Some(buffer) = &self.vertex_buffer else {
            return;
        };
        if self.quads.is_empty() || self.needs_upload {
            return;
        }
        render_pass.set_pipeline(&self.pipeline);
        render_pass.set_vertex_buffer(0, buffer.slice(..));
        for (i, (asset, _)) in self.quads.iter().enumerate() {
            let Some(bind_group) = self.textures.get(asset) else {
                continue;
            };
            render_pass.set_bind_group(1, bind_group, &[]);
            let start = (i * 6) as u32;
            render_pass.draw(start..start + 6, 0..1);
        }
    }

    fn missing_texture(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> wgpu::BindGroup {
        // Hellgraue Fläche statt eines unsichtbaren Bildes
        let gray = RgbaImage {
            width: 1,
            height: 1,
            pixels: vec![200, 200, 200, 255],
        };
        self.upload_texture(device, queue, MISSING, &gray)
    }

    fn upload_texture(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        label: &str,
        image: &RgbaImage,
    ) -> wgpu::BindGroup {
        let levels = mip_chain(image);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: image.width,
                height: image.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: levels.len() as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        for (level, mip) in levels.iter().enumerate() {
            queue.write_texture(
                wgpu::ImageCopyTexture {
                    texture: &texture,
                    mip_level: level as u32,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &mip.pixels,
                wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(4 * mip.width),
                    rows_per_image: Some(mip.height),
                },
                wgpu::Extent3d {
                    width: mip.width,
                    height: mip.height,
                    depth_or_array_layers: 1,
                },
            );
        }
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Image Texture Bind Group"),
            layout: &self.texture_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quad_vertices_map_corners_to_texture_corners() {
        let corners = [
            glam::Vec2::new(10.0, 10.0),
            glam::Vec2::new(50.0, 10.0),
            glam::Vec2::new(50.0, 30.0),
            glam::Vec2::new(10.0, 30.0),
        ];
        let quad = quad_vertices(&corners);
        for v in quad {
            // Texturkoordinate entspricht der relativen Lage im Rechteck
            let u = (v.position[0] - 10.0) / 40.0;
            let w = (v.position[1] - 10.0) / 20.0;
            assert_eq!(v.uv, [u, w]);
        }
        // Beide Dreiecke decken alle vier Ecken ab
        for c in corners {
            assert!(quad.iter().any(|v| v.position == c.to_array()));
        }
    }

    #[test]
    fn test_mip_chain_halves_down_to_one_pixel() {
        let base = RgbaImage {
            width: 8,
            height: 3,
            pixels: vec![128; 8 * 3 * 4],
        };
        let levels = mip_chain(&base);
        let sizes: Vec<(u32, u32)> = levels.iter().map(|l| (l.width, l.height)).collect();
        assert_eq!(sizes, vec![(8, 3), (4, 1), (2, 1), (1, 1)]);
        for l in &levels {
            assert_eq!(l.pixels.len(), (l.width * l.height * 4) as usize);
        }
    }
}
