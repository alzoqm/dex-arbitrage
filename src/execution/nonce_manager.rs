use std::{
    collections::HashMap,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use parking_lot::Mutex;

use crate::rpc::RpcClient;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NonceState {
    Reserved,
    Submitted {
        tx_hash: alloy::primitives::B256,
        submitted_at: Instant,
    },
    Included,
    Dropped,
}

#[derive(Debug, Clone)]
pub struct PendingNonce {
    pub nonce: u64,
    pub tx_hash: alloy::primitives::B256,
    pub submitted_at: Instant,
    pub max_priority_fee: u128,
    pub max_fee: u128,
}

/// Nonce manager with replacement and cancellation support
#[derive(Debug)]
pub struct NonceManager {
    next_nonce: AtomicU64,
    states: Mutex<HashMap<u64, NonceState>>,
    pending_txs: Mutex<HashMap<u64, PendingNonce>>,
    replacement_enabled: bool,
}

impl NonceManager {
    pub fn new() -> Self {
        Self {
            next_nonce: AtomicU64::new(0),
            states: Mutex::new(HashMap::new()),
            pending_txs: Mutex::new(HashMap::new()),
            replacement_enabled: true,
        }
    }

    pub fn with_replacement(replacement_enabled: bool) -> Self {
        Self {
            next_nonce: AtomicU64::new(0),
            states: Mutex::new(HashMap::new()),
            pending_txs: Mutex::new(HashMap::new()),
            replacement_enabled,
        }
    }

    /// Sync the on-chain nonce with the RPC
    pub async fn sync(
        &self,
        rpc: &RpcClient,
        address: alloy::primitives::Address,
    ) -> anyhow::Result<()> {
        let onchain = rpc.get_transaction_count(address, "pending").await?;
        let current = self.next_nonce.load(Ordering::SeqCst);
        // Only update backwards if onchain is higher (reorg or reset)
        if onchain > current {
            self.next_nonce.store(onchain, Ordering::SeqCst);
        }
        Ok(())
    }

    /// Reserve a new nonce
    pub fn reserve(&self) -> u64 {
        let nonce = self.next_nonce.fetch_add(1, Ordering::SeqCst);
        self.states.lock().insert(nonce, NonceState::Reserved);
        nonce
    }

    /// Mark a nonce as submitted with transaction details for potential replacement
    pub fn mark_submitted(
        &self,
        nonce: u64,
        tx_hash: alloy::primitives::B256,
        max_priority_fee: u128,
        max_fee: u128,
    ) {
        let state = NonceState::Submitted {
            tx_hash,
            submitted_at: Instant::now(),
        };
        self.states.lock().insert(nonce, state);

        let pending = PendingNonce {
            nonce,
            tx_hash,
            submitted_at: Instant::now(),
            max_priority_fee,
            max_fee,
        };
        self.pending_txs.lock().insert(nonce, pending);
    }

    /// Mark a nonce as included
    pub fn mark_included(&self, nonce: u64) {
        self.states.lock().insert(nonce, NonceState::Included);
        self.pending_txs.lock().remove(&nonce);
    }

    /// Mark a nonce as dropped (failed or replaced)
    pub fn mark_dropped(&self, nonce: u64) {
        self.states.lock().insert(nonce, NonceState::Dropped);
        self.pending_txs.lock().remove(&nonce);
    }

    /// Get all pending nonces that could be replaced
    pub fn pending_nonces(&self) -> Vec<PendingNonce> {
        self.pending_txs.lock().values().cloned().collect()
    }

    /// Check if a nonce is stuck (pending too long)
    pub fn is_stuck(&self, nonce: u64, timeout: Duration) -> bool {
        match self.states.lock().get(&nonce) {
            Some(NonceState::Submitted { submitted_at, .. }) => submitted_at.elapsed() > timeout,
            _ => false,
        }
    }

    /// Get the next transaction that can be replaced (bumped)
    pub fn next_replacable(&self, timeout: Duration) -> Option<PendingNonce> {
        if !self.replacement_enabled {
            return None;
        }

        self.pending_txs
            .lock()
            .values()
            .filter(|p| p.submitted_at.elapsed() > timeout)
            .min_by_key(|p| p.nonce)
            .cloned()
    }

    /// Calculate bumped fees for a transaction replacement
    pub fn calculate_bumped_fees(
        &self,
        pending: &PendingNonce,
        priority_bump: u128,
        base_bump: u128,
    ) -> (u128, u128) {
        let bumped_priority = pending.max_priority_fee.saturating_add(priority_bump);
        let bumped_base = pending.max_fee.saturating_add(base_bump);
        (bumped_priority, bumped_base)
    }

    /// Cancel a pending nonce (mark as dropped)
    pub fn cancel(&self, nonce: u64) -> bool {
        if let Some(NonceState::Submitted { .. }) = self.states.lock().get(&nonce) {
            self.mark_dropped(nonce);
            true
        } else {
            false
        }
    }

    /// Get current nonce without reserving
    pub fn current_nonce(&self) -> u64 {
        self.next_nonce.load(Ordering::SeqCst)
    }

    /// Get the state of a nonce
    pub fn get_state(&self, nonce: u64) -> Option<NonceState> {
        self.states.lock().get(&nonce).copied()
    }

    /// Reconcile pending nonces after restart
    pub async fn reconcile(
        &self,
        rpc: &RpcClient,
        address: alloy::primitives::Address,
    ) -> anyhow::Result<()> {
        let onchain = rpc.get_transaction_count(address, "latest").await?;
        let current = self.next_nonce.load(Ordering::SeqCst);

        let receipt_checks = self
            .pending_txs
            .lock()
            .values()
            .filter(|pending| pending.nonce >= onchain)
            .cloned()
            .collect::<Vec<_>>();

        let mut receipt_updates = Vec::new();
        for pending in receipt_checks {
            let update = match rpc.get_transaction_receipt_status(pending.tx_hash).await {
                Ok(Some(true)) => Some(NonceState::Included),
                Ok(Some(false)) => Some(NonceState::Dropped),
                Ok(None) => None,
                Err(_) => Some(NonceState::Dropped),
            };
            if let Some(state) = update {
                receipt_updates.push((pending.nonce, state));
            }
        }

        {
            let mut states = self.states.lock();
            let mut pending_txs = self.pending_txs.lock();

            for (nonce, state) in states.iter_mut() {
                if *nonce < onchain {
                    *state = NonceState::Included;
                    pending_txs.remove(nonce);
                }
            }

            for (nonce, state) in receipt_updates {
                states.insert(nonce, state);
                pending_txs.remove(&nonce);
            }
        }

        // Update next nonce if it's lower than onchain
        if current < onchain {
            self.next_nonce.store(onchain, Ordering::SeqCst);
        }

        Ok(())
    }
}

impl Default for NonceManager {
    fn default() -> Self {
        Self::new()
    }
}
