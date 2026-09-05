//! TOML configuration (thin adapter at the config-file seam).

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// MAC address of the scale; required for everything except `find`.
    pub scale_mac: Option<String>,
    pub database: Option<DatabaseConfig>,
    #[serde(default)]
    pub listen: ListenConfig,
    #[serde(default)]
    pub clock_sync: ClockSyncConfig,
    #[serde(default)]
    pub metrics: MetricsConfig,
    #[serde(default)]
    pub history: crate::history::HistoryConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DatabaseConfig {
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ListenConfig {
    #[serde(default = "default_spool_path")]
    pub spool_path: PathBuf,
    #[serde(default = "default_spool_max_bytes")]
    pub spool_max_bytes: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClockSyncConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_drift_threshold")]
    pub drift_threshold_secs: i64,
}

/// Which derived calculations are stored alongside a measurement. The
/// representation is the meaning (`MetricsPolicy` in `profile`): no
/// conversion layer, and capture still computes transient metrics for the
/// history tie-break even when storage is trimmed.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct MetricsConfig {
    /// `all` (default) stores all 12 metric columns, `weight-only` stores
    /// only the 4 weight-only ones, `none` stores no metrics.
    #[serde(default)]
    pub store: crate::profile::MetricsPolicy,
}

fn default_spool_path() -> PathBuf {
    PathBuf::from("/var/lib/grammatic/spool.hex")
}

fn default_spool_max_bytes() -> u64 {
    5_242_880
}

fn default_true() -> bool {
    true
}

fn default_drift_threshold() -> i64 {
    120
}

impl Default for ListenConfig {
    fn default() -> Self {
        ListenConfig {
            spool_path: default_spool_path(),
            spool_max_bytes: default_spool_max_bytes(),
        }
    }
}

impl Default for ClockSyncConfig {
    fn default() -> Self {
        ClockSyncConfig {
            enabled: default_true(),
            drift_threshold_secs: default_drift_threshold(),
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Config> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let config: Config =
            toml::from_str(&raw).with_context(|| format!("parsing config {}", path.display()))?;
        Ok(config)
    }

    /// Normalized (lowercase) scale MAC, or an error for commands that need it.
    pub fn scale_mac(&self) -> anyhow::Result<String> {
        self.scale_mac
            .as_deref()
            .map(str::trim)
            .filter(|mac| !mac.is_empty())
            .map(str::to_lowercase)
            .context("scale_mac not set in config; run 'grammatic find' first and edit the config")
    }

    pub fn database_url(&self) -> anyhow::Result<String> {
        self.database
            .as_ref()
            .map(|db| db.url.trim().to_string())
            .filter(|url| !url.is_empty())
            .context("[database] url not set in config")
    }
}
