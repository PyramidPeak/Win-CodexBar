//! Prometheus text exposition for bounded provider metrics.

use super::dashboard;

const CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

#[derive(Debug, PartialEq, Eq)]
pub(super) enum MetricsRenderError {
    DuplicateSeries(String),
}

mod definitions;
mod encoding;
mod rendering;
mod snapshot;

pub(crate) use snapshot::MetricsSnapshot;

pub(super) fn response(state: &dashboard::DashboardState) -> String {
    rendering::response(state)
}

pub(super) fn metrics_response(snapshot: Option<&MetricsSnapshot>) -> String {
    rendering::metrics_response(snapshot)
}

pub(super) fn render_at(
    snapshot: &MetricsSnapshot,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<String, MetricsRenderError> {
    rendering::render_at(snapshot, now)
}

#[cfg(test)]
mod tests;
