//! Prometheus text exposition for bounded provider metrics.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Display;

use chrono::{DateTime, Utc};

use super::dashboard;
use super::dashboard::snapshot::SnapshotInput;
use crate::core::{ProviderId, RateWindow, RateWindowCadence};

const CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

const METRIC_DEFINITIONS: &[(&str, &str)] = &[
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

#[derive(Debug, Clone)]
pub(crate) struct MetricsSnapshot {
    schema_version: u32,
    generated_at: DateTime<Utc>,
    stale_after_seconds: u32,
    refresh_interval_seconds: u32,
    providers: Vec<ProviderMetrics>,
}

#[derive(Debug, Clone)]
struct ProviderMetrics {
    provider: &'static str,
    up: bool,
    updated_at: Option<DateTime<Utc>>,
    codex_quota: Option<CodexQuotaMetrics>,
    cost_today_usd: Option<f64>,
    cost_last_30_days_usd: Option<f64>,
}

#[derive(Debug, Clone)]
struct CodexQuotaMetrics {
    session: Option<QuotaMetric>,
    weekly: Option<QuotaMetric>,
    monthly: Option<QuotaMetric>,
    code_review: Option<QuotaMetric>,
}

#[derive(Debug, Clone)]
struct QuotaMetric {
    used_ratio: f64,
    reset_at: Option<DateTime<Utc>>,
}

impl MetricsSnapshot {
    pub(crate) fn from_input(input: &SnapshotInput) -> Self {
        let enabled = input
            .enabled
            .iter()
            .filter_map(|name| ProviderId::from_cli_name(name))
            .map(|id| id.cli_name())
            .collect::<BTreeSet<_>>();

        let providers = ProviderId::all()
            .iter()
            .copied()
            .filter(|id| enabled.contains(id.cli_name()))
            .map(|id| {
                let provider = input
                    .providers
                    .iter()
                    .find(|provider| ProviderId::from_cli_name(&provider.id) == Some(id));
                let cost = (id == ProviderId::Codex).then(|| {
                    input.costs.iter().find_map(|(name, cost)| {
                        (ProviderId::from_cli_name(name) == Some(ProviderId::Codex)).then_some(cost)
                    })
                });
                let cost = cost.flatten();
                let cost_today_usd = cost.and_then(|value| finite_value(value.today_usd));
                let cost_last_30_days_usd =
                    cost.and_then(|value| finite_value(value.last_30_days_usd));
                match provider.map(|provider| &provider.fetch) {
                    Some(Ok(result)) => ProviderMetrics {
                        provider: id.cli_name(),
                        up: true,
                        updated_at: Some(result.usage.updated_at),
                        codex_quota: (id == ProviderId::Codex).then(|| CodexQuotaMetrics {
                            session: quota_metric_for_cadence(
                                &result.usage.primary,
                                RateWindowCadence::Session,
                            ),
                            weekly: result.usage.secondary.as_ref().and_then(|window| {
                                quota_metric_for_cadence(window, RateWindowCadence::Weekly)
                            }),
                            monthly: result.usage.tertiary.as_ref().and_then(|window| {
                                quota_metric_for_cadence(window, RateWindowCadence::Monthly)
                            }),
                            code_review: result.usage.code_review_window().and_then(quota_metric),
                        }),
                        cost_today_usd,
                        cost_last_30_days_usd,
                    },
                    Some(Err(_)) | None => ProviderMetrics {
                        provider: id.cli_name(),
                        up: false,
                        updated_at: None,
                        codex_quota: None,
                        cost_today_usd,
                        cost_last_30_days_usd,
                    },
                }
            })
            .collect();

        Self {
            schema_version: 1,
            generated_at: input.generated_at,
            stale_after_seconds: input.refresh_seconds.saturating_mul(3).max(180),
            refresh_interval_seconds: input.refresh_seconds,
            providers,
        }
    }
}

fn finite_value(value: Option<f64>) -> Option<f64> {
    value.filter(|value| value.is_finite())
}

fn quota_metric(window: &RateWindow) -> Option<QuotaMetric> {
    if !window.usage_known()
        || window.is_informational
        || !window.used_percent.is_finite()
        || !(0.0..=100.0).contains(&window.used_percent)
    {
        return None;
    }
    Some(QuotaMetric {
        used_ratio: window.used_percent / 100.0,
        reset_at: window.resets_at,
    })
}

fn quota_metric_for_cadence(
    window: &RateWindow,
    expected: RateWindowCadence,
) -> Option<QuotaMetric> {
    let cadence = window
        .window_minutes
        .map(RateWindowCadence::from_minutes)
        .unwrap_or(RateWindowCadence::Unknown);
    (cadence == expected)
        .then(|| quota_metric(window))
        .flatten()
}

#[derive(Debug, PartialEq, Eq)]
enum MetricsRenderError {
    DuplicateSeries(String),
}

/// Scrapes never wait for provider I/O. An expired snapshot is refreshed in the
/// background while the last successful metrics sidecar remains available.
pub(super) fn response(state: &dashboard::DashboardState) -> String {
    let snapshot = state.coordinator.latest_metrics_or_trigger_refresh();
    metrics_response(snapshot.as_deref())
}

fn metrics_response(snapshot: Option<&MetricsSnapshot>) -> String {
    let (status, body) = match snapshot {
        Some(snapshot) => match render(snapshot) {
            Ok(body) => (200, body),
            Err(_) => {
                tracing::warn!("Prometheus rendering failed because metric series were duplicated");
                (500, render_unavailable())
            }
        },
        None => (200, render_unavailable()),
    };
    super::http_response(status, CONTENT_TYPE, body, &[("Cache-Control", "no-store")])
}

fn render(snapshot: &MetricsSnapshot) -> Result<String, MetricsRenderError> {
    render_at(snapshot, Utc::now())
}

fn render_at(snapshot: &MetricsSnapshot, now: DateTime<Utc>) -> Result<String, MetricsRenderError> {
    let mut writer = MetricsWriter::new();
    let snapshot_age = age_seconds(now, snapshot.generated_at);
    writer.sample("codexbar_up", &[], 1)?;
    writer.sample(
        "codexbar_snapshot_schema_version",
        &[],
        snapshot.schema_version,
    )?;
    writer.sample(
        "codexbar_snapshot_generated_timestamp_seconds",
        &[],
        snapshot.generated_at.timestamp(),
    )?;
    writer.sample("codexbar_snapshot_age_seconds", &[], snapshot_age)?;
    writer.sample(
        "codexbar_snapshot_stale_after_seconds",
        &[],
        snapshot.stale_after_seconds,
    )?;
    writer.sample(
        "codexbar_snapshot_stale",
        &[],
        bool_value(snapshot_age > i64::from(snapshot.stale_after_seconds)),
    )?;
    writer.sample(
        "codexbar_refresh_interval_seconds",
        &[],
        snapshot.refresh_interval_seconds,
    )?;

    for provider in &snapshot.providers {
        let provider_labels = [("provider", provider.provider)];
        writer.sample(
            "codexbar_provider_up",
            &provider_labels,
            bool_value(provider.up),
        )?;
        if provider.up
            && let Some(updated_at) = provider.updated_at
        {
            writer.sample(
                "codexbar_provider_updated_timestamp_seconds",
                &provider_labels,
                updated_at.timestamp(),
            )?;
            writer.sample(
                "codexbar_provider_data_age_seconds",
                &provider_labels,
                age_seconds(now, updated_at),
            )?;
        }
        if let Some(quota) = &provider.codex_quota {
            render_quota(
                &mut writer,
                &provider_labels,
                quota.session.as_ref(),
                QuotaMetricNames::SESSION,
            )?;
            render_quota(
                &mut writer,
                &provider_labels,
                quota.weekly.as_ref(),
                QuotaMetricNames::WEEKLY,
            )?;
            render_quota(
                &mut writer,
                &provider_labels,
                quota.monthly.as_ref(),
                QuotaMetricNames::MONTHLY,
            )?;
            render_quota(
                &mut writer,
                &provider_labels,
                quota.code_review.as_ref(),
                QuotaMetricNames::CODE_REVIEW,
            )?;
        }
        if let Some(value) = provider.cost_today_usd {
            writer.sample_f64("codexbar_cost_today_usd", &provider_labels, value)?;
        }
        if let Some(value) = provider.cost_last_30_days_usd {
            writer.sample_f64("codexbar_cost_last_30_days_usd", &provider_labels, value)?;
        }
    }

    Ok(writer.finish())
}

fn render_unavailable() -> String {
    let mut writer = MetricsWriter::new();
    writer
        .sample("codexbar_up", &[], 0)
        .expect("the exporter health series is unique");
    writer.finish()
}

#[derive(Clone, Copy)]
struct QuotaMetricNames {
    used: &'static str,
    remaining: &'static str,
    reset: &'static str,
}

impl QuotaMetricNames {
    const SESSION: Self = Self {
        used: "codexbar_quota_session_used_ratio",
        remaining: "codexbar_quota_session_remaining_ratio",
        reset: "codexbar_quota_session_reset_timestamp_seconds",
    };
    const WEEKLY: Self = Self {
        used: "codexbar_quota_weekly_used_ratio",
        remaining: "codexbar_quota_weekly_remaining_ratio",
        reset: "codexbar_quota_weekly_reset_timestamp_seconds",
    };
    const MONTHLY: Self = Self {
        used: "codexbar_quota_monthly_used_ratio",
        remaining: "codexbar_quota_monthly_remaining_ratio",
        reset: "codexbar_quota_monthly_reset_timestamp_seconds",
    };
    const CODE_REVIEW: Self = Self {
        used: "codexbar_quota_code_review_used_ratio",
        remaining: "codexbar_quota_code_review_remaining_ratio",
        reset: "codexbar_quota_code_review_reset_timestamp_seconds",
    };
}

fn render_quota(
    writer: &mut MetricsWriter,
    labels: &[(&str, &str)],
    quota: Option<&QuotaMetric>,
    names: QuotaMetricNames,
) -> Result<(), MetricsRenderError> {
    if let Some(quota) = quota {
        writer.sample_f64(names.used, labels, quota.used_ratio)?;
        writer.sample_f64(names.remaining, labels, 1.0 - quota.used_ratio)?;
        if let Some(reset_at) = quota.reset_at {
            writer.sample(names.reset, labels, reset_at.timestamp())?;
        }
    }
    Ok(())
}

fn bool_value(value: bool) -> u8 {
    u8::from(value)
}

fn age_seconds(now: DateTime<Utc>, updated_at: DateTime<Utc>) -> i64 {
    (now - updated_at).num_seconds().max(0)
}

struct MetricsWriter {
    samples: BTreeMap<String, Vec<String>>,
    series: BTreeSet<String>,
}

impl MetricsWriter {
    fn new() -> Self {
        Self {
            samples: BTreeMap::new(),
            series: BTreeSet::new(),
        }
    }

    fn sample(
        &mut self,
        name: &str,
        labels: &[(&str, &str)],
        value: impl Display,
    ) -> Result<(), MetricsRenderError> {
        let series = format_series(name, labels);
        if !self.series.insert(series.clone()) {
            return Err(MetricsRenderError::DuplicateSeries(series));
        }
        self.samples
            .entry(name.to_string())
            .or_default()
            .push(format!("{series} {value}\n"));
        Ok(())
    }

    fn sample_f64(
        &mut self,
        name: &str,
        labels: &[(&str, &str)],
        value: f64,
    ) -> Result<(), MetricsRenderError> {
        if value.is_finite() {
            self.sample(name, labels, value)?;
        }
        Ok(())
    }

    fn finish(self) -> String {
        let mut body = String::new();
        let mut samples = self.samples;
        for &(name, help) in METRIC_DEFINITIONS {
            body.push_str("# HELP ");
            body.push_str(name);
            body.push(' ');
            body.push_str(help);
            body.push('\n');
            body.push_str("# TYPE ");
            body.push_str(name);
            body.push_str(" gauge\n");
            if let Some(lines) = samples.remove(name) {
                for line in lines {
                    body.push_str(&line);
                }
            }
        }
        for lines in samples.into_values() {
            for line in lines {
                body.push_str(&line);
            }
        }
        debug_assert!(body.ends_with('\n'));
        body
    }
}

fn format_series(name: &str, labels: &[(&str, &str)]) -> String {
    if labels.is_empty() {
        return name.to_string();
    }
    let mut labels = labels.to_vec();
    labels.sort_unstable_by(|left, right| left.0.cmp(right.0));
    let labels = labels
        .into_iter()
        .map(|(name, value)| format!(r#"{name}="{}""#, escape_label(value)))
        .collect::<Vec<_>>()
        .join(",");
    format!("{name}{{{labels}}}")
}

fn escape_label(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' | '\r' => escaped.push_str("\\n"),
            _ => escaped.push(character),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use chrono::{TimeZone, Utc};

    use super::*;
    use crate::cli::serve::dashboard::coordinator::SnapshotArtifactsBuildFn;
    use crate::cli::serve::dashboard::snapshot::{
        DashboardIdentity, ProviderFetchEnvelope, RawCostPayload, SnapshotInput, build_snapshot,
    };
    use crate::cli::serve::dashboard::source::SnapshotArtifacts;
    use crate::core::{NamedRateWindow, ProviderFetchResult, RateWindow, UsageSnapshot};
    use crate::providers::codex::CodexApi;

    fn at(hour: u32) -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 14, hour, 0, 0)
            .single()
            .unwrap()
    }

    fn provider(id: &str, fetch: Result<ProviderFetchResult, String>) -> ProviderFetchEnvelope {
        ProviderFetchEnvelope {
            id: id.to_string(),
            display_name: format!("Sensitive {id} name"),
            session_label: "Session".to_string(),
            weekly_label: "Weekly".to_string(),
            fetch,
        }
    }

    fn usage(primary: RateWindow) -> UsageSnapshot {
        let mut usage = UsageSnapshot::new(primary);
        usage.updated_at = at(1);
        usage.account_email = Some("owner@example.test".to_string());
        usage.login_method = Some("Sensitive plan".to_string());
        usage
    }

    fn codex_usage_from_json(json: serde_json::Value) -> UsageSnapshot {
        CodexApi::new()
            .build_result_from_json_for_test(&json)
            .expect("Codex usage")
            .0
    }

    fn input(
        providers: Vec<ProviderFetchEnvelope>,
        costs: HashMap<String, RawCostPayload>,
    ) -> SnapshotInput {
        let enabled = providers
            .iter()
            .map(|provider| provider.id.clone())
            .collect::<BTreeSet<_>>();
        input_with_enabled(providers, costs, enabled)
    }

    fn input_with_enabled(
        providers: Vec<ProviderFetchEnvelope>,
        costs: HashMap<String, RawCostPayload>,
        enabled: BTreeSet<String>,
    ) -> SnapshotInput {
        let order = enabled.iter().cloned().collect();
        SnapshotInput {
            providers,
            costs,
            claude_accounts: None,
            identity: DashboardIdentity::Full,
            generated_at: at(0),
            refresh_seconds: 60,
            version: Some("0.56.8".to_string()),
            order,
            enabled,
        }
    }

    fn metrics_snapshot(
        providers: Vec<ProviderFetchEnvelope>,
        costs: HashMap<String, RawCostPayload>,
    ) -> MetricsSnapshot {
        MetricsSnapshot::from_input(&input(providers, costs))
    }

    fn artifacts(input: SnapshotInput) -> SnapshotArtifacts {
        let metrics = MetricsSnapshot::from_input(&input);
        SnapshotArtifacts {
            dashboard: build_snapshot(&input),
            metrics: Some(metrics),
        }
    }

    fn samples<'a>(body: &'a str, metric: &'a str) -> impl Iterator<Item = &'a str> {
        body.lines().filter(move |line| {
            line.starts_with(metric)
                && line
                    .as_bytes()
                    .get(metric.len())
                    .is_some_and(|next| matches!(next, b' ' | b'{'))
        })
    }

    fn metric_name(line: &str) -> Option<&str> {
        if let Some(rest) = line
            .strip_prefix("# HELP ")
            .or_else(|| line.strip_prefix("# TYPE "))
        {
            return rest.split_ascii_whitespace().next();
        }
        if line.starts_with('#') || line.is_empty() {
            return None;
        }
        line.split(['{', ' ', '\t']).next()
    }

    fn assert_metric_families_are_contiguous(body: &str) {
        let mut closed = BTreeSet::new();
        let mut current = None;
        for line in body.lines() {
            let Some(name) = metric_name(line) else {
                continue;
            };
            if current == Some(name) {
                continue;
            }
            if let Some(previous) = current.replace(name) {
                closed.insert(previous);
            }
            assert!(
                !closed.contains(name),
                "metric family {name} is split into multiple groups"
            );
        }
    }

    #[test]
    fn renders_enabled_provider_health_and_codex_metrics_without_private_text() {
        let mut codex_usage = usage(RateWindow::with_details(25.0, Some(300), Some(at(3)), None));
        codex_usage.secondary = Some(RateWindow::with_details(
            40.0,
            Some(10_080),
            Some(at(4)),
            None,
        ));
        let mut costs = HashMap::new();
        costs.insert(
            "codex".to_string(),
            RawCostPayload {
                today_usd: Some(1.25),
                last_30_days_usd: Some(12.5),
            },
        );
        costs.insert(
            "anthropic".to_string(),
            RawCostPayload {
                today_usd: Some(0.75),
                last_30_days_usd: None,
            },
        );
        let snapshot = metrics_snapshot(
            vec![
                provider(
                    "openai",
                    Ok(ProviderFetchResult::new(codex_usage, "sensitive-source")),
                ),
                provider("claude", Err("private provider failure".to_string())),
            ],
            costs,
        );

        let body = render_at(&snapshot, at(5)).unwrap();
        assert!(body.contains("codexbar_up 1\n"));
        assert!(body.contains("codexbar_provider_up{provider=\"codex\"} 1\n"));
        assert!(body.contains("codexbar_provider_up{provider=\"claude\"} 0\n"));
        assert!(body.contains("codexbar_quota_session_used_ratio{provider=\"codex\"} 0.25\n"));
        assert!(body.contains(
            "codexbar_quota_session_reset_timestamp_seconds{provider=\"codex\"} 1789354800\n"
        ));
        assert!(body.contains("codexbar_quota_weekly_remaining_ratio{provider=\"codex\"} 0.6\n"));
        assert!(body.contains("codexbar_cost_last_30_days_usd{provider=\"codex\"} 12.5\n"));
        assert!(!body.contains("codexbar_cost_today_usd{provider=\"claude\"}"));
        assert!(!body.contains("codexbar_build_info"));
        assert!(!body.contains("version=\""));
        assert!(!body.contains("window=\""));
        assert!(body.ends_with('\n'));

        for private in [
            "owner@example.test",
            "Sensitive plan",
            "Sensitive codex name",
            "sensitive-source",
            "private provider failure",
        ] {
            assert!(!body.contains(private), "private text leaked: {private}");
        }
    }

    #[test]
    fn exports_only_fixed_quota_slots_and_omits_extras() {
        let mut provider_usage = usage(RateWindow::no_active_session());
        provider_usage.secondary = Some(RateWindow::with_details(35.0, Some(10_080), None, None));
        provider_usage.tertiary = Some(RateWindow::with_details(45.0, Some(43_200), None, None));
        provider_usage = provider_usage.with_code_review(RateWindow::new(55.0));
        provider_usage.extra_rate_windows.extend([
            NamedRateWindow::new("reset-credits", "Reset credits", RateWindow::new(0.0)),
            NamedRateWindow::new("customer-defined", "Tenant Secret", RateWindow::new(99.0)),
            NamedRateWindow::new("codex-spark", "Codex Spark", RateWindow::new(20.0)),
        ]);
        let snapshot = metrics_snapshot(
            vec![provider(
                "codex",
                Ok(ProviderFetchResult::new(provider_usage, "cli")),
            )],
            HashMap::new(),
        );

        let body = render_at(&snapshot, at(2)).unwrap();
        assert_eq!(
            samples(&body, "codexbar_quota_session_used_ratio").count(),
            0
        );
        assert!(body.contains("codexbar_quota_weekly_used_ratio{provider=\"codex\"} 0.35\n"));
        assert!(body.contains("codexbar_quota_code_review_used_ratio{provider=\"codex\"} 0.55\n"));
        assert!(body.contains("codexbar_quota_monthly_used_ratio{provider=\"codex\"} 0.45\n"));
        for excluded in [
            "window=\"",
            "reset-credits",
            "customer-defined",
            "Tenant Secret",
            "codex-spark",
        ] {
            assert!(
                !body.contains(excluded),
                "unexpected exported value: {excluded}"
            );
        }
    }

    #[test]
    fn canonical_provider_labels_keep_metric_families_bounded_and_contiguous() {
        let mut codex_usage = usage(RateWindow::new(25.0));
        codex_usage.extra_rate_windows.push(NamedRateWindow::new(
            "dynamic-tenant-id",
            "Sensitive tenant label",
            RateWindow::new(90.0),
        ));
        let body = render_at(
            &metrics_snapshot(
                vec![
                    provider("openai", Ok(ProviderFetchResult::new(codex_usage, "cli"))),
                    provider(
                        "anthropic",
                        Ok(ProviderFetchResult::new(
                            usage(RateWindow::new(50.0)),
                            "cli",
                        )),
                    ),
                    provider(
                        "co\"dex\\lan\nnode",
                        Ok(ProviderFetchResult::new(
                            usage(RateWindow::new(50.0)),
                            "cli",
                        )),
                    ),
                ],
                HashMap::new(),
            ),
            at(2),
        )
        .unwrap();

        assert_metric_families_are_contiguous(&body);
        assert_eq!(samples(&body, "codexbar_provider_up").count(), 2);
        assert!(body.contains("codexbar_provider_up{provider=\"codex\"} 1\n"));
        assert!(body.contains("codexbar_provider_up{provider=\"claude\"} 1\n"));
        assert!(!body.contains("dynamic-tenant-id"));
        assert!(!body.contains("Sensitive tenant label"));
        assert!(!body.contains("co\\\"dex"));
    }

    #[test]
    fn does_not_infer_unknown_informational_non_finite_or_out_of_range_values() {
        let mut primary = RateWindow::with_details(25.0, Some(300), None, None);
        primary.used_percent = f64::NAN;
        let mut model = RateWindow::new(25.0);
        model.used_percent = 125.0;
        let mut tertiary = RateWindow::with_details(25.0, Some(43_200), None, None);
        tertiary.used_percent = -25.0;
        let mut provider_usage = usage(primary);
        provider_usage.secondary = Some(RateWindow::informational("unknown allowance"));
        provider_usage = provider_usage.with_code_review(model);
        provider_usage.tertiary = Some(tertiary);
        let mut costs = HashMap::new();
        costs.insert(
            "codex".to_string(),
            RawCostPayload {
                today_usd: Some(f64::NAN),
                last_30_days_usd: Some(f64::INFINITY),
            },
        );

        let body = render_at(
            &metrics_snapshot(
                vec![provider(
                    "codex",
                    Ok(ProviderFetchResult::new(provider_usage, "cli")),
                )],
                costs,
            ),
            at(2),
        )
        .unwrap();

        assert_eq!(
            samples(&body, "codexbar_quota_session_used_ratio").count(),
            0
        );
        assert_eq!(
            samples(&body, "codexbar_quota_weekly_used_ratio").count(),
            0
        );
        assert_eq!(
            samples(&body, "codexbar_quota_code_review_used_ratio").count(),
            0
        );
        assert_eq!(
            samples(&body, "codexbar_quota_monthly_used_ratio").count(),
            0
        );
        assert_eq!(samples(&body, "codexbar_cost_today_usd").count(), 0);
        assert_eq!(samples(&body, "codexbar_cost_last_30_days_usd").count(), 0);
        assert!(!body.contains("unknown allowance"));
    }

    #[test]
    fn parsed_missing_usage_is_not_exported_as_zero() {
        let usage = codex_usage_from_json(serde_json::json!({
            "rate_limit": {
                "primary_window": {
                    "limit_window_seconds": 18_000,
                    "reset_at": 1_789_354_800
                }
            }
        }));
        let body = render_at(
            &metrics_snapshot(
                vec![provider(
                    "codex",
                    Ok(ProviderFetchResult::new(usage, "oauth")),
                )],
                HashMap::new(),
            ),
            at(2),
        )
        .unwrap();

        assert!(body.contains("codexbar_provider_up{provider=\"codex\"} 1\n"));
        assert_eq!(
            samples(&body, "codexbar_quota_session_used_ratio").count(),
            0
        );
        assert_eq!(
            samples(&body, "codexbar_quota_session_reset_timestamp_seconds").count(),
            0
        );
    }

    #[test]
    fn parsed_positional_fallback_without_cadence_is_not_exported() {
        let usage = codex_usage_from_json(serde_json::json!({
            "rate_limits": [
                { "used_percent": 10 },
                { "used_percent": 20 },
                { "used_percent": 30 },
                { "used_percent": 40 }
            ]
        }));
        let body = render_at(
            &metrics_snapshot(
                vec![provider(
                    "codex",
                    Ok(ProviderFetchResult::new(usage, "oauth")),
                )],
                HashMap::new(),
            ),
            at(2),
        )
        .unwrap();

        for metric in [
            "codexbar_quota_session_used_ratio",
            "codexbar_quota_weekly_used_ratio",
            "codexbar_quota_monthly_used_ratio",
            "codexbar_quota_code_review_used_ratio",
        ] {
            assert_eq!(samples(&body, metric).count(), 0, "unexpected {metric}");
        }
    }

    #[test]
    fn parsed_verified_cadences_and_code_review_are_exported() {
        let named = codex_usage_from_json(serde_json::json!({
            "rate_limit": {
                "primary_window": {
                    "used_percent": 25,
                    "limit_window_seconds": 18_000
                },
                "secondary_window": {
                    "used_percent": 40,
                    "limit_window_seconds": 604_800
                },
                "code_review_window": { "used_percent": 55 }
            }
        }));
        let named_body = render_at(
            &metrics_snapshot(
                vec![provider(
                    "codex",
                    Ok(ProviderFetchResult::new(named, "oauth")),
                )],
                HashMap::new(),
            ),
            at(2),
        )
        .unwrap();
        assert!(
            named_body.contains("codexbar_quota_session_used_ratio{provider=\"codex\"} 0.25\n")
        );
        assert!(named_body.contains("codexbar_quota_weekly_used_ratio{provider=\"codex\"} 0.4\n"));
        assert!(
            named_body.contains("codexbar_quota_code_review_used_ratio{provider=\"codex\"} 0.55\n")
        );

        let array = codex_usage_from_json(serde_json::json!({
            "rate_limits": [
                { "used_percent": 45, "limit_window_seconds": 2_592_000 }
            ]
        }));
        let array_body = render_at(
            &metrics_snapshot(
                vec![provider(
                    "codex",
                    Ok(ProviderFetchResult::new(array, "oauth")),
                )],
                HashMap::new(),
            ),
            at(2),
        )
        .unwrap();
        assert!(
            array_body.contains("codexbar_quota_monthly_used_ratio{provider=\"codex\"} 0.45\n")
        );
    }

    #[test]
    fn duplicate_series_invariant_failure_returns_http_500() {
        let mut snapshot = metrics_snapshot(
            vec![provider(
                "codex",
                Ok(ProviderFetchResult::new(
                    usage(RateWindow::new(25.0)),
                    "cli",
                )),
            )],
            HashMap::new(),
        );
        snapshot.providers.push(snapshot.providers[0].clone());

        let response = metrics_response(Some(&snapshot));

        assert!(response.starts_with("HTTP/1.1 500 Internal Server Error\r\n"));
        assert!(response.contains("codexbar_up 0\n"));
    }

    #[test]
    fn freshness_metrics_are_deterministic_clamped_and_use_strict_staleness() {
        let snapshot = metrics_snapshot(
            vec![provider(
                "codex",
                Ok(ProviderFetchResult::new(
                    usage(RateWindow::new(25.0)),
                    "cli",
                )),
            )],
            HashMap::new(),
        );
        let at_threshold = render_at(
            &snapshot,
            snapshot.generated_at + chrono::Duration::seconds(180),
        )
        .unwrap();
        assert!(at_threshold.contains("codexbar_snapshot_age_seconds 180\n"));
        assert!(at_threshold.contains("codexbar_snapshot_stale 0\n"));

        let stale = render_at(
            &snapshot,
            snapshot.generated_at + chrono::Duration::seconds(181),
        )
        .unwrap();
        assert!(stale.contains("codexbar_snapshot_stale 1\n"));

        let before_provider_update = render_at(&snapshot, snapshot.generated_at).unwrap();
        assert!(
            before_provider_update
                .contains("codexbar_provider_data_age_seconds{provider=\"codex\"} 0\n")
        );
    }

    #[test]
    fn partial_claude_failure_keeps_codex_metrics_usable() {
        let input = input_with_enabled(
            vec![
                provider(
                    "codex",
                    Ok(ProviderFetchResult::new(
                        usage(RateWindow::with_details(50.0, Some(300), None, None)),
                        "cli",
                    )),
                ),
                provider("claude", Err("private failure".to_string())),
            ],
            HashMap::new(),
            ["codex", "claude", "cursor"]
                .into_iter()
                .map(str::to_string)
                .collect(),
        );
        let body = render_at(&MetricsSnapshot::from_input(&input), at(2)).unwrap();

        assert!(body.contains("codexbar_provider_up{provider=\"codex\"} 1\n"));
        assert!(body.contains("codexbar_provider_up{provider=\"claude\"} 0\n"));
        assert!(body.contains("codexbar_provider_up{provider=\"cursor\"} 0\n"));
        assert!(body.contains("codexbar_quota_session_used_ratio{provider=\"codex\"} 0.5\n"));
        assert!(!body.contains("codexbar_quota_session_used_ratio{provider=\"claude\"}"));
        assert!(!body.contains("private failure"));

        let disabled = render_at(&metrics_snapshot(Vec::new(), HashMap::new()), at(2)).unwrap();
        assert_eq!(samples(&disabled, "codexbar_provider_up").count(), 0);
        assert!(disabled.contains("codexbar_up 1\n"));
    }

    #[tokio::test]
    async fn scrape_reads_cache_and_single_flights_the_shared_snapshot_builder() {
        // The metrics route owns no provider client. Blocking the shared dashboard
        // builder proves scrapes return from cache and do not start a second path.
        let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
        let started_tx = Arc::new(Mutex::new(Some(started_tx)));
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let release_rx = Arc::new(Mutex::new(Some(release_rx)));
        let build_count = Arc::new(AtomicUsize::new(0));
        let payload = artifacts(input(
            vec![provider(
                "codex",
                Ok(ProviderFetchResult::new(
                    usage(RateWindow::with_details(25.0, Some(300), None, None)),
                    "cli",
                )),
            )],
            HashMap::new(),
        ));
        let builder_count = build_count.clone();
        let build: SnapshotArtifactsBuildFn = Arc::new(move || {
            let started_tx = started_tx.clone();
            let release_rx = release_rx.clone();
            let build_count = builder_count.clone();
            let payload = payload.clone();
            Box::pin(async move {
                build_count.fetch_add(1, Ordering::SeqCst);
                if let Some(started_tx) = started_tx.lock().expect("poisoned").take() {
                    started_tx
                        .send(())
                        .expect("background collection start receiver must remain alive");
                }
                let release_rx = release_rx.lock().expect("poisoned").take().unwrap();
                release_rx
                    .await
                    .expect("background collection release sender must remain alive");
                Ok(payload)
            })
        });
        let state = dashboard::DashboardState::stub_with_artifacts(
            build,
            3600,
            Some(DashboardIdentity::Redacted),
        );

        let first = response(&state);
        assert!(first.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(first.contains("codexbar_up 0\n"));
        tokio::time::timeout(Duration::from_secs(5), started_rx)
            .await
            .expect("background collection did not start")
            .expect("background collection start sender dropped");

        let second = response(&state);
        assert!(second.contains("codexbar_up 0\n"));
        assert_eq!(build_count.load(Ordering::SeqCst), 1);
        release_tx
            .send(())
            .expect("background collection must still be waiting");
        state.coordinator.get().await.unwrap();

        let ready = response(&state);
        assert!(ready.contains("codexbar_up 1\n"));
        assert!(ready.contains("codexbar_quota_session_used_ratio"));
        assert_eq!(build_count.load(Ordering::SeqCst), 1);
    }
}
