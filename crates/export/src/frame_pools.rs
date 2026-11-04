//! Run the frames of a batch in parallel without letting them pile up on one stack.
//!
//! A frame's rendering is itself parallel (decoding, effects, colour conversion), and a Rayon worker
//! waiting for such inner work runs other queued work meanwhile. With frames as tasks of one pool that
//! can be another frame, which can wait in turn: nothing bounds how many frames end up on one worker's
//! stack but the batch, and a deep project overflows it (an abort that no `catch_unwind` can stop).
//!
//! So each frame in flight has a pool to itself. A waiting worker can only pick up work of its own
//! frame, and the threads are divided up front: `frames * threads_each` never exceeds the pool the
//! export runs in, which keeps `ExportSettings::max_threads` true.

use std::panic::resume_unwind;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use rayon::prelude::*;

/// Threads a frame in flight gets to itself, at least two frames at a time. Frames gain more from
/// their inner parallelism than from running side by side: on 32 threads, H.264 exports of footage
/// took less CPU and memory at 4 frames at a time than at 16, and 14% less time with a blur.
const THREADS_PER_FRAME: usize = 8;

/// The pools of one export, kept between batches: making them per batch would start threads per batch.
#[derive(Default)]
pub(crate) struct FramePools {
    pools: Mutex<Option<(usize, usize, Arc<Vec<rayon::ThreadPool>>)>>,
}

impl FramePools {
    /// At least `frames` pools of `threads_each` threads each; `None` if the system refuses the threads.
    fn get(&self, frames: usize, threads_each: usize) -> Option<Arc<Vec<rayon::ThreadPool>>> {
        let mut slot = self.pools.lock().unwrap_or_else(PoisonError::into_inner);
        // A shorter batch (the last one) uses the pools there are, rather than starting more threads.
        if let Some((f, t, pools)) = slot.as_ref()
            && *f >= frames
            && t.saturating_mul(*f) <= rayon::current_num_threads()
        {
            return Some(pools.clone());
        }
        let build = |k: usize| rayon::ThreadPoolBuilder::new().num_threads(threads_each).thread_name(move |i| format!("filmcraft-frame-{k}-{i}")).build().ok();
        let pools: Arc<Vec<_>> = Arc::new((0..frames).map(build).collect::<Option<Vec<_>>>()?);
        *slot = Some((frames, threads_each, pools.clone()));
        Some(pools)
    }

    /// `job(i)` for every `i` in `first..end`, in parallel on the threads of the pool this is called
    /// in, with the results in order. A panic in a job continues on this thread, as it does in Rayon.
    pub(crate) fn map<T: Send>(&self, first: i64, end: i64, job: impl Fn(i64) -> T + Sync) -> Vec<T> {
        let count = usize::try_from(end.saturating_sub(first)).unwrap_or(0);
        let threads = rayon::current_num_threads();
        // One frame can't nest, and the browser has no threads to start.
        if count <= 1 || threads <= 1 || cfg!(target_arch = "wasm32") {
            return (first..end).map(job).collect();
        }
        let frames = (threads / THREADS_PER_FRAME).max(2).min(count).min(threads);
        let Some(pools) = self.get(frames, threads / frames) else {
            return (first..end).into_par_iter().map(&job).collect();
        };
        let slots: Vec<Mutex<Option<T>>> = (0..count).map(|_| Mutex::new(None)).collect();
        let next = AtomicUsize::new(0);
        std::thread::scope(|s| {
            let workers: Vec<_> = pools
                .iter()
                .take(frames)
                .map(|pool| {
                    s.spawn(|| {
                        loop {
                            let i = next.fetch_add(1, Ordering::Relaxed);
                            let (Some(slot), Ok(step)) = (slots.get(i), i64::try_from(i)) else { return };
                            let value = pool.install(|| job(first.saturating_add(step)));
                            *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(value);
                        }
                    })
                })
                .collect();
            for worker in workers {
                if let Err(panic) = worker.join() {
                    resume_unwind(panic);
                }
            }
        });
        slots.into_iter().filter_map(|slot| slot.into_inner().unwrap_or_else(PoisonError::into_inner)).collect()
    }
}
