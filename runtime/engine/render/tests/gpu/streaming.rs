//! Page streaming (R2b): only root pages are resident at upload; the visibility passes' requests
//! stream the rest in, LRU eviction makes room, and every frame on the way is the cut of what is
//! resident, matching the reference rasterizer drawing the same cut on the CPU.

use marlin_math::{Transform, Vec3};
use marlin_render::Environment;
use marlin_render::MeshHandle;

use crate::support::cook::{Cooked, sphere};
use crate::support::{Harness, POOL_PAGES, VisFrame};
use crate::visibility::{check_against_reference, settle};

/// (pages, root pages) of a cooked mesh.
fn page_counts(c: &Cooked) -> (usize, usize) {
    let dag = c.view().cluster_dag;
    (dag.pages.len(), dag.header.root_page_count as usize)
}

/// Triangles in a frame's cut.
fn triangles(h: &Harness, f: &VisFrame) -> u32 {
    f.cut
        .iter()
        .map(|c| u32::from(h.cluster(c[1]).triangle_count))
        .sum()
}

/// Every resident page of `mesh` has every page it depends on resident, and its root pages stay.
fn check_residency(h: &Harness, mesh: MeshHandle, cooked: &Cooked) {
    let dag = cooked.view().cluster_dag;
    for page in 0..dag.pages.len() {
        let resident = h.meshes.page_slot(mesh, page).is_some();
        if page < dag.header.root_page_count as usize {
            assert!(resident, "root page {page}");
        }
        if resident {
            for &d in dag.page_dependencies(page) {
                assert!(
                    h.meshes.page_slot(mesh, d as usize).is_some(),
                    "page {page} without its dependency {d}"
                );
            }
        }
    }
}

#[test]
fn released_meshes_give_their_pool_slots_back() {
    let mut h = Harness::new(Environment::default());
    let free = h.meshes.free_pages();
    assert_eq!(free, POOL_PAGES as usize - 1, "the cube's one root page");
    let ball = sphere(64, 128);
    let (pages, roots) = page_counts(&ball);
    assert!(pages > roots, "{pages} pages, {roots} roots");
    let (_, handle) = h.upload(&ball);
    assert_eq!(h.meshes.free_pages(), free - roots, "the roots, uploaded");
    assert!(h.meshes.prefetch(handle));
    assert_eq!(h.meshes.free_pages(), free - pages, "then every page");
    let mut slots: Vec<u32> = (0..pages)
        .map(|p| h.meshes.page_slot(handle, p).unwrap())
        .collect();
    slots.sort_unstable();
    slots.dedup();
    assert_eq!(slots.len(), pages, "a slot each");
    h.wait_idle();
    // SAFETY: the GPU is idle.
    unsafe { h.meshes.release_mesh(handle) };
    assert_eq!(h.meshes.free_pages(), free);
    assert!(h.meshes.resident_mesh(handle).is_none());
}

#[test]
fn a_mesh_whose_root_pages_do_not_fit_is_refused() {
    let mut h = Harness::new(Environment::default());
    let ball = sphere(64, 128);
    let (_, roots) = page_counts(&ball);
    let mut uploaded = 0;
    loop {
        match h
            .meshes
            .upload_mesh(h.device, std::sync::Arc::new(ball.clone()))
        {
            Ok(_) => uploaded += 1,
            Err(e) => {
                assert_eq!(e.kind(), marlin_rhi::ErrorKind::OutOfMemory, "{e}");
                break;
            }
        }
    }
    assert_eq!(uploaded, (POOL_PAGES as usize - 1) / roots);
    assert!(h.meshes.free_pages() < roots);
}

#[test]
fn requested_pages_stream_in_until_the_cut_is_complete() {
    let mut h = Harness::new(Environment::default());
    h.vis.lod_error_pixels = 0.0;
    let ball = sphere(64, 128);
    let (_, roots) = page_counts(&ball);
    let (id, handle) = h.upload(&ball);
    h.camera_at(0.0);
    h.add(
        id,
        Transform::from_translation(Vec3::new(0.0, 0.0, -1.2)),
        [1.0; 4],
    );

    // The first frame draws the roots and asks for finer pages.
    let first = h.visibility_frame();
    check_against_reference(&h, &first);
    assert!(first.requests > 0, "pages requested");
    // Its feedback loaded them, for the next frame to upload.
    assert!(h.meshes.resident_pages(handle) > roots);
    assert!(!h.meshes.streaming_idle());
    // Until it settles, every frame is the cut of what is resident.
    let last = settle(&mut h);
    assert_eq!(last.requests, 0);
    let streamed = h.meshes.resident_pages(handle);
    assert!(streamed > roots, "{streamed} pages resident");
    check_residency(&h, handle, &ball);
    let streamed_triangles = triangles(&h, &last);
    assert!(streamed_triangles > triangles(&h, &first) * 2);

    // Every page resident draws the same picture: streaming loaded all the cut needs.
    assert!(h.meshes.prefetch(handle));
    let full = h.visibility_frame();
    check_against_reference(&h, &full);
    assert_eq!(full.vis_depth, last.vis_depth);
    assert_eq!(triangles(&h, &full), streamed_triangles);
}

/// Frames until a close-up of a 65K-triangle ball has streamed every page in with at most
/// `budget` pages a frame (budget 0 wants the finest level, so every page), each frame checked.
fn frames_to_stream_in(budget: u32) -> usize {
    let mut h = Harness::new(Environment::default());
    h.vis.lod_error_pixels = 0.0;
    h.meshes.max_page_uploads = budget;
    let ball = sphere(128, 256);
    let (pages, _) = page_counts(&ball);
    let (id, handle) = h.upload(&ball);
    h.camera_at(0.0);
    h.add(
        id,
        Transform::from_translation(Vec3::new(0.0, 0.0, -1.5)),
        [1.0; 4],
    );
    for frame in 1..=200 {
        let f = h.visibility_frame();
        check_against_reference(&h, &f);
        check_residency(&h, handle, &ball);
        if h.meshes.streaming_idle() {
            assert_eq!(h.meshes.resident_pages(handle), pages);
            return frame;
        }
    }
    panic!("not streamed in after 200 frames");
}

#[test]
fn page_uploads_are_bounded_per_frame() {
    // A request loads with the pages it depends on, so one page a frame still takes a few
    // pages a frame; it takes more frames all the same.
    let bounded = frames_to_stream_in(1);
    let unbounded = frames_to_stream_in(64);
    assert!(bounded > unbounded, "{bounded} frames against {unbounded}");
}

#[test]
fn least_recently_used_pages_make_room() {
    let ball = sphere(64, 128);
    let (pages, roots) = page_counts(&ball);
    // Room for the cube, two balls' roots and one ball's other pages.
    let mut h = Harness::with_pool(
        Environment::default(),
        1 + 2 * roots as u32 + (pages - roots) as u32,
    );
    h.vis.lod_error_pixels = 0.0;
    let (a, ha) = h.upload(&ball);
    let (b, hb) = h.upload(&ball);
    h.add(
        a,
        Transform::from_translation(Vec3::new(0.0, 0.0, 0.0)),
        [1.0, 0.0, 0.0, 1.0],
    );
    h.add(
        b,
        Transform::from_translation(Vec3::new(100.0, 0.0, 0.0)),
        [0.0, 1.0, 0.0, 1.0],
    );

    // Close to A: all of A streams in.
    h.camera_at(1.5);
    settle(&mut h);
    assert_eq!(h.meshes.resident_pages(ha), pages);
    assert_eq!(h.meshes.resident_pages(hb), roots);
    assert_eq!(h.meshes.free_pages(), 0);

    // Close to B: A's pages, unused now, make room for B's.
    h.camera = Some((
        Transform::from_translation(Vec3::new(100.0, 0.0, 1.5)),
        marlin_render::Camera::default(),
    ));
    settle(&mut h);
    assert_eq!(h.meshes.resident_pages(hb), pages);
    assert_eq!(h.meshes.resident_pages(ha), roots, "A back to its roots");
    check_residency(&h, ha, &ball);
    check_residency(&h, hb, &ball);
}
