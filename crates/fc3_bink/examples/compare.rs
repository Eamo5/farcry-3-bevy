//! Checks the decoder against FFmpeg: video frame by frame (YUV and optionally RGBA), audio sample by
//! sample, and measures decode speed. FFmpeg's output is piped straight in, so nothing is written to disk.
//!
//! ```text
//! cargo run --release -p fc3_bink --example compare -- [--frames N] [--audio] [--rgba] [--bench] [--no-video] FILE...
//! ```
//!
//! FFmpeg is taken from the `FFMPEG` environment variable, or `ffmpeg` on the PATH.

use std::io::{BufReader, Read};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use fc3_bink::{Bink, Plane, BLOCK_KIND_NAMES};

struct Options {
    frames: Option<u32>,
    video: bool,
    audio: bool,
    rgba: bool,
    bench: bool,
}

fn main() -> Result<()> {
    let mut options = Options { frames: None, video: true, audio: false, rgba: false, bench: false };
    let mut files = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--frames" => options.frames = Some(args.next().context("--frames needs a count")?.parse()?),
            "--audio" => options.audio = true,
            "--rgba" => options.rgba = true,
            "--bench" => options.bench = true,
            "--no-video" => options.video = false,
            _ if arg.starts_with("--") => bail!("unknown option {arg}"),
            _ => files.push(arg),
        }
    }
    if files.is_empty() {
        bail!("usage: compare [--frames N] [--audio] [--rgba] [--bench] [--no-video] FILE...");
    }
    let mut failed = false;
    for file in &files {
        failed |= !compare_file(file, &options)?;
    }
    if failed {
        bail!("mismatches found");
    }
    Ok(())
}

fn ffmpeg() -> String {
    std::env::var("FFMPEG").unwrap_or_else(|_| "ffmpeg".into())
}

/// Runs FFmpeg on `file` with `args` and returns its stdout as a reader.
fn ffmpeg_pipe(file: &str, args: &[&str]) -> Result<(std::process::Child, BufReader<std::process::ChildStdout>)> {
    let mut child = Command::new(ffmpeg())
        .args(["-v", "error", "-nostdin", "-i", file])
        .args(args)
        .arg("-")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("running ffmpeg")?;
    let stdout = BufReader::with_capacity(1 << 20, child.stdout.take().unwrap());
    Ok((child, stdout))
}

/// Fills `buf` from `r`, returning false at a clean end of stream.
fn read_full(r: &mut impl Read, buf: &mut [u8]) -> Result<bool> {
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(&mut buf[filled..])? {
            0 if filled == 0 => return Ok(false),
            0 => bail!("ffmpeg output ended mid-frame"),
            n => filled += n,
        }
    }
    Ok(true)
}

#[derive(Default)]
struct Diff {
    samples: u64,
    differing: u64,
    max: u32,
    sum: u64,
    squares: u64,
}

impl Diff {
    fn add(&mut self, a: u8, b: u8) {
        let d = a.abs_diff(b) as u32;
        self.samples += 1;
        self.differing += (d != 0) as u64;
        self.max = self.max.max(d);
        self.sum += d as u64;
        self.squares += (d * d) as u64;
    }

    fn plane(&mut self, ours: &Plane, theirs: &[u8]) {
        for y in 0..ours.height() {
            for (&a, &b) in ours.row(y).iter().zip(&theirs[y * ours.width()..(y + 1) * ours.width()]) {
                self.add(a, b);
            }
        }
    }

    fn report(&self) -> String {
        if self.differing == 0 {
            return format!("bit-exact ({} samples)", self.samples);
        }
        let mse = self.squares as f64 / self.samples as f64;
        format!(
            "{} of {} samples differ, max {}, mean {:.4}, PSNR {:.2} dB",
            self.differing,
            self.samples,
            self.max,
            self.sum as f64 / self.samples as f64,
            10.0 * (255.0 * 255.0 / mse).log10()
        )
    }
}

fn compare_file(file: &str, options: &Options) -> Result<bool> {
    let data: Arc<[u8]> = std::fs::read(file).with_context(|| format!("reading {file}"))?.into();
    let mut bink = Bink::new(data)?;
    let (num, den) = bink.frame_rate();
    println!(
        "{file}: BIK{} {}x{} {:.3} fps, {} frames{}, {} audio track(s)",
        bink.revision(),
        bink.width(),
        bink.height(),
        num as f64 / den as f64,
        bink.frame_count(),
        if bink.has_alpha() { ", alpha" } else { "" },
        bink.audio_tracks().len()
    );
    for (i, t) in bink.audio_tracks().iter().enumerate() {
        println!("  track {i}: id {} {} Hz {} ch {:?}", t.id, t.sample_rate, t.channels, t.codec);
    }
    let frames = options.frames.unwrap_or(u32::MAX).min(bink.frame_count());
    let mut ok = true;
    if options.video {
        ok &= compare_video(file, &mut bink, frames)?;
    }
    if options.rgba {
        compare_rgba(file, &mut bink, frames)?;
    }
    if options.audio {
        for track in 0..bink.audio_tracks().len() {
            ok &= compare_audio(file, &bink, track)?;
        }
    }
    if options.bench {
        bench(&mut bink, frames)?;
    }
    Ok(ok)
}

fn compare_video(file: &str, bink: &mut Bink, frames: u32) -> Result<bool> {
    let (w, h) = (bink.width() as usize, bink.height() as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let alpha = bink.has_alpha();
    let count = frames.to_string();
    let format = if alpha { "yuva420p" } else { "yuv420p" };
    let (mut child, mut pipe) = ffmpeg_pipe(file, &["-map", "0:v:0", "-frames:v", &count, "-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", format])?;
    let mut buf = vec![0u8; w * h * (1 + alpha as usize) + 2 * cw * ch];
    let (mut y, mut u, mut v, mut a) = (Diff::default(), Diff::default(), Diff::default(), Diff::default());
    let (mut compared, mut differing, mut first_bad) = (0u32, 0u32, None);
    let mut keyframes = 0;
    while compared < frames && read_full(&mut pipe, &mut buf)? {
        let frame = bink.decode_frame(compared)?;
        let (theirs_y, rest) = buf.split_at(w * h);
        let (theirs_u, rest) = rest.split_at(cw * ch);
        let (theirs_v, theirs_a) = rest.split_at(cw * ch);
        let before = y.differing + u.differing + v.differing + a.differing;
        y.plane(frame.y(), theirs_y);
        u.plane(frame.u(), theirs_u);
        v.plane(frame.v(), theirs_v);
        if let Some(plane) = frame.alpha() {
            a.plane(plane, theirs_a);
        }
        if y.differing + u.differing + v.differing + a.differing != before {
            differing += 1;
            first_bad.get_or_insert(compared);
        }
        keyframes += bink.is_keyframe(compared) as u32;
        compared += 1;
    }
    let _ = child.kill();
    let _ = child.wait();
    println!(
        "  video: {compared} frames compared ({keyframes} keyframes), {differing} differ{}",
        first_bad.map_or(String::new(), |f| format!(", first at {f}"))
    );
    if compared < frames {
        println!("    ffmpeg stopped after {compared} of {frames} frames");
    }
    println!("    Y: {}", y.report());
    println!("    U: {}", u.report());
    println!("    V: {}", v.report());
    if alpha {
        println!("    A: {}", a.report());
    }
    let counts = bink.block_counts();
    let kinds: Vec<String> = (0..10).filter(|&k| counts[k] > 0).map(|k| format!("{} {}", BLOCK_KIND_NAMES[k], counts[k])).collect();
    let scaled: Vec<String> = (0..10).filter(|&k| counts[10 + k] > 0).map(|k| format!("{} {}", BLOCK_KIND_NAMES[k], counts[10 + k])).collect();
    if !kinds.is_empty() {
        println!("    blocks: {}; scaled: {}", kinds.join(", "), scaled.join(", "));
    }
    Ok(differing == 0 && compared > 0)
}

fn compare_rgba(file: &str, bink: &mut Bink, frames: u32) -> Result<()> {
    let (w, h) = (bink.width() as usize, bink.height() as usize);
    let count = frames.to_string();
    let (mut child, mut pipe) = ffmpeg_pipe(file, &["-map", "0:v:0", "-frames:v", &count, "-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", "rgba"])?;
    let mut buf = vec![0u8; w * h * 4];
    let mut ours = vec![0u8; w * h * 4];
    let mut channels: [Diff; 4] = Default::default();
    let mut compared = 0;
    while compared < frames && read_full(&mut pipe, &mut buf)? {
        bink.decode_frame(compared)?.to_rgba(&mut ours);
        for (i, (&a, &b)) in ours.iter().zip(&buf).enumerate() {
            channels[i % 4].add(a, b);
        }
        compared += 1;
    }
    let _ = child.kill();
    let _ = child.wait();
    println!("  rgba: {compared} frames compared against ffmpeg -pix_fmt rgba");
    for (name, diff) in ["R", "G", "B", "A"].iter().zip(&channels) {
        println!("    {name}: {}", diff.report());
    }
    Ok(())
}

fn compare_audio(file: &str, bink: &Bink, track: usize) -> Result<bool> {
    let channels = bink.audio_tracks()[track].channels.to_string();
    let map = format!("0:a:{track}");
    let (mut child, mut pipe) = ffmpeg_pipe(file, &["-map", &map, "-f", "f32le", "-ac", &channels])?;
    let mut bytes = Vec::new();
    pipe.read_to_end(&mut bytes)?;
    child.wait()?;
    let theirs: Vec<f32> = bytes.chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).collect();
    let start = Instant::now();
    let ours = bink.decode_audio(track)?;
    let elapsed = start.elapsed();
    let n = ours.len().min(theirs.len());
    let (mut max_err, mut sum_sq, mut peak) = (0.0f32, 0.0f64, 0.0f32);
    for (&a, &b) in ours[..n].iter().zip(&theirs[..n]) {
        max_err = max_err.max((a - b).abs());
        sum_sq += ((a - b) as f64).powi(2);
        peak = peak.max(b.abs());
    }
    println!(
        "  audio track {track}: {} samples ours, {} ffmpeg; max error {:.3e}, rms error {:.3e}, peak {:.4}; decoded in {:.1} ms",
        ours.len(),
        theirs.len(),
        max_err,
        (sum_sq / n.max(1) as f64).sqrt(),
        peak,
        elapsed.as_secs_f64() * 1000.0
    );
    Ok(ours.len() == theirs.len() && max_err < 1e-4)
}

fn bench(bink: &mut Bink, frames: u32) -> Result<()> {
    // Decode the run twice and keep the faster, so the first pass warms caches and the page tables.
    let mut best = f64::MAX;
    let mut slowest = 0.0f64;
    for _ in 0..2 {
        bink.decode_frame(0)?;
        let start = Instant::now();
        let mut worst = 0.0f64;
        for i in 1..frames {
            let t = Instant::now();
            bink.decode_frame(i)?;
            worst = worst.max(t.elapsed().as_secs_f64());
        }
        let total = start.elapsed().as_secs_f64();
        if total < best {
            best = total;
            slowest = worst;
        }
    }
    let per_frame = best * 1000.0 / (frames - 1).max(1) as f64;
    let mut rgba = vec![0u8; bink.width() as usize * bink.height() as usize * 4];
    let start = Instant::now();
    for _ in 0..20 {
        bink.frame().to_rgba(&mut rgba);
    }
    let convert = start.elapsed().as_secs_f64() * 1000.0 / 20.0;
    println!("  speed: {per_frame:.3} ms per frame over {} frames (slowest {:.3} ms); to_rgba {convert:.3} ms", frames - 1, slowest * 1000.0);
    Ok(())
}
