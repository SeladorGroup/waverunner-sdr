// Spectrum renderer shader.
//
// Vertex shader generates a filled polygon from FFT bin values.
// Fragment shader applies colormap based on amplitude.

struct Uniforms {
    min_db: f32,
    max_db: f32,
    num_bins: u32,
    viewport_width: f32,
    viewport_height: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var<storage, read> spectrum: array<f32>;
@group(0) @binding(2) var colormap_tex: texture_1d<f32>;
@group(0) @binding(3) var colormap_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) amplitude: f32,
}

// Vertex shader: generates two vertices per bin (top and bottom).
// vertex_index encodes: bin = index / 2, is_bottom = index % 2.
@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    let bin = vertex_index / 2u;
    let is_bottom = (vertex_index % 2u) == 1u;

    let db_range = uniforms.max_db - uniforms.min_db;
    let db_val = spectrum[bin];
    let normalized = clamp((db_val - uniforms.min_db) / db_range, 0.0, 1.0);

    // Map bin to x in clip space [-1, 1]
    let x = f32(bin) / f32(uniforms.num_bins - 1u) * 2.0 - 1.0;

    // Map amplitude to y in clip space [-1, 1]
    let y = select(normalized * 2.0 - 1.0, -1.0, is_bottom);

    var out: VertexOutput;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.amplitude = select(normalized, 0.0, is_bottom);
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let color = textureSample(colormap_tex, colormap_sampler, in.amplitude);
    return color;
}
