use std::{
    collections::HashMap,
    sync::atomic::{AtomicUsize, Ordering},
};

use anyhow::{Context, Result};
use parking_lot::Mutex;
use tracing::{info, warn};

use crate::{
    config::Settings,
    types::{ExecutablePlan, SubmissionResult},
};

#[derive(Debug)]
pub struct RiskManager {
    // USD-denominated limits
    max_position_usd_e8: u128,
    max_flash_loan_usd_e8: u128,
    min_profit_usd_e8: u128,
    daily_loss_limit_usd_e8: u128,

    max_concurrent_tx: usize,
    open_tx_count: AtomicUsize,
    daily_pnl_usd_e8: Mutex<HashMap<String, i128>>,
}

impl RiskManager {
    pub fn new(settings: &Settings) -> Self {
        Self {
            max_position_usd_e8: settings.risk.max_position_usd_e8,
            max_flash_loan_usd_e8: settings.risk.max_flash_loan_usd_e8,
            min_profit_usd_e8: settings.risk.min_net_profit_usd_e8,
            daily_loss_limit_usd_e8: settings.risk.daily_loss_limit_usd_e8,
            max_concurrent_tx: settings.risk.max_concurrent_tx,
            open_tx_count: AtomicUsize::new(0),
            daily_pnl_usd_e8: Mutex::new(HashMap::new()),
        }
    }

    pub fn pre_trade_check(&self, plan: &ExecutablePlan) -> Result<()> {
        // Check USD-denominated position limit
        let input_value_usd_e8 = plan.exact.input_value_usd_e8;
        if input_value_usd_e8 > self.max_position_usd_e8 {
            anyhow::bail!(
                "input value {} USD e8 exceeds max position {} USD e8",
                input_value_usd_e8,
                self.max_position_usd_e8
            );
        }

        // Check USD-denominated flash loan limit
        if matches!(
            plan.exact.capital_source,
            crate::types::CapitalSource::FlashLoan | crate::types::CapitalSource::MixedFlashLoan
        ) {
            let flash_value_usd_e8 = plan.exact.flash_loan_value_usd_e8;
            if flash_value_usd_e8 > self.max_flash_loan_usd_e8 {
                anyhow::bail!(
                    "flash loan value {} USD e8 exceeds max flash loan {} USD e8",
                    flash_value_usd_e8,
                    self.max_flash_loan_usd_e8
                );
            }
        }

        // Check USD-denominated profit
        if plan.exact.net_profit_usd_e8 < 0
            || (plan.exact.net_profit_usd_e8 as u128) < self.min_profit_usd_e8
        {
            anyhow::bail!(
                "net profit {} USD e8 below minimum {} USD e8",
                plan.exact.net_profit_usd_e8,
                self.min_profit_usd_e8
            );
        }

        // Concurrent tx limit
        if self.open_tx_count.load(Ordering::Relaxed) >= self.max_concurrent_tx {
            anyhow::bail!("max concurrent tx reached");
        }

        // Daily loss limit check
        let day_key = current_day_key();
        let pnl = *self.daily_pnl_usd_e8.lock().get(&day_key).unwrap_or(&0);
        if pnl < 0 && pnl.unsigned_abs() > self.daily_loss_limit_usd_e8 {
            anyhow::bail!(
                "daily loss {} USD e8 exceeds limit {} USD e8",
                pnl.unsigned_abs(),
                self.daily_loss_limit_usd_e8
            );
        }

        Ok(())
    }

    pub fn mark_submitted(&self) {
        self.open_tx_count.fetch_add(1, Ordering::SeqCst);
    }

    pub fn mark_finalized(&self, realized_profit_usd_e8: i128) {
        let previous = self.open_tx_count.fetch_sub(1, Ordering::SeqCst);
        if previous == 0 {
            warn!("risk manager open tx count underflow prevented");
            self.open_tx_count.store(0, Ordering::SeqCst);
            return;
        }

        let day_key = current_day_key();
        let mut daily = self.daily_pnl_usd_e8.lock();
        *daily.entry(day_key).or_default() += realized_profit_usd_e8;
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
