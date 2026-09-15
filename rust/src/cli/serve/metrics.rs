//! Prometheus text exposition for the bounded Codex metrics contract.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Display;

use chrono::{DateTime, Utc};

use super::dashboard;
use super::dashboard::snapshot::SnapshotInput;
use crate::core::{RateWindow, UsageSnapshot};

const CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";
const CODEX_PROVIDER_ID: &str = "codex";

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
    ("codexbar_build_info", "CodexBar build information."),
    (
        "codexbar_provider_up",
        "Whether the Codex usage fetch succeeded.",
    ),
    (
        "codexbar_provider_updated_timestamp_seconds",
        "Unix timestamp of the Codex usage data update.",
    ),
    (
        "codexbar_provider_data_age_seconds",
        "Age of the Codex usage data.",
    ),
    (
        "codexbar_quota_used_percent",
        "Used quota percentage for a stable Codex quota window.",
    ),
    (
        "codexbar_quota_remaining_percent",
        "Remaining quota percentage for a stable Codex quota window.",
    ),
    (
        "codexbar_quota_reset_timestamp_seconds",
        "Unix timestamp when a stable Codex quota window resets.",
    ),
    (
        "codexbar_cost_today_usd",
        "Estimated local Codex cost today in US dollars.",
    ),
    (
        "codexbar_cost_last_30_days_usd",
        "Estimated local Codex cost over the last 30 days in US dollars.",
    ),
];

#[derive(Debug, Clone)]
pub(crate) struct MetricsSnapshot {
    schema_version: u32,
    generated_at: DateTime<Utc>,
    stale_after_seconds: u32,
    refresh_interval_seconds: u32,
    version: Option<String>,
    codex: Option<CodexMetrics>,
}

#[derive(Debug, Clone)]
struct CodexMetrics {
    up: bool,
    updated_at: Option<DateTime<Utc>>,
    windows: Vec<QuotaMetric>,
    cost_today_usd: Option<f64>,
    cost_last_30_days_usd: Option<f64>,
}

#[derive(Debug, Clone)]
struct QuotaMetric {
    window: &'static str,
    used_percent: f64,
    reset_at: Option<DateTime<Utc>>,
}

impl MetricsSnapshot {
    pub(crate) fn from_input(input: &SnapshotInput) -> Self {
        let codex = input
            .providers
            .iter()
            .find(|provider| provider.id == CODEX_PROVIDER_ID)
            .map(|provider| {
                let cost = input.costs.get(CODEX_PROVIDER_ID);
                match &provider.fetch {
                    Ok(result) => CodexMetrics {
                        up: true,
                        updated_at: Some(result.usage.updated_at),
                        windows: stable_codex_windows(&result.usage),
                        cost_today_usd: cost.and_then(|value| value.today_usd),
                        cost_last_30_days_usd: cost.and_then(|value| value.last_30_days_usd),
                    },
                    Err(_) => CodexMetrics {
                        up: false,
                        updated_at: None,
                        windows: Vec::new(),
                        cost_today_usd: cost.and_then(|value| value.today_usd),
                        cost_last_30_days_usd: cost.and_then(|value| value.last_30_days_usd),
                    },
                }
            });

        Self {
            schema_version: 1,
            generated_at: input.generated_at,
            stale_after_seconds: input.refresh_seconds.saturating_mul(3).max(180),
            refresh_interval_seconds: input.refresh_seconds,
            version: input.version.clone(),
            codex,
        }
    }
}

fn stable_codex_windows(usage: &UsageSnapshot) -> Vec<QuotaMetric> {
    let mut windows = Vec::with_capacity(4);
    push_window(&mut windows, "session", &usage.primary);
    if let Some(window) = &usage.secondary {
        push_window(&mut windows, "weekly", window);
    }
    if let Some(window) = &usage.tertiary {
        push_window(&mut windows, "monthly", window);
    }
    if let Some(window) = &usage.model_specific {
        push_window(&mut windows, "code_review", window);
    }
    windows
}

fn push_window(windows: &mut Vec<QuotaMetric>, name: &'static str, window: &RateWindow) {
    if window.is_informational {
        return;
    }
    windows.push(QuotaMetric {
        window: name,
        used_percent: window.used_percent.clamp(0.0, 100.0),
        reset_at: window.resets_at,
    });
}

#[derive(Debug, PartialEq, Eq)]
enum MetricsRenderError {
    DuplicateSeries(String),
}

/// Scrapes never wait for provider I/O. An expired snapshot is refreshed in the
/// background while the last successful metrics sidecar remains available.
pub(super) fn response(state: &dashboard::DashboardState) -> String {
    let body = match state.coordinator.latest_metrics_or_trigger_refresh() {
        Some(snapshot) => match render(snapshot.as_ref()) {
            Ok(body) => body,
            Err(_) => {
                tracing::warn!("Prometheus rendering failed because metric series were duplicated");
                render_unavailable()
            }
        },
        None => render_unavailable(),
    };
    super::http_response(200, CONTENT_TYPE, body, &[("Cache-Control", "no-store")])
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
    if let Some(version) = snapshot
        .version
        .as_deref()
        .map(str::trim)
        .filter(|version| !version.is_empty())
    {
        let schema_version = snapshot.schema_version.to_string();
        writer.sample(
            "codexbar_build_info",
            &[
                ("version", version),
                ("schema_version", schema_version.as_str()),
            ],
            1,
        )?;
    }

    let provider_labels = [("provider", CODEX_PROVIDER_ID)];
    writer.sample(
        "codexbar_provider_up",
        &provider_labels,
        bool_value(snapshot.codex.as_ref().is_some_and(|codex| codex.up)),
    )?;
    if let Some(codex) = &snapshot.codex {
        if codex.up
            && let Some(updated_at) = codex.updated_at
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
        for window in &codex.windows {
            render_provider_window(&mut writer, window)?;
        }
        if let Some(value) = codex.cost_today_usd {
            writer.sample_f64("codexbar_cost_today_usd", &provider_labels, value)?;
        }
        if let Some(value) = codex.cost_last_30_days_usd {
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

fn render_provider_window(
    writer: &mut MetricsWriter,
    window: &QuotaMetric,
) -> Result<(), MetricsRenderError> {
    let labels = [("provider", CODEX_PROVIDER_ID), ("window", window.window)];
    writer.sample_f64("codexbar_quota_used_percent", &labels, window.used_percent)?;
    writer.sample_f64(
        "codexbar_quota_remaining_percent",
        &labels,
        (100.0 - window.used_percent).clamp(0.0, 100.0),
    )?;
    if let Some(reset_at) = window.reset_at {
        writer.sample(
            "codexbar_quota_reset_timestamp_seconds",
            &labels,
            reset_at.timestamp(),
        )?;
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

    fn input(
        providers: Vec<ProviderFetchEnvelope>,
        costs: HashMap<String, RawCostPayload>,
    ) -> SnapshotInput {
        let order: Vec<String> = providers
            .iter()
            .map(|provider| provider.id.clone())
            .collect();
        let enabled: BTreeSet<String> = order.iter().cloned().collect();
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
    fn renders_codex_only_without_private_text() {
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
        let snapshot = metrics_snapshot(
            vec![
                provider(
                    "codex",
                    Ok(ProviderFetchResult::new(codex_usage, "sensitive-source")),
                ),
                provider("claude", Err("private provider failure".to_string())),
            ],
            costs,
        );

        let body = render_at(&snapshot, at(5)).unwrap();
        assert!(body.contains("codexbar_up 1\n"));
        assert!(body.contains("codexbar_build_info{schema_version=\"1\",version=\"0.56.8\"} 1\n"));
        assert!(body.contains("codexbar_provider_up{provider=\"codex\"} 1\n"));
        assert!(!body.contains("provider=\"claude\""));
        assert!(
            body.contains(
                "codexbar_quota_used_percent{provider=\"codex\",window=\"session\"} 25\n"
            )
        );
        assert!(body.contains(
            "codexbar_quota_remaining_percent{provider=\"codex\",window=\"weekly\"} 60\n"
        ));
        assert!(body.contains("codexbar_cost_last_30_days_usd{provider=\"codex\"} 12.5\n"));
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
    fn exports_only_fixed_codex_quota_windows() {
        let mut provider_usage = usage(RateWindow::no_active_session());
        provider_usage.secondary = Some(RateWindow::new(35.0));
        provider_usage.tertiary = Some(RateWindow::new(45.0));
        provider_usage.model_specific = Some(RateWindow::new(55.0));
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
        let quota = samples(&body, "codexbar_quota_used_percent").collect::<Vec<_>>();
        assert_eq!(quota.len(), 3);
        for window in ["weekly", "monthly", "code_review"] {
            assert!(body.contains(&format!("window=\"{window}\"")));
        }
        for excluded in [
            "window=\"session\"",
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
    fn fixed_labels_keep_metric_families_bounded_and_contiguous() {
        let mut codex_usage = usage(RateWindow::new(25.0));
        codex_usage.extra_rate_windows.push(NamedRateWindow::new(
            "dynamic-tenant-id",
            "Sensitive tenant label",
            RateWindow::new(90.0),
        ));
        let body = render_at(
            &metrics_snapshot(
                vec![
                    provider("codex", Ok(ProviderFetchResult::new(codex_usage, "cli"))),
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
        assert_eq!(samples(&body, "codexbar_provider_up").count(), 1);
        assert!(!body.contains("dynamic-tenant-id"));
        assert!(!body.contains("Sensitive tenant label"));
        assert!(!body.contains("co\\\"dex"));
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
    fn failed_or_disabled_codex_has_unambiguous_series() {
        let failed = render_at(
            &metrics_snapshot(
                vec![provider("codex", Err("private failure".to_string()))],
                HashMap::new(),
            ),
            at(2),
        )
        .unwrap();
        assert!(failed.contains("codexbar_provider_up{provider=\"codex\"} 0\n"));
        assert_eq!(samples(&failed, "codexbar_quota_used_percent").count(), 0);
        assert!(!failed.contains("private failure"));

        let disabled = render_at(&metrics_snapshot(Vec::new(), HashMap::new()), at(2)).unwrap();
        assert!(disabled.contains("codexbar_provider_up{provider=\"codex\"} 0\n"));
        assert!(disabled.contains("codexbar_up 1\n"));
    }

    #[tokio::test]
    async fn cold_scrape_returns_up_zero_without_waiting_for_collection() {
        let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
        let started_tx = Arc::new(Mutex::new(Some(started_tx)));
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let release_rx = Arc::new(Mutex::new(Some(release_rx)));
        let payload = artifacts(input(
            vec![provider(
                "codex",
                Ok(ProviderFetchResult::new(
                    usage(RateWindow::new(25.0)),
                    "cli",
                )),
            )],
            HashMap::new(),
        ));
        let build: SnapshotArtifactsBuildFn = Arc::new(move || {
            let started_tx = started_tx.clone();
            let release_rx = release_rx.clone();
            let payload = payload.clone();
            Box::pin(async move {
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
        release_tx
            .send(())
            .expect("background collection must still be waiting");
        state.coordinator.get().await.unwrap();

        let ready = response(&state);
        assert!(ready.contains("codexbar_up 1\n"));
        assert!(ready.contains("codexbar_quota_used_percent"));
    }
}
