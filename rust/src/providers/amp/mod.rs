//! Amp provider implementation
//!
//! Amp is Sourcegraph's AI coding assistant
//! Fetches usage data from Amp's local config or API

use async_trait::async_trait;
use chrono::Utc;
use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;

use crate::core::{
    FetchContext, Provider, ProviderError, ProviderFetchResult, ProviderId, ProviderMetadata,
    RateWindow, SourceMode, UsageSnapshot,
};

/// Amp provider (Sourcegraph)
pub struct AmpProvider {
    metadata: ProviderMetadata,
}

impl AmpProvider {
    pub fn new() -> Self {
        Self {
            metadata: ProviderMetadata {
                id: ProviderId::Amp,
                display_name: "Amp",
                session_label: "Usage",
                weekly_label: "Monthly",
                supports_opus: false,
                supports_credits: true,
                default_enabled: false,
                is_primary: false,
                dashboard_url: Some("https://ampcode.com/settings/usage"),
                status_page_url: Some("https://sourcegraphstatus.com"),
            },
        }
    }

    /// Get Amp config directory
    fn get_amp_config_path() -> Option<PathBuf> {
        #[cfg(target_os = "windows")]
        {
            dirs::config_dir().map(|p| p.join("amp"))
        }
        #[cfg(not(target_os = "windows"))]
        {
            dirs::home_dir().map(|p| p.join(".amp"))
        }
    }

    /// Get Sourcegraph/Cody config directory (Amp might use this)
    fn get_cody_config_path() -> Option<PathBuf> {
        #[cfg(target_os = "windows")]
        {
            dirs::config_dir().map(|p| p.join("sourcegraph-cody"))
        }
        #[cfg(not(target_os = "windows"))]
        {
            dirs::home_dir().map(|p| p.join(".sourcegraph"))
        }
    }

    /// Read Amp/Sourcegraph access token
    async fn read_access_token(&self, ctx: &FetchContext) -> Result<String, ProviderError> {
        if let Some(token) = access_token_from_context(ctx) {
            return Ok(token);
        }

        if let Some(token) = access_token_from_environment() {
            return Ok(token);
        }

        if let Some(token) = Self::read_local_config_token().await {
            return Ok(token);
        }

        Err(ProviderError::AuthRequired)
    }

    async fn read_local_config_token() -> Option<String> {
        let amp_token = read_access_token_config(Self::get_amp_config_path()).await;
        if amp_token.is_some() {
            return amp_token;
        }

        read_access_token_config(Self::get_cody_config_path()).await
    }

    /// Fetch usage via Sourcegraph API
    async fn fetch_via_web(&self, ctx: &FetchContext) -> Result<UsageSnapshot, ProviderError> {
        let token = self.read_access_token(ctx).await?;

        let client = crate::core::credentialed_http_client_builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| ProviderError::Other(e.to_string()))?;

        // Sourcegraph Cody usage API
        let resp = client
            .get("https://sourcegraph.com/.api/cody/current-user/usage")
            .header("Authorization", format!("token {}", token))
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(ProviderError::AuthRequired);
        }

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ProviderError::Parse(e.to_string()))?;

        self.parse_usage_response(&json)
    }

    fn parse_usage_response(
        &self,
        json: &serde_json::Value,
    ) -> Result<UsageSnapshot, ProviderError> {
        if let Some(display_text) = json
            .get("displayText")
            .or_else(|| json.get("display_text"))
            .or_else(|| {
                json.get("result")
                    .and_then(|result| result.get("displayText"))
            })
            .and_then(Value::as_str)
            && let Some(usage) = usage_snapshot_from_amp_display_text(display_text, Utc::now())
        {
            return Ok(usage);
        }

        // Parse Sourcegraph/Amp usage response
        let used = json
            .get("completionsUsed")
            .or_else(|| json.get("used"))
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);

        let limit = json
            .get("completionsLimit")
            .or_else(|| json.get("limit"))
            .and_then(|v| v.as_f64())
            .unwrap_or(500.0);

        let used_percent = if limit > 0.0 {
            (used / limit) * 100.0
        } else {
            0.0
        };

        let plan = json
            .get("plan")
            .or_else(|| json.get("tier"))
            .and_then(|v| v.as_str())
            .unwrap_or("Pro");

        let reset_time = json
            .get("resetAt")
            .or_else(|| json.get("periodEnd"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let primary_window = RateWindow::with_details(used_percent, None, None, reset_time);
        let usage = UsageSnapshot::new(primary_window).with_login_method(plan);

        Ok(usage)
    }

    fn find_amp_cli() -> Option<PathBuf> {
        which::which("amp").ok().filter(|path| path.exists())
    }

    async fn fetch_via_cli(&self) -> Result<UsageSnapshot, ProviderError> {
        let executable = Self::find_amp_cli().ok_or_else(|| {
            ProviderError::NotInstalled(
                "Amp CLI not found. Install it from https://ampcode.com".to_string(),
            )
        })?;

        let mut command = Command::new(executable);
        command
            .args(["usage"])
            .env("NO_COLOR", "1")
            .kill_on_drop(true);
        hide_windows_console(&mut command);
        let output = timeout(Duration::from_secs(15), command.output())
            .await
            .map_err(|_| ProviderError::Timeout)?
            .map_err(|error| ProviderError::Other(format!("Failed to run Amp CLI: {error}")))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let text = if stdout.trim().is_empty() {
            stderr.trim()
        } else {
            stdout.trim()
        };

        if !output.status.success() {
            let lowercase = text.to_ascii_lowercase();
            if lowercase.contains("login") || lowercase.contains("auth") {
                return Err(ProviderError::AuthRequired);
            }
            return Err(ProviderError::Other(format!("Amp CLI failed: {text}")));
        }
        usage_from_amp_cli_output(text, Utc::now())
    }
}

fn usage_from_amp_cli_output(
    text: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<UsageSnapshot, ProviderError> {
    usage_snapshot_from_amp_display_text(text, now).ok_or_else(|| {
        ProviderError::Parse("Amp CLI returned unrecognized usage output".to_string())
    })
}

#[cfg(windows)]
fn hide_windows_console(command: &mut Command) {
    command.creation_flags(0x08000000);
}

#[cfg(not(windows))]
fn hide_windows_console(command: &mut Command) {
    let _ = command;
}

fn access_token_from_context(ctx: &FetchContext) -> Option<String> {
    ctx.api_key
        .as_deref()
        .filter(|api_key| !api_key.is_empty())
        .map(str::to_string)
}

fn access_token_from_environment() -> Option<String> {
    std::env::var("SRC_ACCESS_TOKEN")
        .ok()
        .or_else(|| std::env::var("AMP_ACCESS_TOKEN").ok())
}

async fn read_access_token_config(config_dir: Option<PathBuf>) -> Option<String> {
    let config_file = config_dir?.join("config.json");
    if !config_file.exists() {
        return None;
    }

    let content = tokio::fs::read_to_string(config_file).await.ok()?;
    let json = serde_json::from_str::<serde_json::Value>(&content).ok()?;
    json.get("accessToken")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

impl Default for AmpProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Provider for AmpProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Amp
    }

    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }

    async fn fetch_usage(&self, ctx: &FetchContext) -> Result<ProviderFetchResult, ProviderError> {
        tracing::debug!("Fetching Amp usage");

        match ctx.source_mode {
            SourceMode::Auto => {
                if let Ok(usage) = self.fetch_via_cli().await {
                    return Ok(ProviderFetchResult::new(usage, "cli"));
                }
                let usage = self.fetch_via_web(ctx).await?;
                Ok(ProviderFetchResult::new(usage, "web"))
            }
            SourceMode::Web => {
                let usage = self.fetch_via_web(ctx).await?;
                Ok(ProviderFetchResult::new(usage, "web"))
            }
            SourceMode::Cli => {
                let usage = self.fetch_via_cli().await?;
                Ok(ProviderFetchResult::new(usage, "cli"))
            }
            SourceMode::OAuth => Err(ProviderError::UnsupportedSource(SourceMode::OAuth)),
        }
    }

    fn available_sources(&self) -> Vec<SourceMode> {
        vec![SourceMode::Auto, SourceMode::Web, SourceMode::Cli]
    }

    fn supports_web(&self) -> bool {
        true
    }

    fn supports_cli(&self) -> bool {
        true
    }
}

/// Monthly pace window sentinel used by Amp subscription/pace UI (30 days).
const AMP_MONTHLY_WINDOW_MINUTES: u32 = 30 * 24 * 60;

/// Parsed Amp subscription (legacy dual credits or current Tier allowances).
#[derive(Debug, Clone, PartialEq)]
pub struct AmpSubscriptionUsage {
    pub plan: String,
    pub reset_description: String,
    pub kind: AmpSubscriptionKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AmpSubscriptionKind {
    Legacy {
        other_used_percent: f64,
        orb_used_percent: f64,
        resets_at: chrono::DateTime<chrono::Utc>,
    },
    Tier {
        agent: AmpAllowance,
        orb: Option<AmpAllowance>,
        period_start: Option<chrono::DateTime<chrono::Utc>>,
        resets_at: Option<chrono::DateTime<chrono::Utc>>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct AmpAllowance {
    pub remaining: f64,
    pub limit: f64,
}

impl AmpAllowance {
    fn used_percent(&self) -> f64 {
        if self.limit > 0.0 {
            ((self.limit - self.remaining) / self.limit * 100.0).clamp(0.0, 100.0)
        } else {
            0.0
        }
    }
}

impl AmpSubscriptionUsage {
    pub fn other_used_percent(&self) -> f64 {
        match &self.kind {
            AmpSubscriptionKind::Legacy {
                other_used_percent, ..
            } => *other_used_percent,
            AmpSubscriptionKind::Tier { agent, .. } => agent.used_percent(),
        }
    }

    pub fn orb_used_percent(&self) -> Option<f64> {
        match &self.kind {
            AmpSubscriptionKind::Legacy {
                orb_used_percent, ..
            } => Some(*orb_used_percent),
            AmpSubscriptionKind::Tier { orb, .. } => orb.as_ref().map(AmpAllowance::used_percent),
        }
    }

    pub fn resets_at(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        match &self.kind {
            AmpSubscriptionKind::Legacy { resets_at, .. } => Some(*resets_at),
            AmpSubscriptionKind::Tier { resets_at, .. } => *resets_at,
        }
    }

    pub fn agent_remaining(&self) -> Option<f64> {
        match &self.kind {
            AmpSubscriptionKind::Legacy { .. } => None,
            AmpSubscriptionKind::Tier { agent, .. } => Some(agent.remaining),
        }
    }

    pub fn agent_limit(&self) -> Option<f64> {
        match &self.kind {
            AmpSubscriptionKind::Legacy { .. } => None,
            AmpSubscriptionKind::Tier { agent, .. } => Some(agent.limit),
        }
    }

    pub fn period_start(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        match &self.kind {
            AmpSubscriptionKind::Legacy { .. } => None,
            AmpSubscriptionKind::Tier { period_start, .. } => *period_start,
        }
    }

    pub fn orb_hours_remaining(&self) -> Option<f64> {
        match &self.kind {
            AmpSubscriptionKind::Legacy { .. } => None,
            AmpSubscriptionKind::Tier { orb, .. } => {
                orb.as_ref().map(|allowance| allowance.remaining)
            }
        }
    }

    pub fn orb_hours_limit(&self) -> Option<f64> {
        match &self.kind {
            AmpSubscriptionKind::Legacy { .. } => None,
            AmpSubscriptionKind::Tier { orb, .. } => orb.as_ref().map(|allowance| allowance.limit),
        }
    }
}

/// Parse Amp Free percentage lines from CLI/display text (upstream 0.42.1+ shape).
///
/// Matches lines like:
/// - `Amp Free: 72% remaining today`
/// - `Amp Free: 72% remaining (resets daily)`
///
/// Returns **used** percent (100 - remaining). The CLI fetch path passes its
/// `amp usage` output through this parser before falling back to the API path.
pub fn parse_amp_free_percent_remaining(text: &str) -> Option<f64> {
    let text = text.replace("**", "");
    for line in text.lines() {
        let line = line.trim();
        let lower = line.to_ascii_lowercase();
        if !lower.starts_with("amp free:") {
            continue;
        }
        let rest = line["amp free:".len()..].trim();
        // Prefer percentage form over dollar `$used / $quota remaining`.
        let Some(percent_idx) = rest.find('%') else {
            continue;
        };
        let number_part = rest[..percent_idx].trim();
        // Reject dollar amounts mistaken for percentages (e.g. "$12 remaining").
        if number_part.contains('$') {
            continue;
        }
        let after = rest[percent_idx + 1..].trim().to_ascii_lowercase();
        if !after.starts_with("remaining") {
            continue;
        }
        let remaining: f64 = number_part.replace(',', "").parse().ok()?;
        if !remaining.is_finite() {
            continue;
        }
        let clamped = remaining.clamp(0.0, 100.0);
        return Some(100.0 - clamped);
    }
    None
}

fn normalize_amp_subscription_line(line: &str) -> String {
    let trimmed = line.trim();
    let Some(rest) = trimmed.strip_prefix("Amp ") else {
        return line.to_string();
    };
    let Some((plan, suffix)) = rest.split_once(" Subscription:") else {
        return line.to_string();
    };
    let plan = plan.trim();
    if plan.is_empty() {
        return line.to_string();
    }
    format!("Subscription {plan}:{suffix}")
}

/// Parse Amp subscription display text (Megawatt/Gigawatt dual other/orb windows).
///
/// Matches:
/// `Subscription Megawatt: 42% other usage and 88% orb usage remaining - resets upon renewal in 12 days`
/// `Subscription Gigawatt: 10% other usage and 95% orb usage remaining - resets upon renewal in 2 months`
///
/// Upstream 0.49.6 #2601: monthly (Gigawatt) renewals advance by calendar
/// month, not 30-day buckets.
pub fn parse_amp_subscription_usage(
    text: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<AmpSubscriptionUsage> {
    let text = text.replace("**", "");

    // Current Amp output reports the agent allowance in dollars and the Orb
    // allowance in hours. The displayed percentages are rounded, so derive
    // both usage lanes from their exact remaining/limit values instead.
    let tier_re = regex_lite::Regex::new(
        r"(?im)^\s*Amp\s+(.+?)\s+Tier:\s*agent\s+usage\s+\$([0-9][0-9,]*(?:\.[0-9]+)?)\s+of\s+\$([0-9][0-9,]*(?:\.[0-9]+)?)\s+remaining\b(.*?)resets\s+upon\s+renewal\s+in\s+([0-9][0-9,]*)\s+(days?|months?)\b",
    )
    .ok()?;
    let orb_re = regex_lite::Regex::new(
        r"(?i)\borb\s+usage\s+([0-9][0-9,]*(?:\.[0-9]+)?)h\s+of\s+([0-9][0-9,]*(?:\.[0-9]+)?)h\s+a1\.small\s+orb\s+hours\s+remaining\b",
    )
    .ok()?;

    for line in text.lines() {
        let Some(caps) = tier_re.captures(line) else {
            continue;
        };
        let plan = caps.get(1)?.as_str().trim();
        let agent_remaining = parse_amp_number(caps.get(2)?.as_str())?;
        let agent_limit = parse_amp_number(caps.get(3)?.as_str())?;
        let details = caps.get(4)?.as_str();
        let renewal_value: i64 = caps.get(5)?.as_str().replace(',', "").parse().ok()?;
        let renewal_unit = caps.get(6)?.as_str().to_ascii_lowercase();
        let reset_description = amp_renewal_description(renewal_value, &renewal_unit);
        let (period_start, resets_at) = parse_amp_tier_period(details).unzip();
        let orb = orb_re.captures(details).and_then(|orb_caps| {
            let remaining = parse_amp_number(orb_caps.get(1)?.as_str())?;
            let limit = parse_amp_number(orb_caps.get(2)?.as_str())?;
            (limit > 0.0).then_some(AmpAllowance { remaining, limit })
        });
        return Some(AmpSubscriptionUsage {
            plan: plan.to_string(),
            reset_description,
            kind: AmpSubscriptionKind::Tier {
                agent: AmpAllowance {
                    remaining: agent_remaining,
                    limit: agent_limit,
                },
                orb,
                period_start,
                resets_at,
            },
        });
    }

    let re = regex_lite::Regex::new(
        r"(?im)^\s*Subscription\s+(.+?):\s*([0-9][0-9,]*(?:\.[0-9]+)?)\s*%\s+other\s+usage\s+and\s+([0-9][0-9,]*(?:\.[0-9]+)?)\s*%\s+orb\s+usage\s+remaining\s*-\s*resets\s+upon\s+renewal\s+in\s+([0-9][0-9,]*)\s+(days?|months?)(?:\s+-\s+https?://\S+)?\s*$",
    )
    .ok()?;

    for line in text.lines() {
        let normalized_line = normalize_amp_subscription_line(line);
        let Some(caps) = re.captures(&normalized_line) else {
            continue;
        };
        let plan = caps.get(1)?.as_str().trim();
        if plan.is_empty() {
            continue;
        }
        let other_remaining = parse_amp_number(caps.get(2)?.as_str())?;
        let orb_remaining = parse_amp_number(caps.get(3)?.as_str())?;
        let renewal_value: i64 = caps.get(4)?.as_str().replace(',', "").parse().ok()?;
        if renewal_value < 0 {
            continue;
        }
        let unit = caps.get(5)?.as_str().to_ascii_lowercase();
        let resets_at = if unit.starts_with("month") {
            add_calendar_months(now, renewal_value)?
        } else {
            now + chrono::Duration::days(renewal_value)
        };
        let reset_description = amp_renewal_description(renewal_value, &unit);
        return Some(AmpSubscriptionUsage {
            plan: plan.to_string(),
            reset_description,
            kind: AmpSubscriptionKind::Legacy {
                other_used_percent: 100.0 - other_remaining.clamp(0.0, 100.0),
                orb_used_percent: 100.0 - orb_remaining.clamp(0.0, 100.0),
                resets_at,
            },
        });
    }
    None
}

fn amp_renewal_description(value: i64, unit: &str) -> String {
    let singular = if unit.starts_with("month") {
        "month"
    } else {
        "day"
    };
    if value == 1 {
        format!("renews in 1 {singular}")
    } else {
        format!("renews in {value} {singular}s")
    }
}

fn parse_amp_tier_period(
    text: &str,
) -> Option<(chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>)> {
    use chrono::NaiveDate;

    let re =
        regex_lite::Regex::new(r"(?i)\bperiod\s+(\d{4}-\d{2}-\d{2})\s+to\s+(\d{4}-\d{2}-\d{2})\b")
            .ok()?;
    let caps = re.captures(text)?;
    let start = NaiveDate::parse_from_str(caps.get(1)?.as_str(), "%Y-%m-%d")
        .ok()?
        .and_hms_opt(0, 0, 0)?
        .and_utc();
    let end = NaiveDate::parse_from_str(caps.get(2)?.as_str(), "%Y-%m-%d")
        .ok()?
        .and_hms_opt(0, 0, 0)?
        .and_utc();
    (end > start).then_some((start, end))
}

/// Add whole calendar months via chrono's calendar arithmetic, mirroring
/// upstream `Calendar.date(byAdding: .month:)` for monthly renewals.
fn add_calendar_months(
    now: chrono::DateTime<chrono::Utc>,
    months: i64,
) -> Option<chrono::DateTime<chrono::Utc>> {
    now.checked_add_months(chrono::Months::new(u32::try_from(months).ok()?))
}

/// Build a [`UsageSnapshot`] from Amp Free / subscription display text.
///
/// Subscription (Megawatt) wins for primary/secondary windows when present:
/// - primary = other usage
/// - secondary = orb usage
///
/// Free percent path fills primary when there is no subscription match.
pub fn usage_snapshot_from_amp_display_text(
    text: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<UsageSnapshot> {
    if let Some(sub) = parse_amp_subscription_usage(text, now) {
        return Some(usage_snapshot_from_subscription(sub));
    }

    let free_used = parse_amp_free_percent_remaining(text)?;
    // Upstream 0.49.6 #2601: the Amp Free daily tier resets at 8:00 PM
    // America/New_York, not local midnight.
    let primary = RateWindow::with_details(
        free_used,
        Some(24 * 60),
        next_free_tier_reset(now),
        Some("resets daily".to_string()),
    );
    Some(UsageSnapshot::new(primary).with_login_method("Amp Free"))
}

fn usage_snapshot_from_subscription(sub: AmpSubscriptionUsage) -> UsageSnapshot {
    let AmpSubscriptionUsage {
        plan,
        reset_description,
        kind,
    } = sub;

    match kind {
        AmpSubscriptionKind::Legacy {
            other_used_percent,
            orb_used_percent,
            resets_at,
        } => {
            let window_minutes = RateWindow::monthly_window_minutes(Some(resets_at))
                .or(Some(AMP_MONTHLY_WINDOW_MINUTES));
            let primary = RateWindow::with_details(
                other_used_percent,
                window_minutes,
                Some(resets_at),
                Some(reset_description.clone()),
            );
            let secondary = RateWindow::with_details(
                orb_used_percent,
                window_minutes,
                Some(resets_at),
                Some(reset_description),
            );
            UsageSnapshot::new(primary)
                .with_secondary(secondary)
                .with_login_method(plan)
        }
        AmpSubscriptionKind::Tier {
            agent,
            orb,
            period_start,
            resets_at,
        } => {
            let window_minutes = match (period_start, resets_at) {
                (Some(start), Some(end)) => u32::try_from((end - start).num_minutes())
                    .ok()
                    .filter(|minutes| *minutes > 0),
                _ => None,
            };
            let primary = if agent.limit <= 0.0 {
                RateWindow::informational("No active Amp tier allowance")
            } else {
                RateWindow::with_details(
                    agent.used_percent(),
                    window_minutes,
                    resets_at,
                    Some(tier_allowance_description(
                        &reset_description,
                        &agent,
                        "dollars",
                    )),
                )
            };
            let mut usage = UsageSnapshot::new(primary)
                .with_login_method(plan)
                .with_primary_label("Agent usage");
            if let Some(orb) = orb {
                let secondary = RateWindow::with_details(
                    orb.used_percent(),
                    window_minutes,
                    resets_at,
                    Some(tier_allowance_description(
                        &reset_description,
                        &orb,
                        "hours",
                    )),
                );
                usage = usage
                    .with_secondary(secondary)
                    .with_secondary_label("Orb usage");
            }
            usage
        }
    }
}

fn tier_allowance_description(
    reset_description: &str,
    allowance: &AmpAllowance,
    unit: &str,
) -> String {
    match unit {
        "hours" => format!(
            "{reset_description} · {:.2}h/{:.2}h remaining",
            allowance.remaining, allowance.limit
        ),
        _ => format!(
            "{reset_description} · ${:.2}/${:.2} remaining",
            allowance.remaining, allowance.limit
        ),
    }
}

/// Next 8:00 PM America/New_York boundary strictly after `now`.
fn next_free_tier_reset(
    now: chrono::DateTime<chrono::Utc>,
) -> Option<chrono::DateTime<chrono::Utc>> {
    use chrono::{Datelike, TimeZone};
    let tz = chrono_tz::America::New_York;
    let local_now = now.with_timezone(&tz);
    let today = local_now.date_naive();
    let today_reset = tz
        .with_ymd_and_hms(today.year(), today.month(), today.day(), 20, 0, 0)
        .single()?
        .with_timezone(&chrono::Utc);
    if today_reset > now {
        return Some(today_reset);
    }
    let tomorrow = today + chrono::Duration::days(1);
    tz.with_ymd_and_hms(tomorrow.year(), tomorrow.month(), tomorrow.day(), 20, 0, 0)
        .single()
        .map(|dt| dt.with_timezone(&chrono::Utc))
}

fn parse_amp_number(raw: &str) -> Option<f64> {
    let value: f64 = raw.replace(',', "").parse().ok()?;
    value.is_finite().then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    #[test]
    fn dashboard_points_to_current_usage_page() {
        assert_eq!(
            AmpProvider::new().metadata().dashboard_url,
            Some("https://ampcode.com/settings/usage")
        );
    }

    #[test]
    fn parses_amp_free_percent_remaining_today() {
        let text = "Signed in as user@example.com\nAmp Free: 72% remaining today\n";
        assert_eq!(parse_amp_free_percent_remaining(text), Some(28.0));
    }

    #[test]
    fn parses_amp_free_percent_resets_daily() {
        let text = "Amp Free: 100% remaining (resets daily)";
        assert_eq!(parse_amp_free_percent_remaining(text), Some(0.0));
    }

    #[test]
    fn parses_bold_amp_free_and_current_subscription_labels() {
        let now = Utc.with_ymd_and_hms(2026, 8, 24, 12, 0, 0).unwrap();
        assert_eq!(
            parse_amp_free_percent_remaining("**Amp Free:** 0% remaining today (resets daily)"),
            Some(100.0)
        );

        let sub = parse_amp_subscription_usage(
            "**Amp Megawatt Subscription:** 68% other usage and 97% orb usage remaining - resets upon renewal in 5 days",
            now,
        )
        .expect("bold subscription");
        assert_eq!(sub.plan, "Megawatt");
        assert!((sub.other_used_percent() - 32.0).abs() < f64::EPSILON);
        assert!((sub.orb_used_percent().unwrap() - 3.0).abs() < f64::EPSILON);
        assert_eq!(sub.resets_at(), Some(now + chrono::Duration::days(5)));
    }

    #[test]
    fn ignores_dollar_remaining_form() {
        let text = "Amp Free: $4.20 / $10 remaining (replenishes +$1 / hour)";
        assert_eq!(parse_amp_free_percent_remaining(text), None);
    }

    #[test]
    fn returns_none_when_amp_free_missing() {
        assert_eq!(
            parse_amp_free_percent_remaining("Individual credits: $3 remaining"),
            None
        );
    }

    #[test]
    fn parses_megawatt_subscription_dual_windows() {
        let now = Utc.with_ymd_and_hms(2026, 7, 1, 12, 0, 0).unwrap();
        let text = "Signed in as user@example.com (Acme)\n\
Subscription Megawatt: 42% other usage and 88% orb usage remaining - resets upon renewal in 12 days\n";
        let sub = parse_amp_subscription_usage(text, now).expect("subscription");
        assert_eq!(sub.plan, "Megawatt");
        assert!((sub.other_used_percent() - 58.0).abs() < f64::EPSILON);
        assert!((sub.orb_used_percent().unwrap() - 12.0).abs() < f64::EPSILON);
        assert_eq!(sub.reset_description, "renews in 12 days");
        assert_eq!(sub.resets_at(), Some(now + chrono::Duration::days(12)));

        let snapshot = usage_snapshot_from_amp_display_text(text, now).expect("snapshot");
        assert!((snapshot.primary.used_percent - 58.0).abs() < f64::EPSILON);
        assert_eq!(
            snapshot.primary.window_minutes,
            RateWindow::monthly_window_minutes(snapshot.primary.resets_at)
                .or(Some(AMP_MONTHLY_WINDOW_MINUTES))
        );
        assert_eq!(
            snapshot.primary.reset_description.as_deref(),
            Some("renews in 12 days")
        );
        let secondary = snapshot.secondary.expect("orb secondary");
        assert!((secondary.used_percent - 12.0).abs() < f64::EPSILON);
        assert_eq!(
            secondary.window_minutes,
            RateWindow::monthly_window_minutes(secondary.resets_at)
                .or(Some(AMP_MONTHLY_WINDOW_MINUTES))
        );
        assert_eq!(snapshot.login_method.as_deref(), Some("Megawatt"));
    }

    #[test]
    fn megawatt_one_day_renewal_wording() {
        let now = Utc.with_ymd_and_hms(2026, 7, 1, 0, 0, 0).unwrap();
        let text = "Subscription Megawatt: 0% other usage and 100% orb usage remaining - resets upon renewal in 1 day";
        let sub = parse_amp_subscription_usage(text, now).unwrap();
        assert_eq!(sub.reset_description, "renews in 1 day");
        assert!((sub.other_used_percent() - 100.0).abs() < f64::EPSILON);
        assert!((sub.orb_used_percent().unwrap() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn gigawatt_monthly_renewal_advances_calendar_months() {
        // Upstream 0.49.6 #2601: monthly renewals (Gigawatt) use calendar
        // months, not 30-day buckets.
        let now = Utc.with_ymd_and_hms(2026, 8, 17, 12, 0, 0).unwrap();
        let text = "Subscription Gigawatt: 10% other usage and 95% orb usage remaining - resets upon renewal in 2 months";
        let sub = parse_amp_subscription_usage(text, now).expect("subscription");
        assert_eq!(sub.plan, "Gigawatt");
        assert_eq!(sub.reset_description, "renews in 2 months");
        assert_eq!(
            sub.resets_at(),
            Some(Utc.with_ymd_and_hms(2026, 10, 17, 12, 0, 0).unwrap())
        );
    }

    #[test]
    fn free_tier_resets_at_8pm_new_york() {
        // Upstream 0.49.6 #2601: Amp Free resets at 8:00 PM America/New_York.
        // 2026-08-17 18:00 UTC = 14:00 EDT → same-day 20:00 EDT = 00:00 UTC Aug 18.
        let now = Utc.with_ymd_and_hms(2026, 8, 17, 18, 0, 0).unwrap();
        let snapshot =
            usage_snapshot_from_amp_display_text("Amp Free: 72% remaining (resets daily)", now)
                .expect("snapshot");
        assert_eq!(
            snapshot.primary.resets_at,
            Some(Utc.with_ymd_and_hms(2026, 8, 18, 0, 0, 0).unwrap())
        );

        // 2026-08-18 00:30 UTC = 20:30 EDT Aug 17 (after the boundary) → the
        // next reset is Aug 18 20:00 EDT = Aug 19 00:00 UTC.
        let later = Utc.with_ymd_and_hms(2026, 8, 18, 0, 30, 0).unwrap();
        let snapshot =
            usage_snapshot_from_amp_display_text("Amp Free: 72% remaining (resets daily)", later)
                .expect("snapshot");
        assert_eq!(
            snapshot.primary.resets_at,
            Some(Utc.with_ymd_and_hms(2026, 8, 19, 0, 0, 0).unwrap())
        );
    }

    #[test]
    fn free_path_still_builds_snapshot() {
        let now = Utc.with_ymd_and_hms(2026, 7, 1, 0, 0, 0).unwrap();
        let text = "Amp Free: 72% remaining today";
        let snapshot = usage_snapshot_from_amp_display_text(text, now).unwrap();
        assert!((snapshot.primary.used_percent - 28.0).abs() < f64::EPSILON);
        assert!(snapshot.secondary.is_none());
        assert_eq!(snapshot.login_method.as_deref(), Some("Amp Free"));
    }

    #[test]
    fn provider_cli_boundary_projects_tier_output_into_usage_snapshot() {
        let now = Utc.with_ymd_and_hms(2026, 9, 16, 12, 0, 0).unwrap();
        let text = "Amp Example Tier: agent usage $18.57 of $20 remaining, \
orb usage 732.8h of 750h a1.small orb hours remaining - \
period 2026-09-13 to 2026-10-13, resets upon renewal in 27 days";

        let usage = usage_from_amp_cli_output(text, now).expect("tier CLI output");
        assert_eq!(usage.primary_label.as_deref(), Some("Agent usage"));
        assert_eq!(usage.secondary_label.as_deref(), Some("Orb usage"));
        assert!((usage.primary.used_percent - 7.15).abs() < 0.0001);
        assert!((usage.secondary.expect("orb").used_percent - 2.2933333333).abs() < 0.0001);
    }
}

#[cfg(test)]
mod current_subscription_tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    #[test]
    fn parses_current_amp_subscription_line_format() {
        let now = Utc.with_ymd_and_hms(2026, 8, 18, 12, 0, 0).unwrap();
        let text = "Signed in as user@example.com\nAmp Megawatt Subscription: 100% other usage and 100% orb usage remaining - resets upon renewal in 1 month\n";
        let sub = parse_amp_subscription_usage(text, now).expect("subscription");

        assert_eq!(sub.plan, "Megawatt");
        assert!((sub.other_used_percent() - 0.0).abs() < f64::EPSILON);
        assert!((sub.orb_used_percent().unwrap() - 0.0).abs() < f64::EPSILON);
        assert_eq!(
            sub.resets_at(),
            Some(Utc.with_ymd_and_hms(2026, 9, 18, 12, 0, 0).unwrap())
        );
        assert_eq!(sub.reset_description, "renews in 1 month");
    }

    #[test]
    fn parses_tier_allowances_from_exact_balances_and_period() {
        let now = Utc.with_ymd_and_hms(2026, 9, 16, 12, 0, 0).unwrap();
        let text = "Amp Megawatt Tier: agent usage $18.57 of $20 remaining (93%), \
orb usage 732.8h of 750h a1.small orb hours remaining (98%) - \
period 2026-09-13 to 2026-10-13, resets upon renewal in 27 days";

        let sub = parse_amp_subscription_usage(text, now).expect("tier");
        assert_eq!(sub.plan, "Megawatt");
        assert!((sub.other_used_percent() - 7.15).abs() < 0.0001);
        assert!((sub.orb_used_percent().unwrap() - 2.2933333333).abs() < 0.0001);
        assert_eq!(sub.agent_remaining(), Some(18.57));
        assert_eq!(sub.agent_limit(), Some(20.0));
        assert_eq!(sub.orb_hours_remaining(), Some(732.8));
        assert_eq!(sub.orb_hours_limit(), Some(750.0));
        assert_eq!(
            sub.resets_at(),
            Some(Utc.with_ymd_and_hms(2026, 10, 13, 0, 0, 0).unwrap())
        );

        let snapshot = usage_snapshot_from_amp_display_text(text, now).expect("snapshot");
        assert!((snapshot.primary.used_percent - 7.15).abs() < 0.0001);
        assert_eq!(snapshot.primary.window_minutes, Some(30 * 24 * 60));
        assert_eq!(snapshot.primary_label.as_deref(), Some("Agent usage"));
        let secondary = snapshot.secondary.expect("orb");
        assert!((secondary.used_percent - 2.2933333333).abs() < 0.0001);
        assert_eq!(snapshot.secondary_label.as_deref(), Some("Orb usage"));
    }

    #[test]
    fn invalid_tier_period_does_not_invent_reset_window() {
        let now = Utc.with_ymd_and_hms(2026, 9, 16, 12, 0, 0).unwrap();
        let text = "Amp Example Tier: agent usage $18 of $20 remaining - \
period 2026-02-30 to 2026-03-30, resets upon renewal in 27 days";
        let sub = parse_amp_subscription_usage(text, now).expect("tier");
        assert!(sub.period_start().is_none());
        assert!(sub.resets_at().is_none());

        let snapshot = usage_snapshot_from_amp_display_text(text, now).expect("snapshot");
        assert_eq!(snapshot.primary.used_percent, 10.0);
        assert!(snapshot.primary.window_minutes.is_none());
        assert!(snapshot.primary.resets_at.is_none());
        assert_eq!(
            snapshot.primary.reset_description.as_deref(),
            Some("renews in 27 days · $18.00/$20.00 remaining")
        );
    }

    #[test]
    fn tier_without_orb_keeps_agent_lane() {
        let now = Utc.with_ymd_and_hms(2026, 9, 16, 12, 0, 0).unwrap();
        let text = "Amp Example Tier: agent usage $3 of $20 remaining - \
period 2026-09-13 to 2026-10-13, resets upon renewal in 27 days";
        let snapshot = usage_snapshot_from_amp_display_text(text, now).expect("snapshot");
        assert!(snapshot.secondary.is_none());
        assert_eq!(snapshot.primary.used_percent, 85.0);
    }
}
