mod compressor;
mod config;
mod server;
mod state;
mod zram;

use anyhow::Result;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let config = config::Config::load()?;
    info!("bssl-ram starting — idle threshold: {}s", config.idle_threshold_secs);

    zram::ensure_zram_swap(&config)?;

    server::run(config).await
}
