mod block_import_operation;
mod blockchain;
mod forked_lazy_backend;

pub use block_import_operation::BlockImportOperation;
pub use blockchain::Blockchain;
pub use forked_lazy_backend::ForkedLazyBackend;

#[cfg(test)]
mod tests;

use polkadot_sdk::{
    sc_client_api::{
        HeaderBackend, TrieCacheContext, UsageInfo,
        backend::{self, AuxStore},
    },
    sp_blockchain,
    sp_core::{H256, offchain::storage::InMemOffchainStorage},
    sp_runtime::{
        Justification, StateVersion,
        traits::{Block as BlockT, Header as HeaderT, NumberFor, One, Saturating, Zero},
    },
};
use serde::de::DeserializeOwned;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use parking_lot::RwLock;

use crate::substrate_node::lazy_loading::rpc_client::RPCClient;

pub struct Backend<Block: BlockT + DeserializeOwned> {
    pub(crate) rpc_client: Option<Arc<dyn RPCClient<Block>>>,
    pub(crate) fork_checkpoint: Option<Block::Header>,
    states: RwLock<HashMap<Block::Hash, ForkedLazyBackend<Block>>>,
    pub(crate) blockchain: Blockchain<Block>,
    import_lock: RwLock<()>,
    pinned_blocks: RwLock<HashMap<Block::Hash, i64>>,
}

impl<Block: BlockT + DeserializeOwned> Backend<Block> {
    fn new(rpc_client: Option<Arc<dyn RPCClient<Block>>>, fork_checkpoint: Option<Block::Header>) -> Self {
        Self {
            rpc_client: rpc_client.clone(),
            states: Default::default(),
            blockchain: Blockchain::new(rpc_client),
            import_lock: Default::default(),
            pinned_blocks: Default::default(),
            fork_checkpoint,
        }
    }

    #[inline]
    pub fn rpc(&self) -> Option<&dyn RPCClient<Block>> {
        self.rpc_client.as_deref()
    }
}

impl<Block: BlockT + DeserializeOwned> AuxStore for Backend<Block> {
    fn insert_aux<
        'a,
        'b: 'a,
        'c: 'a,
        I: IntoIterator<Item = &'a (&'c [u8], &'c [u8])>,
        D: IntoIterator<Item = &'a &'b [u8]>,
    >(
        &self,
        _insert: I,
        _delete: D,
    ) -> sp_blockchain::Result<()> {
        unimplemented!("`insert_aux` is not supported in lazy loading mode.")
    }

    fn get_aux(&self, _key: &[u8]) -> sp_blockchain::Result<Option<Vec<u8>>> {
        unimplemented!("`get_aux` is not supported in lazy loading mode.")
    }
}

impl<Block: BlockT + DeserializeOwned> backend::Backend<Block> for Backend<Block> {
    type BlockImportOperation = BlockImportOperation<Block>;
    type Blockchain = Blockchain<Block>;
    type State = ForkedLazyBackend<Block>;
    type OffchainStorage = InMemOffchainStorage;

    fn begin_operation(&self) -> sp_blockchain::Result<Self::BlockImportOperation> {
        let old_state = self.state_at(Default::default(), TrieCacheContext::Trusted)?;
        Ok(BlockImportOperation {
            pending_block: None,
            old_state,
            new_state: None,
            aux: Default::default(),
            storage_updates: Default::default(),
            child_storage_updates: Default::default(),
            finalized_blocks: Default::default(),
            set_head: None,
        })
    }

    fn begin_state_operation(
        &self,
        operation: &mut Self::BlockImportOperation,
        block: Block::Hash,
    ) -> sp_blockchain::Result<()> {
        operation.old_state = self.state_at(block, TrieCacheContext::Trusted)?;
        Ok(())
    }

    fn commit_operation(&self, operation: Self::BlockImportOperation) -> sp_blockchain::Result<()> {
        for (block, justification) in operation.finalized_blocks {
            self.blockchain.finalize_header(block, justification)?;
        }

        if let Some(pending_block) = operation.pending_block {
            let old_state = &operation.old_state;
            let (header, body, justification) = pending_block.block.into_inner();
            let hash = header.hash();

            let storage_updates = operation.storage_updates.clone();
            let child_storage_updates = operation.child_storage_updates.clone();

            let mut removed_keys_map = old_state.removed_keys.read().clone();
            for (key, value) in &storage_updates {
                if value.is_some() {
                    removed_keys_map.remove(key);
                } else {
                    removed_keys_map.insert(key.clone(), ());
                }
            }
            let new_removed_keys = Arc::new(RwLock::new(removed_keys_map));

            let mut db_clone = old_state.db.read().clone();
            {
                let mut entries = vec![(None, storage_updates.clone())];
                if !child_storage_updates.is_empty() {
                    entries.extend(child_storage_updates.iter().map(|(key, data)| {
                        (Some(polkadot_sdk::sp_storage::ChildInfo::new_default(key)), data.clone())
                    }));
                }
                db_clone.insert(entries, StateVersion::V1);
            }
            let new_db = Arc::new(RwLock::new(db_clone));
            let fork_block = self.fork_checkpoint.as_ref().map(|checkpoint| checkpoint.hash());
            let new_state = ForkedLazyBackend {
                rpc_client: self.rpc_client.clone(),
                block_hash: Some(hash),
                fork_block,
                db: new_db,
                removed_keys: new_removed_keys,
                before_fork: false,
            };
            self.states.write().insert(hash, new_state);

            self.blockchain.insert(hash, header, justification, body, pending_block.state)?;
        }

        if !operation.aux.is_empty() {
            self.blockchain.write_aux(operation.aux);
        }

        if let Some(set_head) = operation.set_head {
            self.blockchain.set_head(set_head)?;
        }

        Ok(())
    }

    fn finalize_block(
        &self,
        hash: Block::Hash,
        justification: Option<Justification>,
    ) -> sp_blockchain::Result<()> {
        self.blockchain.finalize_header(hash, justification)
    }

    fn append_justification(
        &self,
        hash: Block::Hash,
        justification: Justification,
    ) -> sp_blockchain::Result<()> {
        self.blockchain.append_justification(hash, justification)
    }

    fn blockchain(&self) -> &Self::Blockchain {
        &self.blockchain
    }

    fn usage_info(&self) -> Option<UsageInfo> {
        None
    }

    fn offchain_storage(&self) -> Option<Self::OffchainStorage> {
        None
    }

    fn state_at(
        &self,
        hash: Block::Hash,
        _trie_cache_context: TrieCacheContext,
    ) -> sp_blockchain::Result<Self::State> {
        if hash == Default::default() {
            let (fork_block, before_fork) = match &self.fork_checkpoint {
                Some(checkpoint) => (Some(checkpoint.hash()), true),
                None => (None, false),
            };

            return Ok(ForkedLazyBackend::<Block> {
                rpc_client: self.rpc_client.clone(),
                block_hash: Some(hash),
                fork_block,
                db: Default::default(),
                removed_keys: Default::default(),
                before_fork,
            });
        }

        let (backend, should_write) =
            self.states.read().get(&hash).cloned().map(|state| Ok((state, false))).unwrap_or_else(
                || {
                    self.rpc()
                        .and_then(|rpc| rpc.header(Some(hash)).ok())
                        .flatten()
                        .ok_or(sp_blockchain::Error::UnknownBlock(format!(
                            "Failed to fetch block header: {hash:?}"
                        )))
                        .map(|header| {
                            let state = match &self.fork_checkpoint {
                                Some(checkpoint) => {
                                    if header.number().gt(checkpoint.number()) {
                                        let parent = self
                                            .state_at(*header.parent_hash(), TrieCacheContext::Trusted)
                                            .ok();

                                        ForkedLazyBackend::<Block> {
                                            rpc_client: self.rpc_client.clone(),
                                            block_hash: Some(hash),
                                            fork_block: Some(checkpoint.hash()),
                                            db: parent.clone().map_or(Default::default(), |p| p.db),
                                            removed_keys: parent
                                                .map_or(Default::default(), |p| p.removed_keys),
                                            before_fork: false,
                                        }
                                    } else {
                                        ForkedLazyBackend::<Block> {
                                            rpc_client: self.rpc_client.clone(),
                                            block_hash: Some(hash),
                                            fork_block: Some(checkpoint.hash()),
                                            db: Default::default(),
                                            removed_keys: Default::default(),
                                            before_fork: true,
                                        }
                                    }
                                }
                                None => {
                                    let parent = self
                                        .state_at(*header.parent_hash(), TrieCacheContext::Trusted)
                                        .ok();

                                    ForkedLazyBackend::<Block> {
                                        rpc_client: self.rpc_client.clone(),
                                        block_hash: Some(hash),
                                        fork_block: None,
                                        db: parent.clone().map_or(Default::default(), |p| p.db),
                                        removed_keys: parent
                                            .map_or(Default::default(), |p| p.removed_keys),
                                        before_fork: false,
                                    }
                                }
                            };

                            (state, true)
                        })
                },
            )?;

        if should_write {
            self.states.write().insert(hash, backend.clone());
        }

        Ok(backend)
    }

    fn revert(
        &self,
        n: NumberFor<Block>,
        revert_finalized: bool,
    ) -> sp_blockchain::Result<(NumberFor<Block>, HashSet<Block::Hash>)> {
        let mut storage = self.blockchain.storage.write();

        if storage.blocks.is_empty() {
            return Ok((Zero::zero(), HashSet::new()));
        }

        let mut states = self.states.write();
        let pinned = self.pinned_blocks.read();

        let mut target = n;
        let original_finalized_number = storage.finalized_number;

        if !target.is_zero() && !revert_finalized {
            let revertible = storage.best_number.saturating_sub(storage.finalized_number);
            if target > revertible {
                target = revertible;
            }
        }

        let mut reverted = NumberFor::<Block>::zero();
        let mut reverted_finalized = HashSet::new();

        let mut current_hash = storage.best_hash;
        let mut current_number = storage.best_number;

        while reverted < target {
            if current_number.is_zero() {
                break;
            }

            if let Some(count) = pinned.get(&current_hash) {
                if *count > 0 {
                    break;
                }
            }

            let Some(block) = storage.blocks.get(&current_hash) else {
                break;
            };

            let header = block.header().clone();
            let number = *header.number();
            let parent_hash = header.parent_hash();
            let parent_number = number.saturating_sub(One::one());

            let parent_becomes_leaf = if number.is_zero() {
                false
            } else {
                !storage.blocks.iter().any(|(other_hash, stored)| {
                    *other_hash != current_hash && stored.header().parent_hash() == parent_hash
                })
            };

            let hash_to_remove = current_hash;

            storage.blocks.remove(&hash_to_remove);
            if let Some(entry) = storage.hashes.get(&number) {
                if *entry == hash_to_remove {
                    storage.hashes.remove(&number);
                }
            }
            states.remove(&hash_to_remove);

            storage.leaves.remove(
                hash_to_remove,
                number,
                parent_becomes_leaf.then_some(*parent_hash),
            );

            if number <= original_finalized_number {
                reverted_finalized.insert(hash_to_remove);
            }

            reverted = reverted.saturating_add(One::one());

            current_hash = *parent_hash;
            current_number = parent_number;

            storage.best_hash = current_hash;
            storage.best_number = current_number;
        }

        let best_hash_after = storage.best_hash;
        let best_number_after = storage.best_number;
        let extra_leaves: Vec<_> =
            storage.leaves.revert(best_hash_after, best_number_after).collect();

        for (hash, number) in extra_leaves {
            if let Some(count) = pinned.get(&hash) {
                if *count > 0 {
                    return Err(sp_blockchain::Error::Backend(format!(
                        "Can't revert pinned block {hash:?}",
                    )));
                }
            }

            storage.blocks.remove(&hash);
            if let Some(entry) = storage.hashes.get(&number) {
                if *entry == hash {
                    storage.hashes.remove(&number);
                }
            }
            states.remove(&hash);

            if number <= original_finalized_number {
                reverted_finalized.insert(hash);
            }
        }

        storage.hashes.insert(best_number_after, best_hash_after);

        if storage.finalized_number > best_number_after {
            storage.finalized_number = best_number_after;
        }

        while storage.finalized_number > Zero::zero()
            && !storage.hashes.contains_key(&storage.finalized_number)
        {
            storage.finalized_number = storage.finalized_number.saturating_sub(One::one());
        }

        if let Some(hash) = storage.hashes.get(&storage.finalized_number).copied() {
            storage.finalized_hash = hash;
        } else {
            storage.finalized_hash = storage.genesis_hash;
        }

        Ok((reverted, reverted_finalized))
    }

    fn remove_leaf_block(&self, hash: Block::Hash) -> sp_blockchain::Result<()> {
        let best_hash = self.blockchain.info().best_hash;

        if best_hash == hash {
            return Err(sp_blockchain::Error::Backend(
                format!("Can't remove best block {hash:?}",),
            ));
        }

        let mut storage = self.blockchain.storage.write();

        let Some(block) = storage.blocks.get(&hash) else {
            return Err(sp_blockchain::Error::UnknownBlock(format!("{hash:?}")));
        };

        let number = *block.header().number();
        let parent_hash = *block.header().parent_hash();

        if !storage.leaves.contains(number, hash) {
            return Err(sp_blockchain::Error::Backend(format!(
                "Can't remove non-leaf block {hash:?}",
            )));
        }

        if self.pinned_blocks.read().get(&hash).is_some_and(|count| *count > 0) {
            return Err(sp_blockchain::Error::Backend(format!(
                "Can't remove pinned block {hash:?}",
            )));
        }

        let parent_becomes_leaf = if number.is_zero() {
            false
        } else {
            !storage.blocks.iter().any(|(other_hash, stored)| {
                *other_hash != hash && stored.header().parent_hash() == &parent_hash
            })
        };

        let mut states = self.states.write();

        storage.blocks.remove(&hash);
        if let Some(entry) = storage.hashes.get(&number) {
            if *entry == hash {
                storage.hashes.remove(&number);
            }
        }
        states.remove(&hash);

        storage.leaves.remove(hash, number, parent_becomes_leaf.then_some(parent_hash));

        Ok(())
    }

    fn get_import_lock(&self) -> &RwLock<()> {
        &self.import_lock
    }

    fn requires_full_sync(&self) -> bool {
        false
    }

    fn pin_block(&self, hash: <Block as BlockT>::Hash) -> sp_blockchain::Result<()> {
        let mut blocks = self.pinned_blocks.write();
        *blocks.entry(hash).or_default() += 1;
        Ok(())
    }

    fn unpin_block(&self, hash: <Block as BlockT>::Hash) {
        let mut blocks = self.pinned_blocks.write();
        blocks.entry(hash).and_modify(|counter| *counter -= 1).or_insert(-1);
    }
}

impl<Block: BlockT + DeserializeOwned> backend::LocalBackend<Block> for Backend<Block> {}

pub fn new_backend<Block>(
    rpc_client: Option<Arc<dyn RPCClient<Block>>>,
    checkpoint: Option<Block::Header>,
) -> Result<Arc<Backend<Block>>, polkadot_sdk::sc_service::Error>
where
    Block: BlockT + DeserializeOwned,
    Block::Hash: From<H256>,
{
    let backend = Arc::new(Backend::new(rpc_client, checkpoint));
    Ok(backend)
}
