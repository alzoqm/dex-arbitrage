use anyhow::Result;
use tracing_subscriber::{fmt, EnvFilter};

use crate::config::Settings;

pub fn init(settings: &Settings) -> Result<()> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let builder = fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_thread_ids(false)
        .with_thread_names(false)
        .with_file(false)
        .with_line_number(false);

    if settings.json_logs {
        builder.json().init();
    } else {
        builder.compact().init();
    }

    Ok(())
}
