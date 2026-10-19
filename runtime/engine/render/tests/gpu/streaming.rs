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

/// A cooked mesh as a package loads it: the root pages in memory, every other page read when
/// requested and handed over a call to `take` later (like a read on its way), the reads counted.
/// `broken` pages fail to read.
struct FromDisk {
    cooked: Cooked,
    resident: usize,
    broken: Vec<usize>,
    /// Per blob: 0 not requested, 1 being read, 2 read.
    state: std::sync::Mutex<Vec<u8>>,
    reads: std::sync::atomic::AtomicUsize,
}

impl FromDisk {
    fn new(cooked: &Cooked, broken: Vec<usize>) -> std::sync::Arc<Self> {
        let resident = cooked.view().cluster_dag.header.root_page_count as usize;
        std::sync::Arc::new(Self {
            cooked: cooked.clone(),
            resident,
            broken,
            state: std::sync::Mutex::new(vec![0; cooked.pages.len()]),
            reads: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    fn reads(&self) -> usize {
        self.reads.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl marlin_render::MeshData for FromDisk {
    fn view(&self) -> marlin_render::asset::mesh::MeshView<'_> {
        self.cooked.view()
    }

    fn bulk(&self, index: usize) -> Option<&[u8]> {
        (index < self.resident).then(|| self.cooked.pages[index].as_slice())
    }

    fn request(&self, index: usize) {
        let mut state = self.state.lock().unwrap();
        assert!(index >= self.resident, "a resident page requested");
        if state[index] == 0 {
            state[index] = 1;
            self.reads
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    fn take(&self, index: usize) -> Option<Result<Vec<u8>, ()>> {
        let mut state = self.state.lock().unwrap();
        match state[index] {
            1 => {
                state[index] = 2;
                None
            }
            2 => {
                state[index] = 0;
                Some(if self.broken.contains(&index) {
                    Err(())
                } else {
                    Ok(self.cooked.pages[index].clone())
                })
            }
            _ => None,
        }
    }
}

#[test]
fn pages_not_in_memory_are_read_then_loaded() {
    let mut h = Harness::new(Environment::default());
    h.vis.lod_error_pixels = 0.0;
    let ball = sphere(128, 256);
    let (pages, roots) = page_counts(&ball);
    let disk = FromDisk::new(&ball, Vec::new());
    let (id, handle) = h.upload_from(&ball, disk.clone());
    h.camera_at(0.0);
    h.add(
        id,
        Transform::from_translation(Vec3::new(0.0, 0.0, -1.5)),
        [1.0; 4],
    );
    // Every frame on the way is the cut of what is resident; a page is read once.
    let last = settle(&mut h);
    assert_eq!(h.meshes.resident_pages(handle), pages);
    assert_eq!(disk.reads(), pages - roots);
    check_residency(&h, handle, &ball);

    // The same picture as a mesh with every page in memory.
    let mut all = Harness::new(Environment::default());
    all.vis.lod_error_pixels = 0.0;
    let (id, _) = all.upload(&ball);
    all.camera_at(0.0);
    all.add(
        id,
        Transform::from_translation(Vec3::new(0.0, 0.0, -1.5)),
        [1.0; 4],
    );
    let full = settle(&mut all);
    assert_eq!(full.vis_depth, last.vis_depth);
}

#[test]
fn a_page_that_fails_to_read_leaves_its_surface_coarser() {
    let mut h = Harness::new(Environment::default());
    h.vis.lod_error_pixels = 0.0;
    let ball = sphere(128, 256);
    let (pages, roots) = page_counts(&ball);
    // The last page: the finest level, nothing depends on it.
    let disk = FromDisk::new(&ball, vec![pages - 1]);
    let (id, handle) = h.upload_from(&ball, disk.clone());
    h.camera_at(0.0);
    h.add(
        id,
        Transform::from_translation(Vec3::new(0.0, 0.0, -1.5)),
        [1.0; 4],
    );
    settle(&mut h);
    let resident = h.meshes.resident_pages(handle);
    assert!(
        resident > roots && resident < pages,
        "{resident} of {pages}"
    );
    assert!(h.meshes.page_slot(handle, pages - 1).is_none());
    check_residency(&h, handle, &ball);
    // It is not read again.
    let reads = disk.reads();
    for _ in 0..4 {
        let f = h.visibility_frame();
        check_against_reference(&h, &f);
    }
    assert_eq!(disk.reads(), reads);
}
