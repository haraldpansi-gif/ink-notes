// WGSL Shader für 2D-Vektor-Meshes der Inking-App
// Unterstützt echte Multi-Color-Vertizes und DIN-A4-Seitenkoordinatentransformation

struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) color: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

struct CameraUniforms {
    transform_scale: vec2<f32>,
    transform_offset: vec2<f32>,
    // x: 1.0 = dunkle Tinte für dunkles Papier aufhellen (siehe `adapt_ink_color`)
    ink_params: vec4<f32>,
};

@group(0) @binding(0)
var<uniform> camera: CameraUniforms;

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    let ndc = in.position * camera.transform_scale + camera.transform_offset;
    out.clip_position = vec4<f32>(ndc, 0.0, 1.0);
    out.color = in.color;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return in.color;
}

// Striche: bei dunklem Papier wird die HSL-Helligkeit dunkler Farben gespiegelt
// (L -> 1 - L, Farbton und Sättigung bleiben). Entspricht `background::adapt_ink_color`.
@fragment
fn fs_ink(in: VertexOutput) -> @location(0) vec4<f32> {
    if (camera.ink_params.x < 0.5) {
        return in.color;
    }
    let max_c = max(max(in.color.r, in.color.g), in.color.b);
    let min_c = min(min(in.color.r, in.color.g), in.color.b);
    let lift = max(1.0 - max_c - min_c, 0.0);
    return vec4<f32>(in.color.rgb + vec3<f32>(lift), in.color.a);
}

// Bildschirmfüllendes Dreieck ohne Farbe (setzt den Stencil-Puffer zurück)
@vertex
fn vs_fullscreen(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let x = f32((index << 1u) & 2u) * 2.0 - 1.0;
    let y = f32(index & 2u) * 2.0 - 1.0;
    return vec4<f32>(x, y, 0.0, 1.0);
}

@fragment
fn fs_none() -> @location(0) vec4<f32> {
    return vec4<f32>(0.0);
}

// Eingefügte Bilder: texturierte Vierecke unter der Tinte
struct ImageVertexInput {
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
};

struct ImageVertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@group(1) @binding(0)
var image_texture: texture_2d<f32>;
@group(1) @binding(1)
var image_sampler: sampler;

@vertex
fn vs_image(in: ImageVertexInput) -> ImageVertexOutput {
    var out: ImageVertexOutput;
    let ndc = in.position * camera.transform_scale + camera.transform_offset;
    out.clip_position = vec4<f32>(ndc, 0.0, 1.0);
    out.uv = in.uv;
    return out;
}

@fragment
fn fs_image(in: ImageVertexOutput) -> @location(0) vec4<f32> {
    return textureSample(image_texture, image_sampler, in.uv);
}

// Karo, Linien und Punkte: pro Pixel aus der Seitenkoordinate berechnet, damit die Abstände
// in jedem Zoom gleichmäßig bleiben. Linien und Punkte sind mindestens ein Pixel breit und
// kantengeglättet; werden die Abstände sehr klein, blendet das Muster aus statt zu flimmern.
struct PatternInput {
    @location(0) position: vec2<f32>,
    @location(1) color: vec4<f32>,
    // Art (1 Karo, 2 Linien, 3 Punkte), Abstand, Linienstärke, Punktgröße
    @location(2) params: vec4<f32>,
    // Seitenbreite, Seitenhöhe, Rand oben/unten bei Linien
    @location(3) bounds: vec4<f32>,
};

struct PatternOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) page: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) params: vec4<f32>,
    @location(3) bounds: vec4<f32>,
};

@vertex
fn vs_pattern(in: PatternInput) -> PatternOutput {
    var out: PatternOutput;
    let ndc = in.position * camera.transform_scale + camera.transform_offset;
    out.clip_position = vec4<f32>(ndc, 0.0, 1.0);
    out.page = in.position;
    out.color = in.color;
    out.params = in.params;
    out.bounds = in.bounds;
    return out;
}

// Deckung einer Linie bei `line` für die Koordinate `c` (`upp`: Seiteneinheiten je Pixel).
fn line_coverage(c: f32, line: f32, half_width: f32, upp: f32) -> f32 {
    let half_px = max(half_width / upp, 0.5);
    let d = abs(c - line) / upp;
    return clamp(half_px + 0.5 - d, 0.0, 1.0);
}

@fragment
fn fs_pattern(in: PatternOutput) -> @location(0) vec4<f32> {
    let p = in.page;
    let kind = in.params.x;
    let spacing = in.params.y;
    let upp = max(max(fwidth(p.x), fwidth(p.y)), 1e-4);
    let size = in.bounds.xy;
    // Unter 3 Pixel Abstand ausblenden (ganz herausgezoomt)
    let fade = clamp((spacing / upp - 3.0) / 3.0, 0.0, 1.0);

    var coverage = 0.0;
    if (kind < 1.5) {
        // Karo: senkrechte und waagerechte Linien bei k · Abstand, nicht am Blattrand
        let k = round(p / spacing);
        let line = k * spacing;
        if (k.x >= 1.0 && line.x < size.x - 0.01) {
            coverage = max(coverage, line_coverage(p.x, line.x, in.params.z * 0.5, upp));
        }
        if (k.y >= 1.0 && line.y < size.y - 0.01) {
            coverage = max(coverage, line_coverage(p.y, line.y, in.params.z * 0.5, upp));
        }
    } else if (kind < 2.5) {
        // Linien: waagerecht ab dem oberen Rand bis zum unteren Rand
        let margin = in.bounds.z;
        let k = round((p.y - margin) / spacing);
        let line = margin + k * spacing;
        if (k >= 0.0 && line <= size.y - margin + 0.01) {
            coverage = line_coverage(p.y, line, in.params.z * 0.5, upp);
        }
    } else {
        // Punkte: an den Kreuzungen des Karos
        let k = round(p / spacing);
        let point = k * spacing;
        if (all(k >= vec2<f32>(1.0)) && point.x < size.x - 0.01 && point.y < size.y - 0.01) {
            let radius_px = max(in.params.w * 0.5 / upp, 0.6);
            let d = length(p - point) / upp;
            coverage = clamp(radius_px + 0.5 - d, 0.0, 1.0);
        }
    }
    return vec4<f32>(in.color.rgb, in.color.a * coverage * fade);
}
