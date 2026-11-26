// The game's eye adaptation (see `grade.rs`): the frame's average
// luminance, from a histogram of its exposed HDR colour, and the exposure
// eased toward the one that brings it to the preset's middle grey, within the
// preset's range.

struct Eye {
    // x: the histogram's lowest log2 luminance (darker counts as it), y: its
    // span in stops, z: log2 of the middle grey, w: the frame's seconds.
    range: vec4<f32>,
    // x, y: the least and most shift (stops) from the camera's exposure;
    // z, w: the speeds to the dark and to the light (the share of the way a
    // 30 fps frame goes).
    limits: vec4<f32>,
    // x: 1 to go straight to the target (a load), y: 0 to hold at 1.
    flags: vec4<f32>,
}

@group(0) @binding(0) var<uniform> eye: Eye;
@group(0) @binding(1) var screen: texture_2d<f32>;
@group(0) @binding(2) var<storage, read_write> histogram: array<atomic<u32>, 64>;
// x: the scale on the exposed colour now, y: the frame's average log2
// luminance, z: the scale it eases toward.
@group(0) @binding(3) var<storage, read_write> state: vec4<f32>;

var<workgroup> bins: array<atomic<u32>, 64>;

const LUMA = vec3<f32>(0.2126, 0.7152, 0.0722);

@compute @workgroup_size(16, 16, 1)
fn histogram_pass(@builtin(global_invocation_id) id: vec3<u32>, @builtin(local_invocation_index) li: u32) {
    if li < 64u {
        atomicStore(&bins[li], 0u);
    }
    workgroupBarrier();
    let dim = textureDimensions(screen);
    if id.x < dim.x && id.y < dim.y {
        let lum = dot(textureLoad(screen, vec2<i32>(id.xy), 0).rgb, LUMA);
        var bin = 0u;
        if lum > exp2(eye.range.x) {
            bin = u32(saturate((log2(lum) - eye.range.x) / eye.range.y) * 62.0 + 1.0);
        }
        atomicAdd(&bins[bin], 1u);
    }
    workgroupBarrier();
    if li < 64u {
        atomicAdd(&histogram[li], atomicLoad(&bins[li]));
    }
}

@compute @workgroup_size(1, 1, 1)
fn average_pass() {
    var total = 0.0;
    var sum = 0.0;
    for (var i = 0u; i < 64u; i += 1u) {
        let n = f32(atomicLoad(&histogram[i]));
        atomicStore(&histogram[i], 0u);
        // (The darkest bin at the floor, black.)
        var v = eye.range.x;
        if i > 0u {
            v = eye.range.x + (f32(i) - 0.5) / 62.0 * eye.range.y;
        }
        sum += n * exp2(v);
        total += n;
    }
    // (The plain mean, not the log-average: with the log-average a sunny
    // noon sat ten times under its middle grey, at the most exposure and
    // blown out, where the presets have a scene sit at the least.)
    var avg = eye.range.x;
    if total > 0.0 {
        avg = log2(max(sum / total, exp2(eye.range.x)));
    }
    let goal = exp2(clamp(eye.range.z - avg, eye.limits.x, eye.limits.y));
    var now = state.x;
    if eye.flags.y < 0.5 {
        now = 1.0;
    } else if eye.flags.x > 0.5 || !(now > 0.0) {
        now = goal;
    } else {
        // (Slower into the dark than into the light, as the eye.)
        var speed = eye.limits.w;
        if goal > now {
            speed = eye.limits.z;
        }
        let k = 1.0 - pow(1.0 - clamp(speed, 0.0, 1.0), eye.range.w * 30.0);
        now += (goal - now) * k;
    }
    state = vec4(now, avg, goal, 0.0);
}
