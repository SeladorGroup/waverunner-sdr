// Constellation renderer shader.
//
// Renders IQ sample pairs as a scatter plot. Each vertex is an IQ
// point; the vertex shader maps to clip space using the symmetric
// range parameter.

struct Uniforms {
    range: f32,
    point_size: f32,
    alpha: f32,
    _padding: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
}

// Each vertex carries an IQ pair (x=I, y=Q).
@vertex
fn vs_main(@location(0) iq: vec2<f32>) -> VertexOutput {
    // Map IQ to clip space: [-range, +range] → [-1, +1]
    let x = iq.x / uniforms.range;
    let y = iq.y / uniforms.range;

    var out: VertexOutput;
    out.position = vec4<f32>(x, y, 0.0, 1.0);

    // Color based on distance from origin (phase-dependent)
    let mag = length(iq) / uniforms.range;
    let hue = atan2(iq.y, iq.x) * 0.159154943; // / (2*PI)
    let r = clamp(abs(hue * 6.0 - 3.0) - 1.0, 0.0, 1.0);
    let g = clamp(2.0 - abs(hue * 6.0 - 2.0), 0.0, 1.0);
    let b = clamp(2.0 - abs(hue * 6.0 - 4.0), 0.0, 1.0);

    out.color = vec4<f32>(
        mix(0.5, r, mag),
        mix(0.5, g, mag),
        mix(0.5, b, mag),
        uniforms.alpha,
    );
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return in.color;
}
