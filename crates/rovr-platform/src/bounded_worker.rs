//! Bounded platform observation worker (blocker 2).
//!
//! AX and SkyLight calls can hang indefinitely (app dying mid-call, Dock
//! restart). Spawning a thread per snapshot and abandoning it on timeout leaks
//! one thread per hang — repeated reconciliation creates unbounded hung
//! workers. This module provides a persistent dedicated worker thread whose
//! owner chooses the isolation scope (global snapshot or one application):
//!
//! - at most ONE job is in flight per worker;
//! - a wedged worker causes fast-fail callers (bounded wait, no new threads,
//!   nothing queued behind the hang);
//! - the wedge is detectable (`wedged_since`); recovery occurs only after the
//!   timed-out closure actually returns. No recovery closure is queued behind
//!   a permanent hang. Exactly ONE thread stays wedged — bounded and visible.

use std::cell::Cell;
use std::sync::mpsc::{
    channel, sync_channel, Receiver, RecvTimeoutError, SyncSender, TryRecvError,
};
use std::time::{Duration, Instant};

/// How long a caller waits for the worker before declaring a timeout.
pub const DEFAULT_JOB_TIMEOUT: Duration = Duration::from_secs(2);
/// Legacy constructor tuning retained for API stability; recovery now waits
/// for observed completion rather than queueing timed retries.
pub const DEFAULT_RETRY_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoundedError {
    /// The job did not complete within the caller's timeout. The worker may
    /// still be running it; nothing was spawned.
    Timeout,
    /// A previous job is believed wedged and the retry interval has not yet
    /// elapsed — failed fast without touching the worker.
    FastFail { since: Instant },
    /// The worker thread is gone (should never happen; it has no exit path).
    Dead,
}

impl std::fmt::Display for BoundedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BoundedError::Timeout => write!(f, "platform worker timed out"),
            BoundedError::FastFail { since } => {
                write!(f, "platform worker wedged (since {:?})", since.elapsed())
            }
            BoundedError::Dead => write!(f, "platform worker dead"),
        }
    }
}

struct Job<R> {
    epoch: u64,
    job: Box<dyn FnOnce() -> R + Send>,
}

pub struct BoundedWorker<R: Send + 'static> {
    tx: SyncSender<Job<R>>,
    rx: Receiver<(u64, R)>,
    /// True while a submitted job may still be executing on the worker.
    in_flight: Cell<bool>,
    epoch: Cell<u64>,
    wedged_at: Cell<Option<Instant>>,
    job_timeout: Duration,
}

impl<R: Send + 'static> BoundedWorker<R> {
    /// Spawn the single worker thread. Exactly one thread is created for the
    /// worker's lifetime; no other code path creates threads.
    pub fn new(job_timeout: Duration, _retry_interval: Duration) -> Self {
        // Capacity one plus the `in_flight` guard means the worker can hold
        // exactly one running/queued closure. A timed-out closure is never
        // followed by recovery closures queued behind it.
        let (tx, job_rx) = sync_channel::<Job<R>>(1);
        let (result_tx, rx) = channel::<(u64, R)>();
        std::thread::Builder::new()
            .name("rovr-platform-worker".into())
            .spawn(move || {
                while let Ok(next) = job_rx.recv() {
                    let Job { epoch, job } = next;
                    let result = job();
                    let _ = result_tx.send((epoch, result));
                }
                // job_rx disconnected: worker exits. No jobs are dropped.
            })
            .expect("spawn rovr-platform-worker");
        Self {
            tx,
            rx,
            in_flight: Cell::new(false),
            epoch: Cell::new(0),
            wedged_at: Cell::new(None),
            job_timeout,
        }
    }

    /// When the worker wedged, if it is currently believed wedged.
    pub fn wedged_since(&self) -> Option<Instant> {
        self.wedged_at.get()
    }

    /// Submit a job without waiting for it. A worker accepts no more than one
    /// job at a time; a late result is reaped before a new epoch is issued.
    pub fn submit(&self, job: impl FnOnce() -> R + Send + 'static) -> Result<u64, BoundedError> {
        self.reap_finished()?;
        if self.in_flight.get() {
            let since = self.wedged_at.get().unwrap_or_else(Instant::now);
            return Err(BoundedError::FastFail { since });
        }

        let epoch = self.epoch.get().wrapping_add(1);
        self.epoch.set(epoch);
        self.tx
            .try_send(Job {
                epoch,
                job: Box::new(job),
            })
            .map_err(|_| BoundedError::Dead)?;
        self.in_flight.set(true);
        Ok(epoch)
    }

    /// Poll for the result of `epoch` without blocking. Results from older
    /// epochs are discarded and can never be mistaken for the requested job.
    pub fn poll(&self, epoch: u64) -> Result<Option<R>, BoundedError> {
        loop {
            match self.rx.try_recv() {
                Ok((result_epoch, result)) => {
                    self.in_flight.set(false);
                    self.wedged_at.set(None);
                    if result_epoch == epoch {
                        return Ok(Some(result));
                    }
                }
                Err(TryRecvError::Empty) => return Ok(None),
                Err(TryRecvError::Disconnected) => return Err(BoundedError::Dead),
            }
        }
    }

    /// Wait for `epoch` against one absolute deadline.
    pub fn wait_until(&self, epoch: u64, deadline: Instant) -> Result<R, BoundedError> {
        loop {
            if let Some(result) = self.poll(epoch)? {
                return Ok(result);
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                self.mark_wedged();
                return Err(BoundedError::Timeout);
            };
            match self.rx.recv_timeout(remaining) {
                Ok((result_epoch, result)) => {
                    self.in_flight.set(false);
                    self.wedged_at.set(None);
                    if result_epoch == epoch {
                        return Ok(result);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    self.mark_wedged();
                    return Err(BoundedError::Timeout);
                }
                Err(RecvTimeoutError::Disconnected) => return Err(BoundedError::Dead),
            }
        }
    }

    pub fn is_idle(&self) -> bool {
        if self.in_flight.get() {
            let _ = self.reap_finished();
        }
        !self.in_flight.get()
    }

    fn reap_finished(&self) -> Result<(), BoundedError> {
        if !self.in_flight.get() {
            return Ok(());
        }
        match self.rx.try_recv() {
            Ok(_) => {
                self.in_flight.set(false);
                self.wedged_at.set(None);
                Ok(())
            }
            Err(TryRecvError::Empty) => Ok(()),
            Err(TryRecvError::Disconnected) => Err(BoundedError::Dead),
        }
    }

    fn mark_wedged(&self) {
        if self.wedged_at.get().is_none() {
            self.wedged_at.set(Some(Instant::now()));
        }
    }

    /// Run `job` on the lone worker with a bounded wait.
    pub fn run(&self, job: impl FnOnce() -> R + Send + 'static) -> Result<R, BoundedError> {
        let epoch = self.submit(job)?;
        self.wait_until(epoch, Instant::now() + self.job_timeout)
    }
}

/// Instrumented wrapper used by tests and by the platform to track execution
/// concurrency of the jobs themselves.
#[cfg(test)]
pub(crate) mod test_support {
    /// Runs `f` while tracking concurrent executions on the shared counters.
    pub fn tracked_job(
        active: &std::sync::atomic::AtomicUsize,
        max_seen: &std::sync::atomic::AtomicUsize,
        f: impl FnOnce(),
    ) {
        let now = active.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        max_seen.fetch_max(now, std::sync::atomic::Ordering::SeqCst);
        f();
        active.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    type Shared = (Arc<AtomicUsize>, Arc<AtomicUsize>);

    fn counters() -> Shared {
        (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)))
    }

    /// Blocker 2 regression: repeated timeouts must NOT create unbounded
    /// concurrent snapshot workers. Thirty consecutive timeouts against a slow
    /// job leave exactly ONE worker executing at most one job at a time.
    #[test]
    fn blocker2_repeated_timeouts_cannot_spawn_unbounded_workers() {
        let (active, max_seen) = counters();
        let worker: BoundedWorker<()> = BoundedWorker::new(
            Duration::from_millis(20), // short caller timeout
            Duration::from_millis(0),  // always allow a retry attempt
        );

        for i in 0..30 {
            let active = active.clone();
            let max_seen = max_seen.clone();
            let result = worker.run(move || {
                crate::bounded_worker::test_support::tracked_job(&active, &max_seen, || {
                    std::thread::sleep(Duration::from_millis(60));
                });
            });
            assert!(
                matches!(
                    result,
                    Err(BoundedError::Timeout) | Err(BoundedError::FastFail { .. })
                ),
                "iteration {i}: expected bounded failure, got {result:?}"
            );
        }

        assert_eq!(
            max_seen.load(Ordering::SeqCst),
            1,
            "observed execution concurrency must be 1 despite 30 timeouts"
        );
        assert!(
            worker.wedged_since().is_some(),
            "wedged state must be detectable"
        );
    }

    /// Blocker 2: while wedged, callers FAIL FAST without submitting work;
    /// after the timed-out job finishes, recovery against the same worker succeeds.
    #[test]
    fn blocker2_wedge_is_detectable_and_recovery_is_explicit() {
        let (active, max_seen) = counters();
        let worker: BoundedWorker<u32> = BoundedWorker::new(
            Duration::from_millis(30),
            Duration::from_millis(120), // retry window for the test
        );

        // Wedge the worker with a slow job.
        let active1 = active.clone();
        let max1 = max_seen.clone();
        let err = worker
            .run(move || {
                crate::bounded_worker::test_support::tracked_job(&active1, &max1, || {
                    std::thread::sleep(Duration::from_millis(200));
                });
                1
            })
            .unwrap_err();
        assert_eq!(err, BoundedError::Timeout);

        // Immediate follow-up fails fast WITHOUT executing another job.
        let active2 = active.clone();
        let max2 = max_seen.clone();
        let err = worker.run(move || {
            crate::bounded_worker::test_support::tracked_job(&active2, &max2, || {});
            2
        });
        assert!(
            matches!(err, Err(BoundedError::FastFail { .. })),
            "must fail fast while wedged, got {err:?}"
        );

        // After the timed-out job completes, the same worker serves a new job.
        std::thread::sleep(Duration::from_millis(250));
        let result = worker.run(|| 3).expect("recovery must succeed");
        assert_eq!(result, 3);
        assert!(worker.wedged_since().is_none(), "wedge cleared on success");
        assert_eq!(max_seen.load(Ordering::SeqCst), 1);
    }

    /// Blocker 2: healthy operation is unaffected — results come back.
    #[test]
    fn blocker2_healthy_path_returns_results() {
        let worker: BoundedWorker<u64> =
            BoundedWorker::new(Duration::from_secs(1), Duration::from_millis(10));
        for i in 0..5u64 {
            assert_eq!(worker.run(move || i * 2).unwrap(), i * 2);
        }
        assert!(worker.wedged_since().is_none());
    }

    #[test]
    fn all_waits_share_the_callers_absolute_deadline() {
        let worker = BoundedWorker::new(Duration::from_secs(1), Duration::ZERO);
        let epoch = worker
            .submit(|| {
                std::thread::sleep(Duration::from_millis(80));
                1
            })
            .unwrap();
        let started = Instant::now();
        let deadline = started + Duration::from_millis(20);
        assert_eq!(
            worker.wait_until(epoch, deadline),
            Err(BoundedError::Timeout)
        );
        assert!(started.elapsed() < Duration::from_millis(70));
    }

    #[test]
    fn submit_is_single_flight_and_late_result_is_reaped() {
        let worker = BoundedWorker::new(Duration::from_millis(10), Duration::ZERO);
        let epoch = worker
            .submit(|| {
                std::thread::sleep(Duration::from_millis(40));
                1
            })
            .unwrap();
        assert!(matches!(
            worker.submit(|| 2),
            Err(BoundedError::FastFail { .. })
        ));
        assert_eq!(
            worker.wait_until(epoch, Instant::now() + Duration::from_millis(5)),
            Err(BoundedError::Timeout)
        );
        std::thread::sleep(Duration::from_millis(50));
        let next = worker.submit(|| 3).unwrap();
        assert_eq!(
            worker.wait_until(next, Instant::now() + Duration::from_secs(1)),
            Ok(3)
        );
    }

    #[test]
    fn slow_worker_does_not_hide_healthy_result_at_shared_deadline() {
        let slow = BoundedWorker::new(Duration::from_secs(1), Duration::ZERO);
        let healthy = BoundedWorker::new(Duration::from_secs(1), Duration::ZERO);
        let slow_epoch = slow
            .submit(|| {
                std::thread::sleep(Duration::from_millis(80));
                1
            })
            .unwrap();
        let healthy_epoch = healthy.submit(|| 2).unwrap();
        let deadline = Instant::now() + Duration::from_millis(20);
        assert_eq!(
            slow.wait_until(slow_epoch, deadline),
            Err(BoundedError::Timeout)
        );
        assert_eq!(healthy.wait_until(healthy_epoch, deadline), Ok(2));
    }
}
