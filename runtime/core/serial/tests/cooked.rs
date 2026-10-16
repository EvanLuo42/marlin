use std::mem::offset_of;

use marlin_serial::cooked::{
    CookedBuffer, CookedBuilder, CookedView, LayoutError, Pod, RelPtr, RelSlice,
};

#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(C)]
struct Mesh {
    name: RelSlice<u8>,
    vertices: RelSlice<[f32; 3]>,
    material: RelPtr<Material>,
    lod_bias: f32,
}

// SAFETY: three 8/8/4-byte fields plus an f32, all align 4: no padding.
unsafe impl Pod for Mesh {}

#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(C, align(16))]
struct Material {
    color: [f32; 4],
    flags: u64,
    _pad: u64,
}

// SAFETY: 16 + 8 + 8 bytes with explicit padding, align 16.
unsafe impl Pod for Material {}

fn build() -> CookedBuffer {
    let mut b = CookedBuilder::new();
    let mesh = b.push(&Mesh {
        name: RelSlice::EMPTY,
        vertices: RelSlice::EMPTY,
        material: RelPtr::NULL,
        lod_bias: 0.25,
    });
    let name = b.push_str("crate");
    let vertices = b.push_slice(&[[0.0f32, 1.0, 2.0], [3.0, 4.0, 5.0]]);
    let material = b.push(&Material {
        color: [1.0, 0.5, 0.25, 1.0],
        flags: 7,
        _pad: 0,
    });
    assert_eq!(material.offset() % 16, 0);
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
    assert_eq!(b.align(), 16);
    b.finish()
}

#[test]
fn build_and_view() {
    let buffer = build();
    assert_eq!(buffer.align(), 16);
    let view = buffer.view();
    let mesh: &Mesh = view.root().unwrap();
    assert_eq!(mesh.lod_bias, 0.25);
    assert_eq!(view.str(&mesh.name), Ok("crate"));
    assert_eq!(
        view.slice(&mesh.vertices).unwrap(),
        &[[0.0, 1.0, 2.0], [3.0, 4.0, 5.0]]
    );
    let material = view.deref(&mesh.material).unwrap().unwrap();
    assert_eq!(material.flags, 7);
    assert_eq!(material.color[1], 0.5);
}

#[test]
fn relocatable() {
    // Offsets are self-relative: the same bytes work at any (suitably aligned) address.
    let buffer = build();
    let mut moved = CookedBuffer::zeroed(buffer.len() + 64, 64);
    moved.as_mut_slice()[64..].copy_from_slice(&buffer);
    let view = CookedView::new(&moved[64..]);
    let mesh: &Mesh = view.root().unwrap();
    assert_eq!(view.str(&mesh.name), Ok("crate"));
    assert_eq!(view.deref(&mesh.material).unwrap().unwrap().flags, 7);
}

#[test]
fn null_and_empty() {
    let mut b = CookedBuilder::new();
    b.push(&Mesh {
        name: RelSlice::EMPTY,
        vertices: RelSlice::EMPTY,
        material: RelPtr::NULL,
        lod_bias: 1.0,
    });
    let buffer = b.finish();
    let view = buffer.view();
    let mesh: &Mesh = view.root().unwrap();
    assert_eq!(view.deref(&mesh.material), Ok(None));
    assert_eq!(view.slice(&mesh.vertices), Ok(&[][..]));
    assert_eq!(view.str(&mesh.name), Ok(""));
}

fn patch(buffer: &CookedBuffer, at: usize, bytes: &[u8]) -> CookedBuffer {
    let mut copy = CookedBuffer::copy_from(buffer, buffer.align());
    copy.as_mut_slice()[at..at + bytes.len()].copy_from_slice(bytes);
    copy
}

#[test]
fn corrupt_offsets_are_errors() {
    let buffer = build();
    let name_at = offset_of!(Mesh, name);
    let material_at = offset_of!(Mesh, material);

    // Pointing past the end.
    let bad = patch(&buffer, name_at, &1_000_000i32.to_ne_bytes());
    let view = bad.view();
    assert_eq!(
        view.str(&view.root::<Mesh>().unwrap().name),
        Err(LayoutError::OutOfBounds)
    );

    // Pointing before the start.
    let bad = patch(&buffer, name_at, &(-8i32).to_ne_bytes());
    let view = bad.view();
    assert_eq!(
        view.slice(&view.root::<Mesh>().unwrap().name),
        Err(LayoutError::OutOfBounds)
    );

    // A length running past the end, including one whose byte size overflows.
    for len in [10_000u32, u32::MAX] {
        let bad = patch(&buffer, name_at + 4, &len.to_ne_bytes());
        let view = bad.view();
        let mesh = view.root::<Mesh>().unwrap();
        assert_eq!(view.slice(&mesh.name), Err(LayoutError::OutOfBounds));
        assert_eq!(view.slice(&mesh.vertices).map(<[_]>::len), Ok(2));
    }

    // A misaligned target.
    let off = buffer.view().root::<Mesh>().unwrap().material.offset();
    let bad = patch(&buffer, material_at, &(off - 4).to_ne_bytes());
    let view = bad.view();
    assert_eq!(
        view.deref(&view.root::<Mesh>().unwrap().material),
        Err(LayoutError::Misaligned)
    );

    // Invalid UTF-8.
    let bad = patch(&buffer, size_of::<Mesh>(), &[0xff]);
    let view = bad.view();
    assert_eq!(
        view.str(&view.root::<Mesh>().unwrap().name),
        Err(LayoutError::InvalidUtf8)
    );
}

#[test]
fn view_checks_its_own_bounds() {
    let buffer = build();
    // Truncated buffers.
    for len in 0..size_of::<Mesh>() {
        assert_eq!(
            CookedView::new(&buffer[..len]).root::<Mesh>(),
            Err(LayoutError::OutOfBounds)
        );
    }
    let short = CookedView::new(&buffer[..size_of::<Mesh>() + 2]);
    let mesh = short.root::<Mesh>().unwrap();
    assert_eq!(short.str(&mesh.name), Err(LayoutError::OutOfBounds));

    // A misaligned buffer start.
    assert_eq!(
        CookedView::new(&buffer[1..]).root::<u32>(),
        Err(LayoutError::Misaligned)
    );

    // Relative pointers from outside the view are refused.
    let elsewhere = RelSlice::<u8>::EMPTY;
    let outside = Mesh {
        name: elsewhere,
        vertices: RelSlice::EMPTY,
        material: RelPtr::NULL,
        lod_bias: 0.0,
    };
    let view = buffer.view();
    assert_eq!(view.deref(&outside.material), Ok(None));
    let mesh = view.root::<Mesh>().unwrap();
    let copied = *mesh;
    assert_eq!(view.str(&copied.name), Err(LayoutError::NotInView));
}

#[test]
fn byte_level_corruption_never_panics() {
    let buffer = build();
    for i in 0..buffer.len() {
        for flip in [0x01u8, 0x80, 0xff] {
            let mut bad = CookedBuffer::copy_from(&buffer, 16);
            bad.as_mut_slice()[i] ^= flip;
            let view = bad.view();
            let mesh = view.root::<Mesh>().unwrap();
            let _ = view.str(&mesh.name);
            let _ = view.slice(&mesh.vertices);
            let _ = view.deref(&mesh.material);
        }
    }
}

#[test]
fn builder_link_errors() {
    let mut other = CookedBuilder::new();
    other.push(&0u64);
    let foreign = other.push(&RelPtr::<u32>::NULL);
    let mut b = CookedBuilder::new();
    let target = b.push(&1u32);
    // A position from another, longer builder.
    assert_eq!(b.link(foreign, target), Err(LayoutError::OutOfBounds));
}
