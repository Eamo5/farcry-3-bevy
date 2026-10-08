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
