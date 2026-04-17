//! Reorg detection and rollback handling.
//!
//! Provides reorg detection by tracking block hashes and parent hashes,
//! and supports rolling back to a common ancestor.

use std::collections::{HashMap, VecDeque};

use alloy::primitives::B256;
use anyhow::{Context, Result};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::types::{BlockRef, FinalityLevel};

/// Maximum number of blocks to keep in the canonical chain history
const MAX_BLOCK_HISTORY: usize = 128;

/// Reorg event
#[derive(Debug, Clone)]
pub enum ReorgEvent {
    Detected {
        common_ancestor_block: u64,
        new_chain_block: u64,
        depth: usize,
    },
    Rollback {
        from_block: u64,
        to_block: u64,
    },
    Replay {
        from_block: u64,
        to_block: u64,
    },
}

/// Block history entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockHistoryEntry {
    pub number: u64,
    pub hash: B256,
    pub parent_hash: B256,
    pub finality: FinalityLevel,
}

/// Reorg detector that tracks the canonical chain
pub struct ReorgDetector {
    chain: Mutex<VecDeque<BlockHistoryEntry>>,
    block_by_hash: Mutex<HashMap<B256, u64>>,
    block_by_number: Mutex<HashMap<u64, BlockHistoryEntry>>,
}

impl ReorgDetector {
    pub fn new() -> Self {
        Self {
            chain: Mutex::new(VecDeque::with_capacity(MAX_BLOCK_HISTORY)),
            block_by_hash: Mutex::new(HashMap::new()),
            block_by_number: Mutex::new(HashMap::new()),
        }
    }

    /// Get the current tip of the canonical chain
    pub fn current_tip(&self) -> Option<BlockHistoryEntry> {
        self.chain.lock().back().cloned()
    }

    /// Get a block by hash
    pub fn get_by_hash(&self, hash: B256) -> Option<BlockHistoryEntry> {
        let number = *self.block_by_hash.lock().get(&hash)?;
        self.block_by_number.lock().get(&number).cloned()
    }

    /// Get a block by number
    pub fn get_by_number(&self, number: u64) -> Option<BlockHistoryEntry> {
        self.block_by_number.lock().get(&number).cloned()
    }

    /// Update the canonical chain with a new block
    /// Returns Some(reorg_event) if a reorg is detected
    pub fn update_chain(&self, block_ref: BlockRef) -> Result<Option<ReorgEvent>> {
        let mut chain = self.chain.lock();
        let mut block_by_hash = self.block_by_hash.lock();
        let mut block_by_number = self.block_by_number.lock();

        let entry = BlockHistoryEntry {
            number: block_ref.number,
            hash: block_ref.hash,
            parent_hash: block_ref.parent_hash,
            finality: block_ref.finality,
        };

        // Check if this is a new block or a reorg
        match chain.back() {
            Some(tip) => {
                if entry.number == tip.number {
                    // Same block number - check if it's a different hash (reorg at same height)
                    if entry.hash != tip.hash {
                        self.handle_reorg_at_same_height(
                            &mut chain,
                            &mut block_by_hash,
                            &mut block_by_number,
                            entry,
                        )
                    } else {
                        // Same block, no update needed
                        Ok(None)
                    }
                } else if entry.number == tip.number + 1 {
                    // Normal chain extension
                    if entry.parent_hash != tip.hash {
                        self.handle_parent_mismatch(
                            &mut chain,
                            &mut block_by_hash,
                            &mut block_by_number,
                            entry,
                        )
                    } else {
                        self.append_block(
                            &mut chain,
                            &mut block_by_hash,
                            &mut block_by_number,
                            entry,
                        );
                        Ok(None)
                    }
                } else if entry.number > tip.number {
                    // Gap or reorg with deeper chain
                    self.handle_reorg_with_gap(
                        &mut chain,
                        &mut block_by_hash,
                        &mut block_by_number,
                        entry,
                    )
                } else {
                    // Older block - might be a reorg where we found the new chain
                    self.handle_reorg_with_older_block(
                        &mut chain,
                        &mut block_by_hash,
                        &mut block_by_number,
                        entry,
                    )
                }
            }
            None => {
                // First block
                self.append_block(&mut chain, &mut block_by_hash, &mut block_by_number, entry);
                Ok(None)
            }
        }
    }

    fn append_block(
        &self,
        chain: &mut VecDeque<BlockHistoryEntry>,
        block_by_hash: &mut HashMap<B256, u64>,
        block_by_number: &mut HashMap<u64, BlockHistoryEntry>,
        entry: BlockHistoryEntry,
    ) {
        chain.push_back(entry.clone());
        block_by_hash.insert(entry.hash, entry.number);
        block_by_number.insert(entry.number, entry);

        // Prune old blocks
        while chain.len() > MAX_BLOCK_HISTORY {
            if let Some(old) = chain.pop_front() {
                block_by_hash.remove(&old.hash);
                block_by_number.remove(&old.number);
            }
        }
    }

    fn handle_reorg_at_same_height(
        &self,
        chain: &mut VecDeque<BlockHistoryEntry>,
        block_by_hash: &mut HashMap<B256, u64>,
        block_by_number: &mut HashMap<u64, BlockHistoryEntry>,
        entry: BlockHistoryEntry,
    ) -> Result<Option<ReorgEvent>> {
        let old_tip = chain.back().cloned().context("chain should have a tip")?;

        // Remove the old tip
        chain.pop_back();
        block_by_hash.remove(&old_tip.hash);
        block_by_number.remove(&old_tip.number);

        // Add the new block
        chain.push_back(entry.clone());
        block_by_hash.insert(entry.hash, entry.number);
        block_by_number.insert(entry.number, entry.clone());

        warn!(
            old_block = old_tip.number,
            new_block = entry.number,
            old_hash = %old_tip.hash,
            new_hash = %entry.hash,
            "reorg detected at same block height"
        );

        Ok(Some(ReorgEvent::Rollback {
            from_block: old_tip.number,
            to_block: old_tip.number,
        }))
    }

    fn handle_parent_mismatch(
        &self,
        chain: &mut VecDeque<BlockHistoryEntry>,
        block_by_hash: &mut HashMap<B256, u64>,
        block_by_number: &mut HashMap<u64, BlockHistoryEntry>,
        entry: BlockHistoryEntry,
    ) -> Result<Option<ReorgEvent>> {
        // Find the common ancestor
        let common_ancestor = self.find_common_ancestor(entry.parent_hash)?;

        // Rollback to common ancestor
        let from_block = chain.back().map(|t| t.number).unwrap_or(0);
        while let Some(tip) = chain.back() {
            if tip.hash == common_ancestor.hash {
                break;
            }
            let old = chain.pop_back().context("chain should not be empty")?;
            block_by_hash.remove(&old.hash);
            block_by_number.remove(&old.number);
        }

        let depth = from_block.saturating_sub(common_ancestor.number) as usize;
        self.append_block(chain, block_by_hash, block_by_number, entry.clone());

        warn!(
            common_ancestor = common_ancestor.number,
            from_block,
            to_block = common_ancestor.number,
            depth,
            "reorg detected due to parent mismatch"
        );

        Ok(Some(ReorgEvent::Detected {
            common_ancestor_block: common_ancestor.number,
            new_chain_block: entry.number,
            depth,
        }))
    }

    fn handle_reorg_with_gap(
        &self,
        chain: &mut VecDeque<BlockHistoryEntry>,
        block_by_hash: &mut HashMap<B256, u64>,
        block_by_number: &mut HashMap<u64, BlockHistoryEntry>,
        entry: BlockHistoryEntry,
    ) -> Result<Option<ReorgEvent>> {
        // For gaps, we might need to backfill or this is a deeper reorg
        // Try to find if the parent exists in our chain
        if let Some(_parent_entry) = block_by_hash.get(&entry.parent_hash) {
            // Parent exists - this is a gap in our chain that was filled
            debug!(
                block = entry.number,
                parent = %entry.parent_hash,
                "filled gap in canonical chain"
            );
            self.append_block(chain, block_by_hash, block_by_number, entry);
            return Ok(None);
        }

        // Parent doesn't exist - might be a reorg or just missing history
        debug!(
            block = entry.number,
            parent = %entry.parent_hash,
            "received block with unknown parent; anchoring new tip after missing history"
        );
        self.append_block(chain, block_by_hash, block_by_number, entry);

        Ok(None)
    }

    fn handle_reorg_with_older_block(
        &self,
        chain: &mut VecDeque<BlockHistoryEntry>,
        block_by_hash: &mut HashMap<B256, u64>,
        block_by_number: &mut HashMap<u64, BlockHistoryEntry>,
        entry: BlockHistoryEntry,
    ) -> Result<Option<ReorgEvent>> {
        // Check if this block is already in our chain with a different hash
        if let Some(existing) = block_by_number.get(&entry.number) {
            if existing.hash != entry.hash {
                return self.handle_parent_mismatch(chain, block_by_hash, block_by_number, entry);
            }
            // Same block, no update needed
            return Ok(None);
        }

        // This is an older block we don't have - ignore or log
        debug!(block = entry.number, "received older block not in history");
        Ok(None)
    }

    fn find_common_ancestor(&self, target_hash: B256) -> Result<BlockHistoryEntry> {
        let block_by_hash = self.block_by_hash.lock();
        let block_by_number = self.block_by_number.lock();

        // First check if target is in our history
        let Some(target_number) = block_by_hash.get(&target_hash) else {
            anyhow::bail!("target hash {} not in block history", target_hash);
        };

        let Some(entry) = block_by_number.get(target_number) else {
            anyhow::bail!(
                "block number {} not found in block by number",
                target_number
            );
        };

        Ok(entry.clone())
    }

    /// Rollback to a specific snapshot ID (block number)
    pub fn rollback_to_block(&self, block_number: u64) -> Result<()> {
        let mut chain = self.chain.lock();
        let mut block_by_hash = self.block_by_hash.lock();
        let mut block_by_number = self.block_by_number.lock();

        while let Some(tip) = chain.back() {
            if tip.number <= block_number {
                break;
            }
            let old = chain.pop_back().context("chain should not be empty")?;
            block_by_hash.remove(&old.hash);
            block_by_number.remove(&old.number);
        }

        info!(
            rollback_to = block_number,
            current_tip = chain.back().map(|t| t.number),
            "rolled back canonical chain"
        );

        Ok(())
    }

    /// Get the current chain for debugging
    pub fn current_chain(&self) -> Vec<BlockHistoryEntry> {
        self.chain.lock().iter().cloned().collect()
    }

    /// Clear all history (for testing)
    #[cfg(test)]
    pub fn clear(&self) {
        self.chain.lock().clear();
        self.block_by_hash.lock().clear();
        self.block_by_number.lock().clear();
    }
}

impl Default for ReorgDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normal_chain_extension() {
        let detector = ReorgDetector::new();

        let block1 = BlockRef {
            number: 100,
            hash: B256::from([1u8; 32]),
            parent_hash: B256::from([0u8; 32]),
            finality: FinalityLevel::Sealed,
        };

        let result = detector.update_chain(block1).unwrap();
        assert!(result.is_none());

        let block2 = BlockRef {
            number: 101,
            hash: B256::from([2u8; 32]),
            parent_hash: B256::from([1u8; 32]),
            finality: FinalityLevel::Sealed,
        };

        let result = detector.update_chain(block2).unwrap();
        assert!(result.is_none());

        assert_eq!(detector.current_tip().unwrap().number, 101);
    }

    #[test]
    fn test_reorg_at_same_height() {
        let detector = ReorgDetector::new();

        let block1 = BlockRef {
            number: 100,
            hash: B256::from([1u8; 32]),
            parent_hash: B256::from([0u8; 32]),
            finality: FinalityLevel::Sealed,
        };

        detector.update_chain(block1).unwrap();

        let block1_alt = BlockRef {
            number: 100,
            hash: B256::from([99u8; 32]),
            parent_hash: B256::from([0u8; 32]),
            finality: FinalityLevel::Sealed,
        };

        let result = detector.update_chain(block1_alt).unwrap();
        assert!(matches!(result, Some(ReorgEvent::Rollback { .. })));
    }

    #[test]
    fn test_gap_with_unknown_parent_anchors_new_tip() {
        let detector = ReorgDetector::new();

        detector
            .update_chain(BlockRef {
                number: 100,
                hash: B256::from([1u8; 32]),
                parent_hash: B256::from([0u8; 32]),
                finality: FinalityLevel::Sealed,
            })
            .unwrap();

        let result = detector
            .update_chain(BlockRef {
                number: 110,
                hash: B256::from([10u8; 32]),
                parent_hash: B256::from([9u8; 32]),
                finality: FinalityLevel::Sealed,
            })
            .unwrap();

        assert!(result.is_none());
        assert_eq!(detector.current_tip().unwrap().number, 110);
    }
}
