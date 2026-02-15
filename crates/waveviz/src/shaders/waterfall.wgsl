// Waterfall renderer shader.
//
// Renders a 2D texture as a scrolling heatmap. New rows are written
// at a rotating index; the fragment shader applies the scroll offset
// to create the visual scroll effect.

struct Uniforms {
    min_db: f32,
    max_db: f32,
    write_row: u32,
    total_rows: u32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var waterfall_tex: texture_2d<f32>;
@group(0) @binding(2) var colormap_tex: texture_1d<f32>;
@group(0) @binding(3) var tex_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

// Full-screen quad via vertex index trick (no vertex buffer needed).
@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    var positions = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 1.0, -1.0),
        vec2<f32>(-1.0,  1.0),
        vec2<f32>(-1.0,  1.0),
        vec2<f32>( 1.0, -1.0),
        vec2<f32>( 1.0,  1.0),
    );

    var uvs = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 0.0),
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(1.0, 0.0),
    );

    var out: VertexOutput;
    out.position = vec4<f32>(positions[vertex_index], 0.0, 1.0);
    out.uv = uvs[vertex_index];
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // Apply scroll offset: shift the row coordinate by write_row
    let row_frac = in.uv.y;
    let row_offset = f32(uniforms.write_row) / f32(uniforms.total_rows);
    let scrolled_y = fract(row_frac + row_offset);

    let tex_coord = vec2<f32>(in.uv.x, scrolled_y);
    let db_val = textureSample(waterfall_tex, tex_sampler, tex_coord).r;

    let db_range = uniforms.max_db - uniforms.min_db;
    let normalized = clamp((db_val - uniforms.min_db) / db_range, 0.0, 1.0);

    let color = textureSample(colormap_tex, tex_sampler, normalized);
    return color;
}
