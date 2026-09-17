//! Deterministic Prometheus series encoding.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Display;

use super::MetricsRenderError;
use super::definitions::METRIC_DEFINITIONS;

pub(in crate::cli::serve::metrics) struct MetricsWriter {
    samples: BTreeMap<String, Vec<String>>,
    series: BTreeSet<String>,
}

impl MetricsWriter {
    pub(in crate::cli::serve::metrics) fn new() -> Self {
        Self {
            samples: BTreeMap::new(),
            series: BTreeSet::new(),
        }
    }

    pub(in crate::cli::serve::metrics) fn sample(
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

    pub(in crate::cli::serve::metrics) fn sample_f64(
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

    pub(in crate::cli::serve::metrics) fn finish(self) -> String {
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
