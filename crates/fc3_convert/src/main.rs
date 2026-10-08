//! `fc3convert`: converts an installed Far Cry 3 into the Bevy-ready cache.
//!
//! Usage: fc3convert [--data <data_win32>] [--cache <dir>] [--force]
//!                   [--min-free-gb N] [--sectors id,id,...]

use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};
use fc3_convert::{convert, error_count, Context_, Options, Progress, GB};

fn main() -> Result<()> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut data = None;
    let mut cache = fc3_formats::cache::default_cache_root();
    let mut force = false;
    let mut min_free = 5 * GB;
    let mut sectors = None;
    let mut i = 0;
    while i < argv.len() {
        let next = argv.get(i + 1).cloned().unwrap_or_default();
        match argv[i].as_str() {
            "--data" => {
                data = Some(PathBuf::from(next));
                i += 1;
            }
            "--cache" => {
                cache = PathBuf::from(next);
                i += 1;
            }
            "--force" => force = true,
            "--min-free-gb" => {
                min_free = (next.parse::<f64>().context("--min-free-gb")? * GB as f64) as u64;
                i += 1;
            }
            "--sectors" => {
                sectors = Some(
                    next.split(',')
                        .filter_map(|s| s.trim().parse().ok())
                        .collect(),
                );
                i += 1;
            }
            "-h" | "--help" => {
                println!("fc3convert [--data <data_win32>] [--cache <dir>] [--force] [--min-free-gb N] [--sectors id,...]");
                return Ok(());
            }
            other => eprintln!("ignoring unknown argument {other}"),
        }
        i += 1;
    }
    let data = data
        .or_else(fc3_formats::find_install)
        .context("Far Cry 3 data_win32 not found; pass --data or set FC3_DATA")?;
    let mut opts = Options::new(data, cache.clone());
    opts.force = force;
    opts.min_free_bytes = min_free;
    opts.only_sectors = sectors;
    let lists = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/filelists");
    opts.filelists = [PathBuf::from("data/filelists"), lists]
        .into_iter()
        .find(|p| p.exists());
    println!(
        "game data: {}\ncache:     {}",
        opts.data_dir.display(),
        cache.display()
    );

    let start = Instant::now();
    let ctx = Context_::default();
    let report = |p: Progress| {
        let pct = if p.total > 0 {
            p.done as f64 * 100.0 / p.total as f64
        } else {
            0.0
        };
        print!(
            "\r{:28} {:6}/{:<6} {:5.1}%  {:8.1} MB written  {:6.0}s   ",
            p.stage.label(),
            p.done,
            p.total,
            pct,
            p.bytes_written as f64 / 1e6,
            start.elapsed().as_secs_f64()
        );
        let _ = std::io::stdout().flush();
    };
    let manifest = convert(&opts, &ctx, &report)?;
    println!();
    println!(
        "done in {:.0}s: {} terrain sectors, {} object sectors, {} models, {} textures, {} errors{}",
        start.elapsed().as_secs_f64(),
        manifest.sectors_with_terrain.len(),
        manifest.sectors_with_objects.len(),
        manifest.models,
        manifest.textures,
        error_count(&ctx),
        if error_count(&ctx) > 0 { " (see convert_errors.log)" } else { "" }
    );
    Ok(())
}
