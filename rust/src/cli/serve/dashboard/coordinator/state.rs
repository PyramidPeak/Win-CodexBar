//! Shared cache and in-flight state for the snapshot coordinator.

use std::sync::Arc;
use std::time::Instant;

use crate::cli::serve::metrics::MetricsSnapshot;

use super::super::snapshot::SnapshotPayload;
use super::flight::Flight;

#[derive(Clone, Debug)]
pub(in crate::cli::serve::dashboard::coordinator) struct CachedSnapshot {
    pub(in crate::cli::serve::dashboard::coordinator) payload: Arc<SnapshotPayload>,
    pub(in crate::cli::serve::dashboard::coordinator) metrics: Option<Arc<MetricsSnapshot>>,
    pub(in crate::cli::serve::dashboard::coordinator) built_at: Instant,
}

/// Cache and in-flight generation are deliberately independent: an expired
/// successful snapshot remains available while its replacement is built.
#[derive(Debug, Default)]
pub(in crate::cli::serve::dashboard::coordinator) struct CoordinatorState {
    pub(in crate::cli::serve::dashboard::coordinator) cached: Option<CachedSnapshot>,
    pub(in crate::cli::serve::dashboard::coordinator) flight: Option<Arc<Flight>>,
}
