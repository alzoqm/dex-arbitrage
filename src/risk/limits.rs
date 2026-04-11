use std::{collections::HashMap, sync::atomic::{AtomicUsize, Ordering}};

use anyhow::{Context, Result};
use parking_lot::Mutex;
use tracing::{info, warn};

use crate::{
    config::Settings,
    types::{ExecutablePlan, SubmissionResult},
};

#[derive(Debug)]
pub struct RiskManager {
    max_position: u128,
    max_flash_loan: u128,
    min_profit: i128,
    max_concurrent_tx: usize,
    daily_loss_limit: i128,
    open_tx_count: AtomicUsize,
    daily_pnl: Mutex<HashMap<String, i128>>,
}

impl RiskManager {
    pub fn new(settings: &Settings) -> Self {
        Self {
            max_position: settings.risk.max_position,
            max_flash_loan: settings.risk.max_flash_loan,
            min_profit: settings.risk.min_net_profit,
            max_concurrent_tx: settings.risk.max_concurrent_tx,
            daily_loss_limit: settings.risk.daily_loss_limit,
            open_tx_count: AtomicUsize::new(0),
            daily_pnl: Mutex::new(HashMap::new()),
        }
    }

    pub fn pre_trade_check(&self, plan: &ExecutablePlan) -> Result<()> {
        if plan.exact.input_amount > self.max_position {
            anyhow::bail!("input amount exceeds max position");
        }
        if matches!(plan.exact.capital_source, crate::types::CapitalSource::FlashLoan)
            && plan.exact.input_amount > self.max_flash_loan
        {
            anyhow::bail!("flash loan amount exceeds max flash loan limit");
        }
        if plan.exact.expected_profit < self.min_profit {
            anyhow::bail!("expected profit below threshold");
        }
        if self.open_tx_count.load(Ordering::Relaxed) >= self.max_concurrent_tx {
            anyhow::bail!("max concurrent tx reached");
        }
        let day_key = current_day_key();
        let pnl = *self.daily_pnl.lock().get(&day_key).unwrap_or(&0);
        if pnl <= self.daily_loss_limit {
            anyhow::bail!("daily loss limit reached");
        }
        Ok(())
    }

    pub fn mark_submitted(&self) {
        self.open_tx_count.fetch_add(1, Ordering::SeqCst);
    }

    pub fn mark_finalized(&self, realized_profit: i128) {
        self.open_tx_count.fetch_sub(1, Ordering::SeqCst);
        let day_key = current_day_key();
        let mut daily = self.daily_pnl.lock();
        *daily.entry(day_key).or_default() += realized_profit;
    }

    pub fn mark_failed_submission(&self) {
        let previous = self.open_tx_count.fetch_sub(1, Ordering::SeqCst);
        if previous == 0 {
            warn!("risk manager open tx count underflow prevented");
            self.open_tx_count.store(0, Ordering::SeqCst);
        }
    }

    pub fn on_submission_result(&self, result: &SubmissionResult) {
        info!(tx_hash = %result.tx_hash, channel = %result.channel, "submission accepted");
    }
}

fn current_day_key() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("clock before unix epoch")
        .unwrap_or_default()
        .as_secs();
    format!("{}", now / 86_400)
}
