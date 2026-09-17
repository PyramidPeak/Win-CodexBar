//! Stable metric names and help text for the Prometheus exposition.

pub(in crate::cli::serve::metrics) const METRIC_DEFINITIONS: &[(&str, &str)] = &[
    (
        "codexbar_up",
        "Whether CodexBar has a successfully collected metrics snapshot to export.",
    ),
    (
        "codexbar_snapshot_schema_version",
        "Dashboard snapshot schema version used for collection.",
    ),
    (
        "codexbar_snapshot_generated_timestamp_seconds",
        "Unix timestamp when the collection snapshot was generated.",
    ),
    (
        "codexbar_snapshot_age_seconds",
        "Age of the collection snapshot.",
    ),
    (
        "codexbar_snapshot_stale_after_seconds",
        "Age after which the collection snapshot is stale.",
    ),
    (
        "codexbar_snapshot_stale",
        "Whether the collection snapshot is older than its stale threshold.",
    ),
    (
        "codexbar_refresh_interval_seconds",
        "Configured dashboard refresh interval.",
    ),
    (
        "codexbar_provider_up",
        "Whether the provider usage fetch succeeded.",
    ),
    (
        "codexbar_provider_updated_timestamp_seconds",
        "Unix timestamp of the provider usage data update.",
    ),
    (
        "codexbar_provider_data_age_seconds",
        "Age of the provider usage data.",
    ),
    (
        "codexbar_quota_session_used_ratio",
        "Used quota ratio for the Codex session window.",
    ),
    (
        "codexbar_quota_session_remaining_ratio",
        "Remaining quota ratio for the Codex session window.",
    ),
    (
        "codexbar_quota_session_reset_timestamp_seconds",
        "Unix timestamp when the Codex session window resets.",
    ),
    (
        "codexbar_quota_weekly_used_ratio",
        "Used quota ratio for the Codex weekly window.",
    ),
    (
        "codexbar_quota_weekly_remaining_ratio",
        "Remaining quota ratio for the Codex weekly window.",
    ),
    (
        "codexbar_quota_weekly_reset_timestamp_seconds",
        "Unix timestamp when the Codex weekly window resets.",
    ),
    (
        "codexbar_quota_monthly_used_ratio",
        "Used quota ratio for the Codex monthly window.",
    ),
    (
        "codexbar_quota_monthly_remaining_ratio",
        "Remaining quota ratio for the Codex monthly window.",
    ),
    (
        "codexbar_quota_monthly_reset_timestamp_seconds",
        "Unix timestamp when the Codex monthly window resets.",
    ),
    (
        "codexbar_quota_code_review_used_ratio",
        "Used quota ratio for the Codex code review window.",
    ),
    (
        "codexbar_quota_code_review_remaining_ratio",
        "Remaining quota ratio for the Codex code review window.",
    ),
    (
        "codexbar_quota_code_review_reset_timestamp_seconds",
        "Unix timestamp when the Codex code review window resets.",
    ),
    (
        "codexbar_cost_today_usd",
        "Available local Codex cost today in US dollars.",
    ),
    (
        "codexbar_cost_last_30_days_usd",
        "Available local Codex cost over the last 30 days in US dollars.",
    ),
];
