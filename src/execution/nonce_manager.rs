use std::{
    collections::HashMap,
    sync::atomic::{AtomicU64, Ordering},
};

use parking_lot::Mutex;

use crate::rpc::RpcClient;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NonceState {
    Reserved,
    Submitted,
    Included,
    Dropped,
}

#[derive(Debug)]
pub struct NonceManager {
    next_nonce: AtomicU64,
    states: Mutex<HashMap<u64, NonceState>>,
}

impl NonceManager {
    pub fn new() -> Self {
        Self {
            next_nonce: AtomicU64::new(0),
            states: Mutex::new(HashMap::new()),
        }
    }

    pub async fn sync(
        &self,
        rpc: &RpcClient,
        address: alloy::primitives::Address,
    ) -> anyhow::Result<()> {
        let onchain = rpc.get_transaction_count(address, "pending").await?;
        self.next_nonce.store(onchain, Ordering::SeqCst);
        Ok(())
    }

    pub fn reserve(&self) -> u64 {
        let nonce = self.next_nonce.fetch_add(1, Ordering::SeqCst);
        self.states.lock().insert(nonce, NonceState::Reserved);
        nonce
    }

    pub fn mark_submitted(&self, nonce: u64) {
        self.states.lock().insert(nonce, NonceState::Submitted);
    }

    pub fn mark_included(&self, nonce: u64) {
        self.states.lock().insert(nonce, NonceState::Included);
    }

    pub fn mark_dropped(&self, nonce: u64) {
        self.states.lock().insert(nonce, NonceState::Dropped);
    }
}
