use super::{blockchain::StoredBlock, forked_lazy_backend::ForkedLazyBackend};
use polkadot_sdk::{
    sc_client_api::backend,
    sp_blockchain,
    sp_runtime::{
        Justification, Justifications, StateVersion, Storage,
        traits::{Block as BlockT, HashingFor},
    },
    sp_state_machine::{self, BackendTransaction, ChildStorageCollection, StorageCollection},
};
use serde::de::DeserializeOwned;

pub(crate) struct PendingBlock<B: BlockT> {
    pub(crate) block: StoredBlock<B>,
    pub(crate) state: backend::NewBlockState,
}

pub struct BlockImportOperation<Block: BlockT + DeserializeOwned> {
    pub(crate) pending_block: Option<PendingBlock<Block>>,
    pub(crate) old_state: ForkedLazyBackend<Block>,
    pub(crate) new_state: Option<BackendTransaction<HashingFor<Block>>>,
    pub(crate) aux: Vec<(Vec<u8>, Option<Vec<u8>>)>,
    pub(crate) storage_updates: StorageCollection,
    pub(crate) child_storage_updates: ChildStorageCollection,
    pub(crate) finalized_blocks: Vec<(Block::Hash, Option<Justification>)>,
    pub(crate) set_head: Option<Block::Hash>,
}

impl<Block: BlockT + DeserializeOwned> BlockImportOperation<Block> {
    pub(crate) fn apply_storage(
        &mut self,
        storage: Storage,
        commit: bool,
        state_version: StateVersion,
    ) -> sp_blockchain::Result<Block::Hash> {
        use sp_state_machine::Backend;
        check_genesis_storage(&storage)?;

        let child_delta = storage.children_default.values().map(|child_content| {
            (
                &child_content.child_info,
                child_content.data.iter().map(|(k, v)| (k.as_ref(), Some(v.as_ref()))),
            )
        });

        let (root, transaction) = self.old_state.full_storage_root(
            storage.top.iter().map(|(k, v)| (k.as_ref(), Some(v.as_ref()))),
            child_delta,
            state_version,
        );

        if commit {
            self.new_state = Some(transaction);
            self.storage_updates =
                storage
                    .top
                    .iter()
                    .map(|(k, v)| {
                        if v.is_empty() { (k.clone(), None) } else { (k.clone(), Some(v.clone())) }
                    })
                    .collect();

            self.child_storage_updates = storage
                .children_default
                .values()
                .map(|child_content| {
                    let child_storage: StorageCollection = child_content
                        .data
                        .iter()
                        .map(|(k, v)| {
                            if v.is_empty() {
                                (k.clone(), None)
                            } else {
                                (k.clone(), Some(v.clone()))
                            }
                        })
                        .collect();
                    (child_content.child_info.storage_key().to_vec(), child_storage)
                })
                .collect();
        }
        Ok(root)
    }
}

impl<Block: BlockT + DeserializeOwned> backend::BlockImportOperation<Block>
    for BlockImportOperation<Block>
{
    type State = ForkedLazyBackend<Block>;

    fn state(&self) -> sp_blockchain::Result<Option<&Self::State>> {
        Ok(Some(&self.old_state))
    }

    fn set_block_data(
        &mut self,
        header: <Block as BlockT>::Header,
        body: Option<Vec<<Block as BlockT>::Extrinsic>>,
        _indexed_body: Option<Vec<Vec<u8>>>,
        justifications: Option<Justifications>,
        state: backend::NewBlockState,
    ) -> sp_blockchain::Result<()> {
        assert!(self.pending_block.is_none(), "Only one block per operation is allowed");
        self.pending_block =
            Some(PendingBlock { block: StoredBlock::new(header, body, justifications), state });
        Ok(())
    }

    fn update_db_storage(
        &mut self,
        update: BackendTransaction<HashingFor<Block>>,
    ) -> sp_blockchain::Result<()> {
        self.new_state = Some(update);
        Ok(())
    }

    fn set_genesis_state(
        &mut self,
        storage: Storage,
        commit: bool,
        state_version: StateVersion,
    ) -> sp_blockchain::Result<Block::Hash> {
        self.apply_storage(storage, commit, state_version)
    }

    fn reset_storage(
        &mut self,
        storage: Storage,
        state_version: StateVersion,
    ) -> sp_blockchain::Result<Block::Hash> {
        self.apply_storage(storage, true, state_version)
    }

    fn insert_aux<I>(&mut self, ops: I) -> sp_blockchain::Result<()>
    where
        I: IntoIterator<Item = (Vec<u8>, Option<Vec<u8>>)>,
    {
        self.aux.append(&mut ops.into_iter().collect());
        Ok(())
    }

    fn update_storage(
        &mut self,
        update: StorageCollection,
        child_update: ChildStorageCollection,
    ) -> sp_blockchain::Result<()> {
        self.storage_updates = update;
        self.child_storage_updates = child_update;
        Ok(())
    }

    fn mark_finalized(
        &mut self,
        hash: Block::Hash,
        justification: Option<Justification>,
    ) -> sp_blockchain::Result<()> {
        self.finalized_blocks.push((hash, justification));
        Ok(())
    }

    fn mark_head(&mut self, hash: Block::Hash) -> sp_blockchain::Result<()> {
        assert!(self.pending_block.is_none(), "Only one set block per operation is allowed");
        self.set_head = Some(hash);
        Ok(())
    }

    fn update_transaction_index(
        &mut self,
        _index: Vec<sp_state_machine::IndexOperation>,
    ) -> sp_blockchain::Result<()> {
        Ok(())
    }

    fn set_create_gap(&mut self, _create_gap: bool) {}
}

pub(crate) fn check_genesis_storage(storage: &Storage) -> sp_blockchain::Result<()> {
    if storage
        .top
        .iter()
        .any(|(k, _)| polkadot_sdk::sp_core::storage::well_known_keys::is_child_storage_key(k))
    {
        return Err(sp_blockchain::Error::InvalidState);
    }

    Ok(())
}
