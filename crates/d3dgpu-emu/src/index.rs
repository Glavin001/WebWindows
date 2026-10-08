//! Primitive and index-buffer rewrites.
//!
//! WebGPU draws point, line and triangle lists and line and triangle strips. Direct3D 9 adds triangle fans and the
//! `D3DFILL_WIREFRAME` and `D3DFILL_POINT` fill modes, which WebGPU has no rasterizer state for; all of them become
//! index lists here. Strips sometimes need rewriting too: WebGPU always treats `0xffff` / `0xffffffff` in an
//! indexed strip as a primitive restart, which Direct3D 9 has no notion of (see [`has_restart`]).
//!
//! Every function emits complete primitives only. An indexed draw whose index slice is shorter than `prim_count`
//! needs is cut to what the slice holds: Direct3D would read past the buffer and WebGPU would reject the draw.
//!
//! Rewritten triangles keep Direct3D's winding *and* its flat-shading vertex. WebGPU's flat interpolation takes
//! the first vertex of each list triangle. Direct3D takes vertex `i` of strip triangle `i` and vertex `i + 1` of
//! fan triangle `i` (the first-vertex convention of GL and Vulkan, which wined3d and DXVK rely on for
//! `D3DSHADE_FLAT`). So the list form of strip triangle `i` is `(i, i+1, i+2)` for even `i` and `(i, i+2, i+1)` for
//! odd `i`, and of fan triangle `i` is `(i+1, i+2, 0)`: rotations of Direct3D's `(i+1, i, i+2)` and
//! `(0, i+1, i+2)`, so the winding is unchanged.

use d3dgpu_proto::d3d9::PrimitiveType;
use std::collections::HashSet;

/// An index buffer element: `u16` (`D3DFMT_INDEX16`) or `u32` (`D3DFMT_INDEX32`).
pub trait Index: Copy + Eq + core::fmt::Debug {
    /// The value WebGPU reads as a strip restart for this index width.
    const RESTART: Self;
    fn to_u32(self) -> u32;
    /// Truncates for `u16`; callers only feed back values that came from the same buffer.
    fn from_u32(v: u32) -> Self;
}

impl Index for u16 {
    const RESTART: u16 = u16::MAX;
    fn to_u32(self) -> u32 {
        self as u32
    }
    fn from_u32(v: u32) -> u16 {
        v as u16
    }
}

impl Index for u32 {
    const RESTART: u32 = u32::MAX;
    fn to_u32(self) -> u32 {
        self
    }
    fn from_u32(v: u32) -> u32 {
        v
    }
}

/// The list topology `prim` is drawn as after [`to_list`]: points stay points, lines become a line list, triangles
/// a triangle list. `None` for values that are not a `D3DPRIMITIVETYPE`.
pub fn list_topology(prim: PrimitiveType) -> Option<PrimitiveType> {
    Some(match prim {
        PrimitiveType::PointList => PrimitiveType::PointList,
        PrimitiveType::LineList | PrimitiveType::LineStrip => PrimitiveType::LineList,
        PrimitiveType::TriangleList | PrimitiveType::TriangleStrip | PrimitiveType::TriangleFan => {
            PrimitiveType::TriangleList
        }
        _ => return None,
    })
}

fn is_triangles(prim: PrimitiveType) -> bool {
    matches!(prim, PrimitiveType::TriangleList | PrimitiveType::TriangleStrip | PrimitiveType::TriangleFan)
}

/// How many of `prim_count` primitives fit in `available` vertices (or indices).
fn fitting(prim: PrimitiveType, prim_count: u32, available: usize) -> u32 {
    let n = available as u64;
    let max = match prim {
        PrimitiveType::PointList => n,
        PrimitiveType::LineList => n / 2,
        PrimitiveType::LineStrip => n.saturating_sub(1),
        PrimitiveType::TriangleList => n / 3,
        PrimitiveType::TriangleStrip | PrimitiveType::TriangleFan => n.saturating_sub(2),
        _ => 0,
    };
    (prim_count as u64).min(max) as u32
}

/// Stream positions of primitive `i`'s vertices in list order (see the module docs for strips and fans), and how
/// many of the three slots are used.
fn prim_vertices(prim: PrimitiveType, i: u32) -> ([u32; 3], usize) {
    match prim {
        PrimitiveType::PointList => ([i, 0, 0], 1),
        PrimitiveType::LineList => ([2 * i, 2 * i + 1, 0], 2),
        PrimitiveType::LineStrip => ([i, i + 1, 0], 2),
        PrimitiveType::TriangleList => ([3 * i, 3 * i + 1, 3 * i + 2], 3),
        PrimitiveType::TriangleStrip if i.is_multiple_of(2) => ([i, i + 1, i + 2], 3),
        PrimitiveType::TriangleStrip => ([i, i + 2, i + 1], 3),
        PrimitiveType::TriangleFan => ([i + 1, i + 2, 0], 3),
        _ => ([0; 3], 0),
    }
}

/// Visits the triangles of `prim_count` triangle primitives, as stream positions.
fn triangles(prim: PrimitiveType, prim_count: u32) -> impl Iterator<Item = [u32; 3]> {
    (0..prim_count).map(move |i| prim_vertices(prim, i).0)
}

fn expand<T: Copy>(prim: PrimitiveType, count: u32, get: impl Fn(u32) -> T) -> Vec<T> {
    let mut out = Vec::with_capacity(count as usize * prim_vertices(prim, 0).1);
    for i in 0..count {
        let (v, n) = prim_vertices(prim, i);
        out.extend(v[..n].iter().map(|&p| get(p)));
    }
    out
}

/// Vertex numbers of a non-indexed draw of any primitive type, in the [`list_topology`] of `prim`.
pub fn to_list(prim: PrimitiveType, first: u32, prim_count: u32) -> Vec<u32> {
    let count = fitting(prim, prim_count, usize::MAX);
    expand(prim, count, |p| first.wrapping_add(p))
}

/// An index buffer of any primitive type, rewritten as the [`list_topology`] of `prim`.
pub fn to_list_indexed<I: Index>(prim: PrimitiveType, indices: &[I], prim_count: u32) -> Vec<I> {
    let count = fitting(prim, prim_count, indices.len());
    expand(prim, count, |p| indices[p as usize])
}

/// A non-indexed triangle fan as triangle-list vertex numbers.
pub fn fan_to_list(first: u32, prim_count: u32) -> Vec<u32> {
    to_list(PrimitiveType::TriangleFan, first, prim_count)
}

/// An indexed triangle fan as a triangle-list index buffer.
pub fn fan_to_list_indexed<I: Index>(indices: &[I], prim_count: u32) -> Vec<I> {
    to_list_indexed(PrimitiveType::TriangleFan, indices, prim_count)
}

/// A non-indexed triangle strip as triangle-list vertex numbers.
pub fn strip_to_list(first: u32, prim_count: u32) -> Vec<u32> {
    to_list(PrimitiveType::TriangleStrip, first, prim_count)
}

/// An indexed triangle strip as a triangle-list index buffer. Used when the indices contain WebGPU's restart value
/// ([`has_restart`]) and widening is not an option.
pub fn strip_to_list_indexed<I: Index>(indices: &[I], prim_count: u32) -> Vec<I> {
    to_list_indexed(PrimitiveType::TriangleStrip, indices, prim_count)
}

fn edges<T: Copy>(t: [T; 3], out: &mut Vec<T>) {
    out.extend_from_slice(&[t[0], t[1], t[1], t[2], t[2], t[0]]);
}

fn degenerate<T: Eq>(t: &[T; 3]) -> bool {
    t[0] == t[1] || t[1] == t[2] || t[0] == t[2]
}

/// `D3DFILL_WIREFRAME` for a non-indexed draw: each triangle's three edges as a line list.
///
/// Points and lines are not affected by the fill mode; for them this returns [`to_list`] (a point list or line
/// list). Shared edges are emitted once per triangle. Culling is not applied: Direct3D culls back faces before
/// filling, so the core must cull triangles itself (or accept extra lines) when `D3DRS_CULLMODE` is not
/// `D3DCULL_NONE`.
pub fn triangles_to_lines(prim: PrimitiveType, first: u32, prim_count: u32) -> Vec<u32> {
    if !is_triangles(prim) {
        return to_list(prim, first, prim_count);
    }
    let mut out = Vec::with_capacity(prim_count as usize * 6);
    for t in triangles(prim, prim_count) {
        edges(t.map(|p| first.wrapping_add(p)), &mut out);
    }
    out
}

/// `D3DFILL_WIREFRAME` for an indexed draw; see [`triangles_to_lines`].
///
/// Degenerate triangles (two equal indices) are skipped: they have no area, so Direct3D draws nothing for them,
/// and the zero-area triangles that stitch strips together would otherwise show as lines jumping between strips.
pub fn triangles_to_lines_indexed<I: Index>(prim: PrimitiveType, indices: &[I], prim_count: u32) -> Vec<I> {
    if !is_triangles(prim) {
        return to_list_indexed(prim, indices, prim_count);
    }
    let count = fitting(prim, prim_count, indices.len());
    let mut out = Vec::with_capacity(count as usize * 6);
    for t in triangles(prim, count) {
        let t = t.map(|p| indices[p as usize]);
        if !degenerate(&t) {
            edges(t, &mut out);
        }
    }
    out
}

/// `D3DFILL_POINT` for a non-indexed draw: every vertex the triangles use, once, as a point list. That is just the
/// vertex range of the draw. Points and lines are unaffected by the fill mode and come back as [`to_list`].
pub fn triangles_to_points(prim: PrimitiveType, first: u32, prim_count: u32) -> Vec<u32> {
    if !is_triangles(prim) {
        return to_list(prim, first, prim_count);
    }
    let n = match prim {
        PrimitiveType::TriangleList => prim_count as u64 * 3,
        _ if prim_count == 0 => 0,
        _ => prim_count as u64 + 2,
    };
    (0..n).map(|i| first.wrapping_add(i as u32)).collect()
}

/// `D3DFILL_POINT` for an indexed draw: the indices of non-degenerate triangles, each value once, in order of first
/// use. Deduplicated so a vertex shared by six triangles is drawn (and blended) once, as if Direct3D drew each
/// vertex of the mesh. Points and lines come back as [`to_list_indexed`].
pub fn triangles_to_points_indexed<I: Index>(prim: PrimitiveType, indices: &[I], prim_count: u32) -> Vec<I> {
    if !is_triangles(prim) {
        return to_list_indexed(prim, indices, prim_count);
    }
    let count = fitting(prim, prim_count, indices.len());
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for t in triangles(prim, count) {
        let t = t.map(|p| indices[p as usize]);
        if degenerate(&t) {
            continue;
        }
        for i in t {
            if seen.insert(i.to_u32()) {
                out.push(i);
            }
        }
    }
    out
}

/// `D3DFMT_INDEX16` data as 32-bit indices. Widening also removes the `0xffff` restart hazard of indexed strips.
pub fn widen_u16_to_u32(indices: &[u16]) -> Vec<u32> {
    indices.iter().map(|&i| i as u32).collect()
}

/// The smallest and largest index, for sizing vertex uploads. `None` when `indices` is empty.
pub fn index_range<I: Index>(indices: &[I]) -> Option<(u32, u32)> {
    let mut it = indices.iter().map(|i| i.to_u32());
    let first = it.next()?;
    Some(it.fold((first, first), |(lo, hi), i| (lo.min(i), hi.max(i))))
}

/// Whether an indexed strip would hit WebGPU's always-on primitive restart (`0xffff` for 16-bit indices,
/// `0xffffffff` for 32-bit). Direct3D 9 draws those as ordinary vertices, so such a strip must be widened
/// ([`widen_u16_to_u32`]) or rewritten as a list ([`strip_to_list_indexed`], [`to_list_indexed`]) first.
/// List topologies are not affected.
pub fn has_restart<I: Index>(indices: &[I]) -> bool {
    indices.contains(&I::RESTART)
}

/// Indices with `base_vertex` (`DrawIndexedPrimitive`'s `BaseVertexIndex`) added, for when the core cannot pass
/// it to the draw.
///
/// The addition wraps in 32 bits, as a GPU computing `index + baseVertex` does, rather than clamping: a negative
/// result becomes a huge index that robust buffer access turns into a zero fetch instead of silently drawing
/// vertex 0. A result of `0xffffffff` is a restart in strip topologies, so draw the output as a list.
pub fn rebase<I: Index>(indices: &[I], base_vertex: i32) -> Vec<u32> {
    indices.iter().map(|i| i.to_u32().wrapping_add(base_vertex as u32)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether two triangles are the same up to rotation (same winding).
    fn same_winding(a: [u32; 3], b: [u32; 3]) -> bool {
        (0..3).any(|r| [b[r], b[(r + 1) % 3], b[(r + 2) % 3]] == a)
    }

    fn tris(v: &[u32]) -> Vec<[u32; 3]> {
        v.chunks(3).map(|c| [c[0], c[1], c[2]]).collect()
    }

    #[test]
    fn fan_keeps_winding_and_provoking_vertex() {
        let l = fan_to_list(10, 3);
        assert_eq!(l, [11, 12, 10, 12, 13, 10, 13, 14, 10]);
        for (i, t) in tris(&l).into_iter().enumerate() {
            let i = i as u32;
            assert!(same_winding(t, [10, 10 + i + 1, 10 + i + 2]));
            assert_eq!(t[0], 10 + i + 1, "flat shading uses vertex i+1 of a fan triangle");
        }
        assert!(fan_to_list(0, 0).is_empty());
    }

    #[test]
    fn fan_indexed() {
        let idx: [u16; 5] = [7, 3, 4, 5, 6];
        assert_eq!(fan_to_list_indexed(&idx, 3), [3u16, 4, 7, 4, 5, 7, 5, 6, 7]);
        // Truncated to the triangles the slice holds.
        assert_eq!(fan_to_list_indexed(&idx, 10).len(), 9);
        assert!(fan_to_list_indexed::<u32>(&[1, 2], 1).is_empty());
    }

    #[test]
    fn strip_alternates_winding() {
        let l = strip_to_list(0, 4);
        let t = tris(&l);
        assert_eq!(t, [[0, 1, 2], [1, 3, 2], [2, 3, 4], [3, 5, 4]]);
        for (i, t) in t.into_iter().enumerate() {
            let i = i as u32;
            let d3d = if i.is_multiple_of(2) { [i, i + 1, i + 2] } else { [i + 1, i, i + 2] };
            assert!(same_winding(t, d3d));
            assert_eq!(t[0], i, "flat shading uses vertex i of a strip triangle");
        }
        let idx: [u32; 5] = [9, 8, 7, 6, 5];
        assert_eq!(strip_to_list_indexed(&idx, 3), [9, 8, 7, 8, 6, 7, 7, 6, 5]);
    }

    #[test]
    fn to_list_for_every_type() {
        use PrimitiveType as P;
        assert_eq!(to_list(P::PointList, 5, 3), [5, 6, 7]);
        assert_eq!(to_list(P::LineList, 5, 2), [5, 6, 7, 8]);
        assert_eq!(to_list(P::LineStrip, 5, 3), [5, 6, 6, 7, 7, 8]);
        assert_eq!(to_list(P::TriangleList, 1, 2), [1, 2, 3, 4, 5, 6]);
        assert!(to_list(PrimitiveType(99), 0, 3).is_empty());
        assert_eq!(list_topology(P::LineStrip), Some(P::LineList));
        assert_eq!(list_topology(P::TriangleFan), Some(P::TriangleList));
        assert_eq!(list_topology(PrimitiveType(0)), None);
        let idx: [u16; 4] = [4, 3, 2, 1];
        assert_eq!(to_list_indexed(P::LineStrip, &idx, 3), [4u16, 3, 3, 2, 2, 1]);
        assert_eq!(to_list_indexed(P::LineList, &idx, 5), [4u16, 3, 2, 1]);
    }

    #[test]
    fn wireframe() {
        use PrimitiveType as P;
        assert_eq!(triangles_to_lines(P::TriangleList, 3, 1), [3, 4, 4, 5, 5, 3]);
        assert_eq!(triangles_to_lines(P::TriangleStrip, 0, 2), [0, 1, 1, 2, 2, 0, 1, 3, 3, 2, 2, 1]);
        assert_eq!(triangles_to_lines(P::TriangleFan, 0, 2), [1, 2, 2, 0, 0, 1, 2, 3, 3, 0, 0, 2]);
        // Lines and points pass through in list form.
        assert_eq!(triangles_to_lines(P::LineStrip, 0, 2), [0, 1, 1, 2]);
        assert_eq!(triangles_to_lines(P::PointList, 2, 2), [2, 3]);

        let idx: [u16; 6] = [0, 1, 2, 2, 3, 4];
        assert_eq!(triangles_to_lines_indexed(P::TriangleList, &idx, 2), [0u16, 1, 1, 2, 2, 0, 2, 3, 3, 4, 4, 2]);
        // A stitched strip: 0 1 2 | 2 5 | 5 6 7. Only the real triangles produce lines.
        let strip: [u32; 8] = [0, 1, 2, 2, 5, 5, 6, 7];
        let lines = triangles_to_lines_indexed(P::TriangleStrip, &strip, 6);
        assert_eq!(lines, [0, 1, 1, 2, 2, 0, 5, 7, 7, 6, 6, 5]);
        assert_eq!(triangles_to_lines_indexed(P::LineList, &idx, 2), [0u16, 1, 2, 2]);
    }

    #[test]
    fn point_fill() {
        use PrimitiveType as P;
        assert_eq!(triangles_to_points(P::TriangleList, 4, 2), [4, 5, 6, 7, 8, 9]);
        assert_eq!(triangles_to_points(P::TriangleStrip, 4, 2), [4, 5, 6, 7]);
        assert_eq!(triangles_to_points(P::TriangleFan, 0, 3), [0, 1, 2, 3, 4]);
        assert!(triangles_to_points(P::TriangleFan, 0, 0).is_empty());
        assert_eq!(triangles_to_points(P::LineList, 0, 1), [0, 1]);

        let idx: [u16; 9] = [5, 1, 2, 2, 1, 7, 3, 3, 9];
        // The third triangle is degenerate; repeated vertices appear once, in order of first use.
        assert_eq!(triangles_to_points_indexed(P::TriangleList, &idx, 3), [5u16, 1, 2, 7]);
        let fan: [u32; 4] = [0, 1, 2, 3];
        assert_eq!(triangles_to_points_indexed(P::TriangleFan, &fan, 2), [1, 2, 0, 3]);
    }

    #[test]
    fn helpers() {
        assert_eq!(widen_u16_to_u32(&[1, 0xffff]), [1, 0xffff]);
        assert_eq!(index_range::<u16>(&[]), None);
        assert_eq!(index_range(&[5u16, 2, 9, 3]), Some((2, 9)));
        assert_eq!(index_range(&[7u32]), Some((7, 7)));
        assert!(has_restart(&[1u16, 0xffff]));
        assert!(!has_restart(&[1u32, 0xffff]));
        assert!(has_restart(&[u32::MAX]));
        assert_eq!(rebase(&[0u16, 1, 2], 100), [100, 101, 102]);
        assert_eq!(rebase(&[10u32, 20], -5), [5, 15]);
        assert_eq!(rebase(&[1u16], -2), [u32::MAX]); // wraps, does not clamp
        assert_eq!(u16::from_u32(0x1_0005), 5);
    }
}
