//! `#[derive(Pod)]`, layout hashes and the cooked header.

use std::mem::offset_of;

use marlin_serial::cooked::{
    COOKED_MAGIC, COOKED_VERSION, CookedBuffer, CookedBuilder, CookedHeader, CookedView,
    LayoutError, Pod, PodKind, RelPtr, RelSlice, layout_hash, pod_layout,
};

#[derive(Pod, Clone, Copy, Debug, PartialEq)]
#[repr(C)]
struct Mesh {
    name: RelSlice<u8>,
    vertices: RelSlice<[f32; 3]>,
    material: RelPtr<Material>,
    lod_bias: f32,
}

#[derive(Pod, Clone, Copy, Debug, PartialEq)]
#[repr(C, align(16))]
struct Material {
    color: [f32; 4],
    flags: u64,
    _pad: u64,
}

/// `Mesh` with a field renamed: same bytes, different layout hash.
#[derive(Pod, Clone, Copy)]
#[repr(C)]
struct MeshRenamed {
    label: RelSlice<u8>,
    vertices: RelSlice<[f32; 3]>,
    material: RelPtr<Material>,
    lod_bias: f32,
}

/// A recursive layout.
#[derive(Pod, Clone, Copy)]
#[repr(C)]
struct Node {
    value: u32,
    children: RelSlice<Node>,
}

#[derive(Pod, Clone, Copy)]
#[repr(transparent)]
struct Meters(f32);

#[derive(Pod, Clone, Copy)]
#[repr(C)]
struct Pair<T> {
    a: T,
    b: T,
}

/// A hand-written impl: described by size and alignment only.
#[derive(Clone, Copy)]
#[repr(C)]
struct Manual(u32);

// SAFETY: one `u32`.
unsafe impl Pod for Manual {}

#[test]
fn derived_layouts() {
    let layout = pod_layout::<Mesh>();
    assert_eq!((layout.size, layout.align), (size_of::<Mesh>(), 4));
    let PodKind::Struct(fields) = layout.kind else {
        panic!("not a struct")
    };
    let names: Vec<_> = fields.iter().map(|f| f.name).collect();
    assert_eq!(names, ["name", "vertices", "material", "lod_bias"]);
    assert_eq!(fields[2].offset, offset_of!(Mesh, material));
    assert!(matches!((fields[2].ty)().kind, PodKind::RelPtr(_)));
    assert!(matches!(pod_layout::<Meters>().kind, PodKind::Struct(f) if f[0].name == "0"));
    assert!(matches!(pod_layout::<Manual>().kind, PodKind::Opaque));
    assert_eq!(pod_layout::<Pair<u16>>().size, 4);
}

#[test]
fn layout_hashes() {
    // Deterministic, and finite for recursive types.
    assert_eq!(layout_hash::<Mesh>(), layout_hash::<Mesh>());
    let node = layout_hash::<Node>();
    assert_eq!(node, layout_hash::<Node>());
    // Same bytes, renamed field: different hash.
    assert_eq!(size_of::<Mesh>(), size_of::<MeshRenamed>());
    assert_ne!(layout_hash::<Mesh>(), layout_hash::<MeshRenamed>());
    // Generic instantiations differ by their field types.
    assert_ne!(layout_hash::<Pair<u32>>(), layout_hash::<Pair<f32>>());
    assert_ne!(layout_hash::<Pair<u32>>(), layout_hash::<Pair<i32>>());
    // A pointee's layout is part of the hash.
    assert_ne!(layout_hash::<RelPtr<u32>>(), layout_hash::<RelPtr<f32>>());
    assert_ne!(layout_hash::<[u8; 4]>(), layout_hash::<u32>());
    // Locked down: the hash must not change between builds or platforms. If this fails after a
    // deliberate change to the encoding, every cooked buffer with a header must be recooked.
    assert_eq!(layout_hash::<[u32; 2]>(), 0xff6b_f9e2_18fe_18ff);
}

fn build() -> CookedBuffer {
    let (mut b, mesh) = CookedBuilder::with_header(&Mesh {
        name: RelSlice::EMPTY,
        vertices: RelSlice::EMPTY,
        material: RelPtr::NULL,
        lod_bias: 0.5,
    });
    let name = b.push_str("crate");
    let vertices = b.push_slice(&[[1.0f32, 2.0, 3.0]]);
    let material = b.push(&Material {
        color: [1.0; 4],
        flags: 3,
        _pad: 0,
    });
    b.link_slice(mesh.field(offset_of!(Mesh, name), |m| &m.name), name)
        .unwrap();
    b.link_slice(
        mesh.field(offset_of!(Mesh, vertices), |m| &m.vertices),
        vertices,
    )
    .unwrap();
    b.link(
        mesh.field(offset_of!(Mesh, material), |m| &m.material),
        material,
    )
    .unwrap();
    b.finish()
}

#[test]
fn header_round_trip() {
    let buffer = build();
    let view = CookedView::with_header::<Mesh>(&buffer).unwrap();
    let header = view.header().unwrap();
    assert_eq!(header.magic, COOKED_MAGIC);
    assert_eq!(header.version, COOKED_VERSION);
    assert_eq!(header.len as usize, buffer.len());
    assert_eq!(header.align, 16);
    assert_eq!(header.layout_hash, layout_hash::<Mesh>());
    assert_eq!(header.root as usize, size_of::<CookedHeader>());
    let mesh: &Mesh = view.root().unwrap();
    assert_eq!(mesh.lod_bias, 0.5);
    assert_eq!(view.str(&mesh.name), Ok("crate"));
    assert_eq!(view.deref(&mesh.material).unwrap().unwrap().flags, 3);
    // Headerless views have no header.
    assert!(CookedView::new(&buffer).header().is_none());
}

fn patch(buffer: &CookedBuffer, at: usize, bytes: &[u8]) -> CookedBuffer {
    let mut copy = CookedBuffer::copy_from(buffer, buffer.align());
    copy.as_mut_slice()[at..at + bytes.len()].copy_from_slice(bytes);
    copy
}

#[test]
fn header_mismatches_are_clear_errors() {
    let buffer = build();
    let check = |b: &[u8]| CookedView::with_header::<Mesh>(b).map(drop);

    // The wrong root type.
    let err = CookedView::with_header::<MeshRenamed>(&buffer).unwrap_err();
    assert_eq!(
        err,
        LayoutError::LayoutMismatch {
            expected: layout_hash::<MeshRenamed>(),
            found: layout_hash::<Mesh>(),
        }
    );
    assert!(err.to_string().contains("recook"));

    assert_eq!(
        check(&patch(&buffer, 0, b"XXXX")),
        Err(LayoutError::BadMagic)
    );
    assert_eq!(check(&[]), Err(LayoutError::BadMagic));
    let version_at = offset_of!(CookedHeader, version);
    assert_eq!(
        check(&patch(&buffer, version_at, &9u16.to_ne_bytes())),
        Err(LayoutError::UnsupportedVersion(9))
    );
    let flags_at = offset_of!(CookedHeader, flags);
    assert_eq!(
        check(&patch(&buffer, flags_at, &1u16.to_ne_bytes())),
        Err(LayoutError::UnsupportedVersion(COOKED_VERSION))
    );
    // Truncated, or with bytes after it.
    assert_eq!(
        check(&buffer[..buffer.len() - 1]),
        Err(LayoutError::LengthMismatch {
            header: buffer.len() as u32,
            actual: buffer.len() - 1
        })
    );
    // Loaded at an address not aligned for its contents.
    let mut moved = CookedBuffer::zeroed(buffer.len() + 8, 16);
    moved.as_mut_slice()[8..].copy_from_slice(&buffer);
    assert_eq!(check(&moved[8..]), Err(LayoutError::Misaligned));
    // A root offset out of bounds.
    let root_at = offset_of!(CookedHeader, root);
    assert_eq!(
        check(&patch(&buffer, root_at, &10_000u32.to_ne_bytes())),
        Err(LayoutError::OutOfBounds)
    );
    // An alignment that is not a power of two.
    let align_at = offset_of!(CookedHeader, align);
    assert_eq!(
        check(&patch(&buffer, align_at, &3u32.to_ne_bytes())),
        Err(LayoutError::Misaligned)
    );
}

#[test]
fn header_corruption_never_panics() {
    let buffer = build();
    for i in 0..size_of::<CookedHeader>() {
        for flip in [0x01u8, 0x80, 0xff] {
            let mut bad = CookedBuffer::copy_from(&buffer, 16);
            bad.as_mut_slice()[i] ^= flip;
            if let Ok(view) = CookedView::with_header::<Mesh>(&bad) {
                let mesh = view.root::<Mesh>().unwrap();
                let _ = view.str(&mesh.name);
            }
        }
    }
}
