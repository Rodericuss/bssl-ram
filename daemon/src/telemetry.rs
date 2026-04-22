use std::sync::atomic::{AtomicU64, Ordering};
use tracing::info;
use tracing_subscriber::EnvFilter;

/// Runtime counters. All operations are lock-free (AtomicU64) so the
/// scan loop can increment them without synchronisation overhead, and the
/// periodic stats dump can read them concurrently.
#[derive(Default)]
pub struct Stats {
    pub scans: AtomicU64,
    pub targets_seen: AtomicU64,
    pub compressions: AtomicU64,
    pub bytes_paged_out: AtomicU64,
    pub bytes_skipped_by_kernel: AtomicU64,
    pub skips_warmup: AtomicU64,
    pub skips_active: AtomicU64,
    pub skips_already_compressed: AtomicU64,
    pub skips_low_rss: AtomicU64,
    pub errors: AtomicU64,
}

impl Stats {
    pub fn inc(&self, counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub fn add(&self, counter: &AtomicU64, n: u64) {
        counter.fetch_add(n, Ordering::Relaxed);
    }

    /// Emit a cumulative-stats line. Called periodically from main so
    /// long-running instances always have a grep-friendly scoreboard in
    /// the journal without needing a scraping endpoint.
    pub fn emit(&self) {
        let scans = self.scans.load(Ordering::Relaxed);
        let targets = self.targets_seen.load(Ordering::Relaxed);
        let compressions = self.compressions.load(Ordering::Relaxed);
        let bytes_mib = self.bytes_paged_out.load(Ordering::Relaxed) / 1024 / 1024;
        let bytes_skipped_mib = self.bytes_skipped_by_kernel.load(Ordering::Relaxed) / 1024 / 1024;
        let skip_warmup = self.skips_warmup.load(Ordering::Relaxed);
        let skip_active = self.skips_active.load(Ordering::Relaxed);
        let skip_already = self.skips_already_compressed.load(Ordering::Relaxed);
        let skip_low_rss = self.skips_low_rss.load(Ordering::Relaxed);
        let errors = self.errors.load(Ordering::Relaxed);

        info!(
            scans,
            targets_seen = targets,
            compressions,
            bytes_paged_out_mib = bytes_mib,
            bytes_skipped_mib,
            skip_warmup,
            skip_active,
            skip_already_compressed = skip_already,
            skip_low_rss,
            errors,
            "bssl-ram stats snapshot"
        );
    }
}

/// Initialise tracing-subscriber with sensible defaults and two knobs:
///
/// - `RUST_LOG` env var — standard tracing EnvFilter syntax, e.g.
///   `info`, `bssl_ram=debug`, `bssl_ram::scanner=trace,info`.
/// - `BSSL_LOG_FORMAT` env var — `pretty` (default, human) | `compact` |
///   `json` (for log aggregators, single-line key=value).
pub fn init() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,bssl_ram=info"));

    let format = std::env::var("BSSL_LOG_FORMAT").unwrap_or_else(|_| "pretty".into());

    match format.as_str() {
        "json" => tracing_subscriber::fmt()
            .json()
            .with_env_filter(filter)
            .with_current_span(true)
            .with_span_list(false)
            .with_target(true)
            .init(),
        "compact" => tracing_subscriber::fmt()
            .compact()
            .with_env_filter(filter)
            .with_target(false)
            .init(),
        _ => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(false)
            .init(),
    }
}
