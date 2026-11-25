//! Part of unit `gd-render-hull`. The meshing thread (`bp-mesh`): one `hull_mesh::pipeline::ChunkMesher`,
//! jobs in, finished pieces out. Outside the simulation's 3-core pool (L1945). It never touches Godot.
//!
//! The main thread decides order and drops superseded work before sending (`core`), so this side is a plain
//! first-in first-out worker: every job it receives is built. One [`MeshDone`] is sent per chunk of a
//! `MeshWork::Chunks` or `MeshWork::Volumes` job and one for a `Lod` or `Mask` job, so the main thread can
//! count what is still in flight.
use super::{MeshDone, MeshJob, MeshOutput, MeshWork};
use crate::seams::{Lod, MeshError, SurfaceBytes};
use hull_mesh::pipeline::ChunkMesher;
use std::sync::mpsc::{Receiver, Sender};
use std::time::Instant;

/// Build one job, calling `emit` once per finished piece (see the module docs for how many).
pub(crate) fn process(mesher: &mut ChunkMesher, job: &MeshJob, emit: &mut dyn FnMut(MeshDone)) {
    let input = job.cells.input();
    let mut send = |output: MeshOutput, started: Instant| emit(MeshDone { vessel: job.vessel, generation: job.generation, output, micros: started.elapsed().as_micros().min(u32::MAX as u128) as u32 });
    match &job.work {
        MeshWork::Chunks(coords) => for &coord in coords {
            let started = Instant::now();
            match mesher.mesh_chunk(&input, coord) {
                Ok(mesh) => send(MeshOutput::Chunk(Box::new(mesh)), started),
                Err(e) => send(MeshOutput::Failed(e), started),
            }
        },
        MeshWork::Volumes(coords) => for &coord in coords {
            let started = Instant::now();
            match hull_mesh::chunk::validate_input(&input) {
                Ok(()) => send(MeshOutput::Volume { coord, volume: Box::new(hull_mesh::chunk::build_volume(&input, coord)) }, started),
                Err(e) => send(MeshOutput::Failed(e), started),
            }
        },
        MeshWork::Lod(lod) => {
            let started = Instant::now();
            match hull_mesh::chunk::validate_input(&input) {
                // LOD0 is drawn from chunks: asking for it here is a caller error and yields nothing to draw.
                Ok(()) if *lod == Lod::L0 => send(MeshOutput::Lod { lod: *lod, surface: SurfaceBytes::default() }, started),
                Ok(()) => send(MeshOutput::Lod { lod: *lod, surface: hull_mesh::surface::encode_colored_surface(&hull_mesh::lod::mesh_lod(&input, *lod)) }, started),
                Err(e) => send(MeshOutput::Failed(e), started),
            }
        }
        MeshWork::Mask => {
            let started = Instant::now();
            send(MeshOutput::Mask(Box::new(hull_mesh::mask::build_hull_mask(&input))), started);
        }
    }
}

/// How many [`MeshDone`]s a job produces.
pub(crate) fn outputs(work: &MeshWork) -> usize {
    match work { MeshWork::Chunks(c) | MeshWork::Volumes(c) => c.len(), MeshWork::Lod(_) | MeshWork::Mask => 1 }
}

fn run(jobs: Receiver<MeshJob>, done: Sender<MeshDone>) {
    let mut mesher = ChunkMesher::new();
    while let Ok(job) = jobs.recv() {
        let mut sent = 0usize;
        let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| process(&mut mesher, &job, &mut |d| { sent += 1; let _ = done.send(d); })));
        if built.is_err() {
            // A panic inside the mesher must not leave the main thread waiting on pieces that never come:
            // answer what is still owed as failures and start from clean buffers.
            mesher = ChunkMesher::new();
            for _ in sent..outputs(&job.work) {
                let _ = done.send(MeshDone { vessel: job.vessel, generation: job.generation, output: MeshOutput::Failed(MeshError::ViewLength), micros: 0 });
            }
        }
    }
}

/// Start the thread. It ends when the job sender is dropped.
pub(crate) fn spawn() -> Option<(Sender<MeshJob>, Receiver<MeshDone>, std::thread::JoinHandle<()>)> {
    let (job_tx, job_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let handle = std::thread::Builder::new().name("bp-mesh".into()).spawn(move || run(job_rx, done_tx)).ok()?;
    Some((job_tx, done_rx, handle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::hull::design::tests::box_design;
    use crate::render::hull::HullCells;
    use content::cell::RenderChunkCoord;
    use std::sync::Arc;

    #[test]
    fn the_thread_builds_every_kind_of_work_and_stops_with_its_sender() {
        let d = box_design([40, 8, 12]);
        let cells = Arc::new(HullCells { size: d.grid.size, cells: d.grid.cells.clone(), palette: d.palette.clone(), design_waterline_y: 2, ..HullCells::default() });
        let (jobs, done, handle) = spawn().expect("the thread starts");
        let coords = vec![RenderChunkCoord { x: 0, y: 0, z: 0 }, RenderChunkCoord { x: 1, y: 0, z: 0 }, RenderChunkCoord { x: 9, y: 0, z: 0 }];
        let job = |generation: u32, work: MeshWork| MeshJob { vessel: sim_core::EntityId::from_u64(3), blueprint: d.hash, generation, priority: 1.0, cells: cells.clone(), work };
        for (g, work) in [MeshWork::Chunks(coords.clone()), MeshWork::Volumes(coords[..2].to_vec()), MeshWork::Lod(Lod::L1), MeshWork::Lod(Lod::L0), MeshWork::Mask].into_iter().enumerate() {
            jobs.send(job(g as u32, work)).unwrap();
        }
        let mut got = Vec::new();
        while got.len() < 8 { got.push(done.recv_timeout(std::time::Duration::from_secs(20)).expect("a piece per output")); }
        // In job order: three chunks (the third is outside the grid), two volumes, two LODs, one mask.
        assert!(matches!(&got[0].output, MeshOutput::Chunk(m) if !m.empty && m.triangles > 0 && m.coord == coords[0]));
        assert!(matches!(&got[1].output, MeshOutput::Chunk(m) if !m.empty && !m.shadow.is_empty()));
        assert!(matches!(&got[2].output, MeshOutput::Failed(MeshError::ChunkOutOfRange(_))));
        assert!(matches!(&got[3].output, MeshOutput::Volume { coord, volume } if *coord == coords[0] && volume.bytes.len() == crate::seams::VolumeTexture::BYTES));
        assert!(matches!(&got[5].output, MeshOutput::Lod { lod: Lod::L1, surface } if !surface.is_empty() && surface.format == hull_mesh::format::COLORED_SURFACE));
        assert!(matches!(&got[6].output, MeshOutput::Lod { lod: Lod::L0, surface } if surface.is_empty()));
        assert!(matches!(&got[7].output, MeshOutput::Mask(m) if m.texels.len() == hull_mesh::mask::MASK_W * hull_mesh::mask::MASK_H));
        assert_eq!(got.iter().map(|g| g.generation).collect::<Vec<_>>(), vec![0, 0, 0, 1, 1, 2, 3, 4]);
        // A grid whose slices disagree is refused, not built and not a panic.
        let bad = Arc::new(HullCells { size: [4, 4, 4], cells: Vec::new(), ..HullCells::default() });
        jobs.send(MeshJob { vessel: sim_core::EntityId::NONE, blueprint: d.hash, generation: 9, priority: 0.0, cells: bad, work: MeshWork::Lod(Lod::L2) }).unwrap();
        assert!(matches!(done.recv_timeout(std::time::Duration::from_secs(20)).unwrap().output, MeshOutput::Failed(_)));
        drop(jobs);
        handle.join().expect("the thread ends when its sender is dropped");
        assert_eq!(outputs(&MeshWork::Chunks(coords)), 3);
    }
}
