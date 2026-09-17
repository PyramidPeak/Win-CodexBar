//! Dashboard snapshot coordinator: TTL cache + single-flight builds.
//!
//! Upstream 0.48.0 F9/#2717 parity: slow snapshot builds are NEVER discarded —
//! a build that outlives any one request still completes, its result is cached,
//! and every waiter (current or arriving mid-build) receives that same result.
//! There is no 504-style "build took too long" path at all: the only failure
//! surfaced is a build that genuinely errored, and errors are never cached —
//! but every waiter that joined the failing generation receives that same
//! stored error (the builder still reports it once), while only the NEXT
//! caller retries with a fresh build.
//!
//! Prometheus scrapes use stale-while-refresh: they return the last successful
//! snapshot immediately and only trigger an expired/missing build in the
//! background. The dashboard's [`SnapshotCoordinator::get`] contract remains
//! wait-for-fresh, with both callers sharing the same build generation.

use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::{Duration, Instant};

use tokio::sync::futures::OwnedNotified;

use crate::cli::serve::metrics::MetricsSnapshot;

use super::snapshot::SnapshotPayload;
use super::source::{BoxSnapshotArtifactsFuture, BoxSnapshotFuture, SnapshotArtifacts};

mod flight;
mod state;

use flight::{BuildGuard, Flight, new_flight};
use state::{CachedSnapshot, CoordinatorState};

/// Pluggable snapshot collector (production: provider+cost scan; tests: stub).
pub type SnapshotBuildFn = Arc<dyn Fn() -> BoxSnapshotFuture + Send + Sync>;
pub(crate) type SnapshotArtifactsBuildFn =
    Arc<dyn Fn() -> BoxSnapshotArtifactsFuture + Send + Sync>;

impl std::fmt::Debug for SnapshotCoordinator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SnapshotCoordinator")
            .field("ttl", &self.ttl)
            .finish_non_exhaustive()
    }
}

/// Cheaply cloneable handle (all coordination state is shared through `Arc`).
#[derive(Clone)]
pub struct SnapshotCoordinator {
    ttl: Duration,
    build: SnapshotArtifactsBuildFn,
    state: Arc<StdMutex<CoordinatorState>>,
}

impl SnapshotCoordinator {
    pub fn new(ttl: Duration, build: SnapshotBuildFn) -> Self {
        let build_artifacts: SnapshotArtifactsBuildFn = Arc::new(move || {
            let future = build();
            Box::pin(async move { future.await.map(SnapshotArtifacts::dashboard_only) })
        });
        Self::new_with_artifacts(ttl, build_artifacts)
    }

    pub(crate) fn new_with_artifacts(ttl: Duration, build: SnapshotArtifactsBuildFn) -> Self {
        Self {
            ttl,
            build,
            state: Arc::new(StdMutex::new(CoordinatorState::default())),
        }
    }

    /// Return the last successful snapshot immediately and ensure an expired
    /// or missing cache is refreshed in the background. Concurrent callers
    /// share one refresh. When no Tokio runtime is active, this remains a pure
    /// cache lookup rather than claiming a flight that cannot be driven.
    fn latest_or_trigger_refresh(&self) -> Option<Arc<SnapshotPayload>> {
        self.latest_cached_or_trigger_refresh()
            .map(|cached| cached.payload)
    }

    pub(crate) fn latest_metrics_or_trigger_refresh(&self) -> Option<Arc<MetricsSnapshot>> {
        self.latest_cached_or_trigger_refresh()
            .and_then(|cached| cached.metrics)
    }

    fn latest_cached_or_trigger_refresh(&self) -> Option<CachedSnapshot> {
        let runtime = tokio::runtime::Handle::try_current().ok();
        let mut claimed = None;
        let cached = {
            let mut state = self.state.lock().expect("coordinator poisoned");
            let cached = state.cached.clone();
            let fresh = state
                .cached
                .as_ref()
                .is_some_and(|cached| cached.built_at.elapsed() < self.ttl);

            if !fresh && state.flight.is_none() && runtime.is_some() {
                let flight = new_flight();
                state.flight = Some(flight.clone());
                claimed = Some(flight);
            }
            cached
        };

        if let (Some(runtime), Some(flight)) = (runtime, claimed) {
            // The guard is constructed before the future is spawned. If the
            // runtime shuts down before its first poll, dropping that unpolled
            // future still clears the claimed flight and wakes waiters.
            drop(runtime.spawn(self.claimed_build_task(flight)));
        }
        cached
    }

    /// Get a snapshot: serve the fresh cached build when younger than `ttl`,
    /// join the in-flight build generation when one is running (receiving THAT
    /// generation's outcome — shared success or the same error), or start a
    /// new build generation otherwise.
    pub async fn get(&self) -> Result<Arc<SnapshotPayload>, String> {
        loop {
            // Decide under the lock; the guard is always dropped before awaits.
            //
            // Waiter lost-wakeup contract: the waiter creates AND enables its
            // `OwnedNotified` while holding this same decision guard — the
            // guard that observes an in-flight generation. For the shared
            // cache/generation state the builder holds this same
            // mutex for every update (success, error, or guard-driven reset);
            // the per-flight outcome is stored under the flight's own mutex
            // strictly BEFORE `notify_waiters`, which is what any woken waiter
            // synchronizes with. In both cases `notify_waiters` comes last, so by the time the decision
            // guard drops the waiter's future is already on the notify wait
            // list and no `notify_waiters` for this build can have fired in
            // between. The registered future is then carried out past the
            // guard drop and awaited unlocked (`OwnedNotified` owns the
            // `Arc<Notify>`, so no borrow of the guard or state contents
            // escapes the critical section).
            enum Decision {
                Serve(Arc<SnapshotPayload>),
                Wait(Arc<Flight>, Pin<Box<OwnedNotified>>),
                Build(Arc<Flight>),
            }
            let decision = {
                let mut state = self.state.lock().expect("coordinator poisoned");
                if let Some(cached) = state
                    .cached
                    .as_ref()
                    .filter(|cached| cached.built_at.elapsed() < self.ttl)
                {
                    Decision::Serve(cached.payload.clone())
                } else if let Some(flight) = &state.flight {
                    // Register AND enable the waiter on THIS flight's Notify
                    // before releasing the guard that observed it —
                    // closes the `notify_waiters` lost-wakeup window: a build
                    // completing in the instant after our decision cannot
                    // fire before this future is on the wait list.
                    let mut notified = Box::pin(flight.notify.clone().notified_owned());
                    notified.as_mut().enable();
                    Decision::Wait(flight.clone(), notified)
                } else {
                    let flight = new_flight();
                    state.flight = Some(flight.clone());
                    Decision::Build(flight)
                }
            };
            match decision {
                Decision::Serve(payload) => return Ok(payload),
                Decision::Wait(flight, notified) => {
                    // Already registered+enabled under the guard that observed
                    // the flight; await after unlock — no lock is held
                    // across this await.
                    notified.await;
                    // The builder stores the flight outcome BEFORE notifying,
                    // so `Some` is the resolution of the exact generation we
                    // joined: every waiter of it fans out to the same success
                    // (cheaply cloned `Arc`) or the same error. `None` means
                    // the builder was cancelled/panicked mid-flight and its
                    // guard already cleared the abandoned flight — re-scan
                    // and retry; no result is ever fabricated here.
                    let outcome = flight.outcome.lock().expect("coordinator poisoned").clone();
                    match outcome {
                        Some(outcome) => return outcome,
                        None => continue,
                    }
                }
                Decision::Build(flight) => {
                    let guard = BuildGuard::new(self.state.clone(), flight.clone());
                    return self.run_claimed_build(flight, guard).await;
                }
            }
        }
    }

    /// Build a `'static` task after synchronously constructing its completion
    /// guard. This ordering makes dropping an unpolled task safe.
    fn claimed_build_task(
        &self,
        flight: Arc<Flight>,
    ) -> impl std::future::Future<Output = Result<Arc<SnapshotPayload>, String>> + Send + 'static
    {
        let guard = BuildGuard::new(self.state.clone(), flight.clone());
        let coordinator = self.clone();
        async move { coordinator.run_claimed_build(flight, guard).await }
    }

    /// Drive a flight already installed in `state`. Foreground dashboard builds
    /// and detached metrics refreshes finish through this exact path.
    async fn run_claimed_build(
        &self,
        flight: Arc<Flight>,
        mut guard: BuildGuard,
    ) -> Result<Arc<SnapshotPayload>, String> {
        let built = (self.build)().await;
        let outcome = built.map(|artifacts| {
            (
                Arc::new(artifacts.dashboard),
                artifacts.metrics.map(Arc::new),
            )
        });
        let waiter_outcome = outcome
            .as_ref()
            .map(|(payload, _)| payload.clone())
            .map_err(Clone::clone);

        // Store this generation's outcome before publishing state and waking
        // waiters, so every registered waiter receives the exact same result.
        *flight.outcome.lock().expect("coordinator poisoned") = Some(waiter_outcome.clone());

        let mut state = self.state.lock().expect("coordinator poisoned");
        if state
            .flight
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &flight))
        {
            if let Ok((payload, metrics)) = &outcome {
                state.cached = Some(CachedSnapshot {
                    payload: payload.clone(),
                    metrics: metrics.clone(),
                    built_at: Instant::now(),
                });
            }
            // Failed refreshes leave the last successful cache intact. Errors
            // themselves remain uncached, so the next caller may retry.
            state.flight = None;
        }
        drop(state);
        guard.disarm();
        flight.notify.notify_waiters();
        waiter_outcome
    }
}

#[cfg(test)]
mod tests;
