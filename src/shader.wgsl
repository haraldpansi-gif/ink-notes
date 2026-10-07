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
