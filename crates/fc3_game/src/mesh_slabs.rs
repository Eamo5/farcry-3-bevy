//! Diagnostics for how Bevy packs meshes into GPU buffers ("slabs").
//!
//! Streaming a row of sectors in hands the render thread a few hundred meshes
//! in one frame; writing them out is what the vendored `bevy_mesh` patch
//! speeds up (see `vendor/README.md`), `upload_budget` spreads over a few
//! frames, and the terrain's finer levels of detail are built as the viewer
//! nears so they don't all come at once.
//!
//! FC3_MESH_DEBUG logs each slab's growth and each frame's burst of new mesh
//! data (with where it came from and how long the allocator took);
//! FC3_MESH_SLAB_MB=min,max sets the slab sizes (Bevy's: 1 and 512).

use std::collections::HashMap;

use bevy::prelude::*;
use bevy::render::mesh::allocator::{allocate_and_free_meshes, MeshAllocator, MeshAllocatorSettings};
use bevy::render::mesh::RenderMesh;
use bevy::render::render_asset::{prepare_assets, ExtractedAssets};
use bevy::render::slab_allocator::SlabAllocatorSettings;
use bevy::render::{Render, RenderApp, RenderSystems};

pub struct MeshSlabsPlugin;

impl Plugin for MeshSlabsPlugin {
    fn build(&self, app: &mut App) {
        let sizes = std::env::var("FC3_MESH_SLAB_MB").ok().and_then(|v| {
            let (a, b) = v.split_once(',')?;
            Some((a.trim().parse::<u64>().ok()?, b.trim().parse::<u64>().ok()?))
        });
        if let (Some((min, max)), Some(render)) = (sizes, app.get_sub_app_mut(RenderApp)) {
            let defaults = SlabAllocatorSettings::default();
            render.insert_resource(MeshAllocatorSettings {
                slab_allocator_settings: SlabAllocatorSettings {
                    min_slab_size: min << 20,
                    max_slab_size: max << 20,
                    large_threshold: (max << 20).min(defaults.large_threshold),
                    ..defaults
                },
                ..default()
            });
        }
        if std::env::var("FC3_MESH_DEBUG").is_ok() {
            app.add_systems(Last, log_mesh_sources);
            let Some(render) = app.get_sub_app_mut(RenderApp) else { return };
            render.init_resource::<Stamp>().add_systems(
                Render,
                (stamp.before(allocate_and_free_meshes), log_slabs.after(allocate_and_free_meshes).before(prepare_assets::<RenderMesh>))
                    .in_set(RenderSystems::PrepareAssets),
            );
        }
    }
}

/// When the allocator's turn came, this frame.
#[derive(Resource, Default)]
struct Stamp(Option<std::time::Instant>);

fn stamp(mut stamp: ResMut<Stamp>) {
    stamp.0 = Some(std::time::Instant::now());
}

fn log_slabs(allocator: Res<MeshAllocator>, extracted: Res<ExtractedAssets<RenderMesh>>, stamp: Res<Stamp>, mut sizes: Local<HashMap<String, u64>>) {
    let ms = stamp.0.map_or(0.0, |s| s.elapsed().as_secs_f64() * 1000.0);
    let mb = |b: u64| b as f64 / (1 << 20) as f64;
    for (id, slab) in &allocator.slabs {
        let size = slab.buffer_size();
        let before = sizes.insert(id.to_string(), size);
        if before.is_some_and(|b| b != size) {
            info!("mesh slab {id} grew {:.1} -> {:.1} MB", mb(before.unwrap_or(0)), mb(size));
        }
    }
    let bytes: u64 =
        extracted.extracted.iter().map(|(_, m)| m.get_vertex_buffer_size() as u64 + m.get_index_buffer_bytes().map_or(0, |b| b.len() as u64)).sum();
    if bytes > 4 << 20 || ms > 3.0 {
        let biggest = extracted.extracted.iter().map(|(_, m)| m.get_vertex_buffer_size()).max().unwrap_or(0);
        info!(
            "{:.1} ms: {} meshes extracted ({:.1} MB, largest {:.1} MB), {} modified, {} removed; {} slabs, {:.0} MB in all",
            ms,
            extracted.extracted.len(),
            mb(bytes),
            mb(biggest as u64),
            extracted.modified.len(),
            extracted.removed.len(),
            allocator.slab_count(),
            mb(allocator.slabs_size())
        );
    }
}

/// FC3_MESH_DEBUG: where each frame's burst of new meshes comes from.
fn log_mesh_sources(mut events: MessageReader<AssetEvent<Mesh>>, server: Res<AssetServer>) {
    let mut by: HashMap<String, u32> = HashMap::new();
    let mut n = 0;
    for ev in events.read() {
        if let AssetEvent::Added { id } = ev {
            n += 1;
            let src =
                server.get_path(*id).map(|p| p.path().parent().map(|d| d.display().to_string()).unwrap_or_default()).unwrap_or_else(|| "(made in code)".into());
            *by.entry(src).or_default() += 1;
        }
    }
    if n >= 50 {
        let mut v: Vec<_> = by.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1));
        info!("{n} meshes added: {:?}", &v[..v.len().min(6)]);
    }
}
