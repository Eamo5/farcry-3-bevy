# Vendored crates

`bevy_mesh` is Bevy's 0.19.1 mesh crate (MIT or Apache-2.0, licence files
alongside) with one change, marked `fc3 patch` in `src/mesh.rs`:
`Mesh::write_packed_vertex_buffer_data` interleaves the vertex attributes in
ordinary memory and copies the result into the GPU staging buffer in one go.
Upstream writes each attribute straight into the mapped staging memory, one
strided write per vertex, which on write-combined memory ran at about 1 ms per
megabyte and held the render thread up by 8-14 ms whenever a row of sectors
streamed in. The common attribute sizes (4, 8, 12 and 16 bytes) are copied as
fixed-size arrays, which inline, rather than with a `memcpy` call per vertex
and attribute.

`ruffle_render_wgpu`, `naga-agal` and `naga-pixelbender` are Ruffle's wgpu
renderer and its AGAL and Pixel Bender shader compilers (MIT or Apache-2.0,
`LICENSE.md` alongside), from Ruffle at
`2372a4e148a67d68efd796f4477b3cddd4f880cd`, the revision `fc3_ui` takes
`ruffle_core` from. Ruffle is on wgpu and naga 30; Bevy here is on 29, and
the interface movies draw on Bevy's device, so these are taken back to wgpu
and naga 29 (`wgpu-profiler` 0.27). Their manifests name versions instead of
Ruffle's workspace. `naga-agal` is patched in for Ruffle's own crates too
(`[patch."https://github.com/ruffle-rs/ruffle"]` in the workspace manifest),
since `ruffle_render` depends on it. `naga-agal`'s tests are left out.

The wgpu 29 changes in `ruffle_render_wgpu`: vertex buffer layouts are plain
(not `Option`) slices, mapped buffer ranges aren't fallible, surface
configurations have no colour space and adapters no limit buckets, a surface
texture presents itself, and Pixel Bender inputs wrap a texture in
`RawWgpuTexture` (in `src/lib.rs`), since `ruffle_render` implements its
`RawTexture` for the textures of Ruffle's wgpu, not this one. naga is built
with `wgsl-out`, as Ruffle's workspace does.

`ruffle/core` is `ruffle_core` itself (same revision and licence), patched
in for the git one so that images inline in text (`<img src="...">` naming
an exported symbol, as the game's strings use for their icons) are laid out
and drawn as Scaleform does: in the line, the symbol at its own size. The
changes are marked `fc3 patch` (`src/html/layout.rs`,
`src/html/layout/context.rs`, `src/display_object/edit_text.rs`,
`src/display_object/graphic.rs`, `src/display_object/movie_clip.rs`; see
`docs/ui.md`). It sits at `ruffle/core` so that its build script finds its
ActionScript sources where `build_playerglobal` looks, `../core/src` from
the crate. Its manifest names versions and Ruffle's other crates at the
revision instead of the workspace's; the lock file resolves the same.

`build_playerglobal` is Ruffle's build helper for `ruffle_core` (same
revision and licence), patched in the same way. Upstream, it compiles
Ruffle's ActionScript 3 globals (`core/src/avm2/globals/*.as`) with
`tools/asc/asc.jar`, which needs Java, then generates `ruffle_core`'s native
method table from the result. Here it reads that compiler output from
`vendor/build_playerglobal/playerglobal_avm2.abc` instead (`src/avm2.rs`,
marked `fc3 patch`), so building needs no Java. The file depends only on
Ruffle's sources at that revision, and has to be made again only when the
revision changes.

It is made without Java from a Ruffle release built from the same revision
(the nightly that follows the commit, when nothing else landed between):
every Ruffle binary embeds the compiler's output as its playerglobal SWF,
with the `[Ruffle(...)]` metadata stripped once the native table is made.
`fc3tool playerglobal-abc` takes that ABC out of the binary and puts the
metadata back from the AS3 sources in cargo's checkout of the revision
(each annotation marks the class or member declared next; it reports how
many of each kind it placed and fails on any it can't place):

    fc3tool playerglobal-abc <ruffle.exe from the nightly's zip> \
        ~/.cargo/git/checkouts/ruffle-<hash>/<rev> \
        vendor/build_playerglobal/playerglobal_avm2.abc

The 2026-10-13 nightly is the build of `2372a4e1`. With Java, the compiler
makes the same file; run from Ruffle's checkout at that revision:

    java -classpath tools/asc/asc.jar macromedia.asc.embedding.ScriptCompiler \
        -optimize -builtin -apiversioning -version 9 -outdir <dir> \
        -out playerglobal_avm2 core/src/avm2/globals/Toplevel.as \
        core/src/avm2/globals/globals.as

and copy `<dir>/playerglobal_avm2.abc` here. The `.cpp` and `.h` it writes
alongside aren't needed. The binary and `main.rs` of the upstream crate are
left out.
