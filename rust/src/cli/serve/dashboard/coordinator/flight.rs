//! Single-flight completion and cancellation guard.

use std::sync::Arc;
use std::sync::Mutex as StdMutex;

use tokio::sync::Notify;

use super::super::snapshot::SnapshotPayload;
use super::state::CoordinatorState;

/// A single build generation: everything a waiter needs to receive THAT
/// generation's exact outcome. Owning the notification channel and the stored
/// outcome together (instead of re-deriving the result from global state
/// after a wake) is what lets all of one generation's waiters fan out to the
/// same error, while a cancelled/dropped build (outcome `None` forever)
/// cleanly routes waiters into a retry loop.

#[derive(Debug)]
pub(in crate::cli::serve::dashboard::coordinator) struct Flight {
    /// Fires once when the builder resolves (success, error, or drop).
    pub(in crate::cli::serve::dashboard::coordinator) notify: Arc<Notify>,
    /// The completed outcome, stored by the builder BEFORE `notify_waiters`:
    /// a woken waiter is therefore guaranteed to either read `Some` or observe
    /// `None` only when the builder was cancelled/dropped mid-flight.
    pub(in crate::cli::serve::dashboard::coordinator) outcome:
        StdMutex<Option<Result<Arc<SnapshotPayload>, String>>>,
}

pub(in crate::cli::serve::dashboard::coordinator) fn new_flight() -> Arc<Flight> {
    Arc::new(Flight {
        notify: Arc::new(Notify::new()),
        outcome: StdMutex::new(None),
    })
}

/// Completion guard for an in-flight build. Cancellation, panic, or dropping an
/// unpolled detached task clears only its matching flight, preserves the last
/// successful cache, and wakes waiters so they can retry.
pub(in crate::cli::serve::dashboard::coordinator) struct BuildGuard {
    state: Arc<StdMutex<CoordinatorState>>,
    flight: Arc<Flight>,
    armed: bool,
}

impl BuildGuard {
    pub(in crate::cli::serve::dashboard::coordinator) fn new(
        state: Arc<StdMutex<CoordinatorState>>,
        flight: Arc<Flight>,
    ) -> Self {
        Self {
            state,
            flight,
            armed: true,
        }
    }

    /// The builder has already updated the shared state; suppress the reset.
    pub(in crate::cli::serve::dashboard::coordinator) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for BuildGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // A poisoned lock means another thread panicked while holding it; do
        // not double-panic during unwinding — leave the state as it is.
        if let Ok(mut state) = self.state.lock()
            && state
                .flight
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &self.flight))
        {
            state.flight = None;
            drop(state);
            // Flight outcome stays `None`: woken waiters observe the drop and
            // loop to retry (a fresh flight) instead of receiving anything
            // fabricate from a build that never completed.
            self.flight.notify.notify_waiters();
        }
    }
}
