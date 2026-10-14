//! `fc3ui shot <movie.swf> <out.png> [frames] [width height]`: plays a
//! converted interface movie off screen for some frames and saves the last.
//! `fc3ui hud <movie> <out.png> [frames] [event channel arg... [+ channel
//! arg...]]`: loads a movie (archive or cache path) over the engine shim at
//! 1280x720, prints what it asks of the engine and its DPS ids, optionally
//! sends it events, and saves a frame.

use anyhow::{Context, Result};

fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).with_writer(std::io::stderr).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("shot") => {
            let swf = args.get(1).context("usage: shot <movie.swf> <out.png> [frames] [width height]")?;
            let out = args.get(2).context("usage: shot <movie.swf> <out.png> [frames] [width height]")?;
            let frames: u32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(1);
            let size = match (args.get(4).and_then(|s| s.parse().ok()), args.get(5).and_then(|s| s.parse().ok())) {
                (Some(w), Some(h)) => Some((w, h)),
                _ => None,
            };
            let movie = fc3_ui::OffscreenMovie::open(fc3_ui::offscreen_descriptors()?, std::path::Path::new(swf), size)?;
            for _ in 0..frames.max(1) {
                movie.run_frame();
            }
            movie.capture()?.save(out)?;
            println!("saved {out}");
        }
        Some("hud") => {
            let usage = "usage: hud <movie> <out.png> [frames] [event channel arg...]";
            let movie = args.get(1).context(usage)?;
            let out = args.get(2).context(usage)?;
            let frames: u32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(30);
            let size = (1280, 720);
            let renderer = fc3_ui::offscreen_renderer(fc3_ui::offscreen_descriptors()?, size)?;
            let mut host = fc3_ui::Host::new(renderer, &fc3_formats::cache::default_cache_root(), size)?;
            host.answer("IsHighDefinition", "1");
            host.answer("GetLanguage", "english");
            host.answer("Platform", "PC");
            host.run_frame();
            host.load(movie, 1);
            for _ in 0..frames.max(1) {
                host.run_frame();
            }
            for call in host.take_calls() {
                println!("engine call: {} {:?}", call.command, call.args);
            }
            println!("dps ids: {:?}", host.dps_ids());
            if args.get(4).map(String::as_str) == Some("event") {
                // (Several, separated by "+": `SetGameMode SPHud + EnableHud`.)
                for group in args[5..].split(|a| a == "+") {
                    let Some((channel, rest)) = group.split_first() else { continue };
                    let rest: Vec<&str> = rest.iter().map(String::as_str).collect();
                    host.event(channel, &rest);
                    for _ in 0..5 {
                        host.run_frame();
                    }
                }
                for _ in 0..frames.max(1) {
                    host.run_frame();
                }
                for call in host.take_calls() {
                    println!("engine call: {} {:?}", call.command, call.args);
                }
                println!("dps ids: {:?}", host.dps_ids());
            }
            // FC3UI_SET="id:visible=1,frame=12,text=99;id2:_alpha=50": clip
            // states to try (a frame or text, else any property).
            if let Ok(set) = std::env::var("FC3UI_SET") {
                for entry in set.split(';').filter(|e| !e.is_empty()) {
                    let Some((id, props)) = entry.split_once(':') else { continue };
                    for kv in props.split(',') {
                        let Some((k, v)) = kv.split_once('=') else { continue };
                        let num = v.parse::<f64>().ok();
                        let ok = match k {
                            "visible" => host.dps_visible(id, v == "1" || v == "true"),
                            "frame" => host.dps_frame(id, num.map_or(fc3_ui::ExternalValue::String(v.into()), fc3_ui::ExternalValue::Number)),
                            "play" => host.dps_play(id, num.map_or(fc3_ui::ExternalValue::String(v.into()), fc3_ui::ExternalValue::Number)),
                            "text" => host.dps_text(id, v),
                            _ => host.dps_set(id, k, num.map_or(fc3_ui::ExternalValue::String(v.into()), fc3_ui::ExternalValue::Number)),
                        };
                        if !ok {
                            println!("no clip {id}");
                        }
                    }
                }
                // (FC3UI_AFTER: frames to run on after, for what plays.)
                let after: u32 = std::env::var("FC3UI_AFTER").ok().and_then(|v| v.parse().ok()).unwrap_or(2);
                for _ in 0..after {
                    host.run_frame();
                }
            }
            if std::env::var_os("FC3UI_INFO").is_some() {
                for id in host.dps_ids() {
                    println!("{id}: {}", host.dps_info(&id));
                }
            }
            fc3_ui::capture(host.player())?.save(out)?;
            println!("saved {out}");
        }
        _ => println!("usage: fc3ui shot <movie.swf> <out.png> [frames] [width height] | hud <movie> <out.png> [frames] [event channel arg...]"),
    }
    Ok(())
}
