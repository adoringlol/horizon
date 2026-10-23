struct Camera { view_projection: mat4x4<f32>, atlas: vec4<f32> }
@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var atlas_texture: texture_2d<f32>;
@group(1) @binding(1) var atlas_sampler: sampler;
struct Varying {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) @interpolate(flat) tile: u32,
}
@vertex fn vs(@location(0) position: vec3<f32>, @location(1) color: vec3<f32>, @location(2) uv: vec2<f32>, @location(3) tile: u32) -> Varying {
    var out: Varying;
    out.position = camera.view_projection * vec4<f32>(position, 1.0);
    out.color = color;
    out.uv = uv;
    out.tile = tile;
    return out;
}
@fragment fn fs(input: Varying) -> @location(0) vec4<f32> {
    let columns = u32(camera.atlas.x);
    let origin = vec2<f32>(f32(input.tile % columns), f32(input.tile / columns)) * camera.atlas.y;
    let pixel = clamp(fract(input.uv) * camera.atlas.y, vec2<f32>(0.5), vec2<f32>(camera.atlas.y - 0.5));
    let texel = textureSample(atlas_texture, atlas_sampler, (origin + pixel) / camera.atlas.z);
    if texel.a < 0.1 { discard; }
    return vec4<f32>(texel.rgb * input.color, texel.a);
}
