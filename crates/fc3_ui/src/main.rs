//! `fc3ui shot <movie.swf> <out.png> [frames] [width height]`: plays a
//! converted interface movie off screen for some frames and saves the last.
//! `fc3ui hud <movie> <out.png> [frames] [event channel arg... [+ channel
//! arg...]]`: loads a movie (archive or cache path) over the engine shim at
//! 1280x720, prints what it asks of the engine and its DPS ids, optionally
//! sends it events, and saves a frame.

use anyhow::{Context, Result};

/// One frame of the movies' clock (their tweens go by time).
const FRAME: f64 = 1.0 / 30.0;

/// One frame, in real time: the movies' tweens go by `getTimer`, the wall
/// clock.
fn step(host: &mut fc3_ui::Host) {
    std::thread::sleep(std::time::Duration::from_secs_f64(FRAME));
    host.run(FRAME);
}

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
            step(&mut host);
            // FC3UI_PRELOAD="UI\...\MainHud.feu@2;...": movies loaded first,
            // at their levels (the HUD's, whose fonts the others use).
            for entry in std::env::var("FC3UI_PRELOAD").unwrap_or_default().split(';').filter(|e| !e.is_empty()) {
                let (path, level) = entry.rsplit_once('@').map_or((entry, 2), |(p, l)| (p, l.parse().unwrap_or(2)));
                host.load(path, level);
                for _ in 0..5 {
                    step(&mut host);
                }
            }
            // (FC3UI_LEVEL: the movie's level, 1 unless said.)
            host.load(movie, std::env::var("FC3UI_LEVEL").ok().and_then(|l| l.parse().ok()).unwrap_or(1));
            for _ in 0..frames.max(1) {
                step(&mut host);
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
                        step(&mut host);
                    }
                }
                for _ in 0..frames.max(1) {
                    step(&mut host);
                }
                for call in host.take_calls() {
                    println!("engine call: {} {:?}", call.command, call.args);
                }
                println!("dps ids: {:?}", host.dps_ids());
            }
            // FC3UI_DATA="DataLootInventory=sName:Feather,iStateID:2;sName:Hide|Other=...":
            // lists for the data providers (items by ";", fields by ",",
            // numbers as numbers), set after the events, as the engine sets
            // them once a page says it is loaded (it builds its grid then).
            for list in std::env::var("FC3UI_DATA").unwrap_or_default().split('|').filter(|l| !l.is_empty()) {
                let Some((name, items)) = list.split_once('=') else { continue };
                let items = items
                    .split(';')
                    .map(|item| {
                        item.split(',')
                            .filter_map(|kv| kv.split_once(':'))
                            .map(|(k, v)| {
                                let value = v.parse::<f64>().map_or_else(|_| fc3_ui::ExternalValue::String(v.to_string()), fc3_ui::ExternalValue::Number);
                                (k.to_string(), value)
                            })
                            .collect()
                    })
                    .collect();
                host.set_data(name, items);
                for _ in 0..frames.max(1) {
                    step(&mut host);
                }
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
                    step(&mut host);
                }
            }
            // FC3UI_CALL="_level1.setTutorial|Title|Text|false;_level1.showTutorial":
            // the movie's functions to call (arguments by "|", "true" and
            // "false" as booleans, the rest as text).
            for call in std::env::var("FC3UI_CALL").unwrap_or_default().split(';').filter(|c| !c.is_empty()) {
                let mut parts = call.split('|');
                let Some(path) = parts.next() else { continue };
                let args = parts
                    .map(|a| match a {
                        "true" => fc3_ui::ExternalValue::Bool(true),
                        "false" => fc3_ui::ExternalValue::Bool(false),
                        _ => fc3_ui::text_value(a),
                    })
                    .collect();
                println!("{path} -> {:?}", host.call_function(path, args));
                for _ in 0..frames.max(1) {
                    step(&mut host);
                }
            }
            // FC3UI_GET="_level1._alpha;_level1.mcBack": what is at paths;
            // FC3UI_LIST="_level1": an object's members.
            for path in std::env::var("FC3UI_GET").unwrap_or_default().split(';').filter(|p| !p.is_empty()) {
                println!("{path} = {}", host.path_info(path));
            }
            for path in std::env::var("FC3UI_LIST").unwrap_or_default().split(';').filter(|p| !p.is_empty()) {
                println!("{path}: {}", host.path_members(path).join(" "));
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
