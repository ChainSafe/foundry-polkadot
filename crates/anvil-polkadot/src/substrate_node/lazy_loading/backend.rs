use super::{
    rpc_client::{RPC, RPCClient},
};
use polkadot_sdk::{
    sc_client_api::{
        StorageKey, TrieCacheContext, UsageInfo,
        backend::{self, NewBlockState},
        blockchain::{self, BlockStatus, HeaderBackend},
        leaves::LeafSet,
    },
    sc_service::{Configuration, Error},
    sp_blockchain::{self, CachedHeaderMetadata, HeaderMetadata},
    sp_core::{self, H256, offchain::storage::InMemOffchainStorage, storage::well_known_keys},
    sp_runtime::{
        Justification, Justifications, StateVersion, Storage,
        generic::{BlockId, SignedBlock},
        traits::{Block as BlockT, HashingFor, Header as HeaderT, NumberFor, Zero},
    },
    sp_state_machine::{
        self, BackendTransaction, ChildStorageCollection, IndexOperation, StorageCollection,
        TrieBackend,
    },
    sp_storage::{self, ChildInfo},
    sp_trie::{self, PrefixedMemoryDB},
};
use serde::de::DeserializeOwned;
use std::{
    collections::{HashMap, HashSet},
    marker::PhantomData,
    ptr,
    sync::Arc,
    time::Duration,
};

struct PendingBlock<B: BlockT> {
    block: StoredBlock<B>,
    state: NewBlockState,
}

#[derive(PartialEq, Eq, Clone)]
enum StoredBlock<B: BlockT> {
    Header(B::Header, Option<Justifications>),
    Full(B, Option<Justifications>),
}

impl<B: BlockT> StoredBlock<B> {
    fn new(
        header: B::Header,
        body: Option<Vec<B::Extrinsic>>,
        just: Option<Justifications>,
    ) -> Self {
        match body {
            Some(body) => StoredBlock::Full(B::new(header, body), just),
            None => StoredBlock::Header(header, just),
        }
    }

    fn header(&self) -> &B::Header {
        match *self {
            StoredBlock::Header(ref h, _) => h,
            StoredBlock::Full(ref b, _) => b.header(),
        }
    }

    fn justifications(&self) -> Option<&Justifications> {
        match *self {
            StoredBlock::Header(_, ref j) | StoredBlock::Full(_, ref j) => j.as_ref(),
        }
    }

    fn extrinsics(&self) -> Option<&[B::Extrinsic]> {
        match *self {
            StoredBlock::Header(_, _) => None,
            StoredBlock::Full(ref b, _) => Some(b.extrinsics()),
        }
    }

    fn into_inner(self) -> (B::Header, Option<Vec<B::Extrinsic>>, Option<Justifications>) {
        match self {
            StoredBlock::Header(header, just) => (header, None, just),
            StoredBlock::Full(block, just) => {
                let (header, body) = block.deconstruct();
                (header, Some(body), just)
            }
        }
    }
}

#[derive(Clone)]
struct BlockchainStorage<Block: BlockT> {
    blocks: HashMap<Block::Hash, StoredBlock<Block>>,
    hashes: HashMap<NumberFor<Block>, Block::Hash>,
    best_hash: Block::Hash,
    best_number: NumberFor<Block>,
    finalized_hash: Block::Hash,
    finalized_number: NumberFor<Block>,
    genesis_hash: Block::Hash,
    header_cht_roots: HashMap<NumberFor<Block>, Block::Hash>,
    leaves: LeafSet<Block::Hash, NumberFor<Block>>,
    aux: HashMap<Vec<u8>, Vec<u8>>,
}

/// In-memory blockchain. Supports concurrent reads.
#[derive(Clone)]
pub struct Blockchain<Block: BlockT + DeserializeOwned> {
    rpc_client: Option<Arc<dyn RPCClient<Block>>>,
    storage: Arc<parking_lot::RwLock<BlockchainStorage<Block>>>,
}

impl<Block: BlockT + DeserializeOwned> Blockchain<Block> {
    /// Create new in-memory blockchain storage.
    fn new(rpc_client: Option<Arc<dyn RPCClient<Block>>>) -> Blockchain<Block> {
        log::info!(
            target: super::LAZY_LOADING_LOG_TARGET,
            "🏗️  Creating new Blockchain storage (empty)"
        );
        
        let storage = Arc::new(parking_lot::RwLock::new(BlockchainStorage {
            blocks: HashMap::new(),
            hashes: HashMap::new(),
            best_hash: Default::default(),
            best_number: Zero::zero(),
            finalized_hash: Default::default(),
            finalized_number: Zero::zero(),
            genesis_hash: Default::default(),
            header_cht_roots: HashMap::new(),
            leaves: LeafSet::new(),
            aux: HashMap::new(),
        }));
        Blockchain { rpc_client, storage }
    }
    #[inline]
    fn rpc(&self) -> Option<&dyn RPCClient<Block>> {
        self.rpc_client.as_deref()
    }

    /// Get header hash of given block.
    pub fn id(&self, id: BlockId<Block>) -> Option<Block::Hash> {
        match id {
            BlockId::Hash(h) => Some(h),
            BlockId::Number(n) => {
                log::info!(
                    target: super::LAZY_LOADING_LOG_TARGET,
                    "Looking up block hash for number={}", n
                );
                
                let block_hash = self.storage.read().hashes.get(&n).cloned();
                
                log::info!(
                    target: super::LAZY_LOADING_LOG_TARGET,
                    "Lookup result: number={}, found={}, total_hashes={}",
                    n,
                    block_hash.is_some(),
                    self.storage.read().hashes.len()
                );
                
                match block_hash {
                    None => {
                        log::info!(
                            target: super::LAZY_LOADING_LOG_TARGET,
                            "Block hash not found locally, trying RPC for number={}", n
                        );
                        let block_hash =
                            self.rpc().and_then(|rpc| rpc.block_hash(Some(n)).ok().flatten());
                        if let Some(h) = block_hash {
                            self.storage.write().hashes.insert(n, h);
                        }
                        block_hash
                    }
                    block_hash => block_hash,
                }
            }
        }
    }

    /// Insert a block header and associated data.
    pub fn insert(
        &self,
        hash: Block::Hash,
        header: <Block as BlockT>::Header,
        justifications: Option<Justifications>,
        body: Option<Vec<<Block as BlockT>::Extrinsic>>,
        new_state: NewBlockState,
    ) -> sp_blockchain::Result<()> {
        let number = *header.number();
        
        log::info!(
            target: super::LAZY_LOADING_LOG_TARGET,
            "Inserting block: number={}, hash={:?}, new_state={:?}",
            number,
            hash,
            new_state
        );
        
        if new_state.is_best() {
            self.apply_head(&header)?;
        }

        let mut storage = self.storage.write();
        
        // Always insert the block into blocks and hashes storage
        storage.blocks.insert(hash, StoredBlock::new(header.clone(), body, justifications));
        storage.hashes.insert(number, hash);
        
        log::info!(
            target: super::LAZY_LOADING_LOG_TARGET,
            "Block inserted successfully: number={}, hash={:?}. Total blocks={}, Total hashes={}",
            number,
            hash,
            storage.blocks.len(),
            storage.hashes.len()
        );
        
        // Set genesis_hash only for the first block inserted
        if storage.blocks.len() == 1 {
            storage.genesis_hash = hash;
        }
        
        // Update leaves for non-genesis blocks
        if storage.blocks.len() > 1 {
            storage.leaves.import(hash, number, *header.parent_hash());
        }
        
        // Finalize block only if explicitly requested via new_state
        if let NewBlockState::Final = new_state {
            storage.finalized_hash = hash;
            storage.finalized_number = number;
        }

        Ok(())
    }

    /// Get total number of blocks.
    pub fn blocks_count(&self) -> usize {
        let count = self.storage.read().blocks.len();

        log::debug!(
            target: super::LAZY_LOADING_LOG_TARGET,
            "Total number of blocks: {:?}",
            count
        );

        count
    }

    /// Compare this blockchain with another in-mem blockchain
    pub fn equals_to(&self, other: &Self) -> bool {
        // Check ptr equality first to avoid double read locks.
        if ptr::eq(self, other) {
            return true;
        }
        self.canon_equals_to(other) && self.storage.read().blocks == other.storage.read().blocks
    }

    /// Compare canonical chain to other canonical chain.
    pub fn canon_equals_to(&self, other: &Self) -> bool {
        // Check ptr equality first to avoid double read locks.
        if ptr::eq(self, other) {
            return true;
        }
        let this = self.storage.read();
        let other = other.storage.read();
        this.hashes == other.hashes
            && this.best_hash == other.best_hash
            && this.best_number == other.best_number
            && this.genesis_hash == other.genesis_hash
    }

    /// Insert header CHT root.
    pub fn insert_cht_root(&self, block: NumberFor<Block>, cht_root: Block::Hash) {
        self.storage.write().header_cht_roots.insert(block, cht_root);
    }

    /// Set an existing block as head.
    pub fn set_head(&self, hash: Block::Hash) -> sp_blockchain::Result<()> {
        let header = self
            .header(hash)?
            .ok_or_else(|| sp_blockchain::Error::UnknownBlock(format!("{}", hash)))?;

        self.apply_head(&header)
    }

    fn apply_head(&self, header: &<Block as BlockT>::Header) -> sp_blockchain::Result<()> {
        let mut storage = self.storage.write();

        let hash = header.hash();
        let number = header.number();

        storage.best_hash = hash;
        storage.best_number = *number;
        storage.hashes.insert(*number, hash);

        Ok(())
    }

    fn finalize_header(
        &self,
        block: Block::Hash,
        justification: Option<Justification>,
    ) -> sp_blockchain::Result<()> {
        let mut storage = self.storage.write();
        storage.finalized_hash = block;

        if justification.is_some() {
            let block = storage
                .blocks
                .get_mut(&block)
                .expect("hash was fetched from a block in the db; qed");

            let block_justifications = match block {
                StoredBlock::Header(_, j) | StoredBlock::Full(_, j) => j,
            };

            *block_justifications = justification.map(Justifications::from);
        }

        Ok(())
    }

    fn append_justification(
        &self,
        hash: Block::Hash,
        justification: Justification,
    ) -> sp_blockchain::Result<()> {
        let mut storage = self.storage.write();

        let block =
            storage.blocks.get_mut(&hash).expect("hash was fetched from a block in the db; qed");

        let block_justifications = match block {
            StoredBlock::Header(_, j) | StoredBlock::Full(_, j) => j,
        };

        if let Some(stored_justifications) = block_justifications {
            if !stored_justifications.append(justification) {
                return Err(sp_blockchain::Error::BadJustification(
                    "Duplicate consensus engine ID".into(),
                ));
            }
        } else {
            *block_justifications = Some(Justifications::from(justification));
        };

        Ok(())
    }

    fn write_aux(&self, ops: Vec<(Vec<u8>, Option<Vec<u8>>)>) {
        let mut storage = self.storage.write();
        for (k, v) in ops {
            match v {
                Some(v) => storage.aux.insert(k, v),
                None => storage.aux.remove(&k),
            };
        }
    }
}

impl<Block: BlockT + DeserializeOwned> HeaderBackend<Block> for Blockchain<Block> {
    fn header(
        &self,
        hash: Block::Hash,
    ) -> sp_blockchain::Result<Option<<Block as BlockT>::Header>> {
        // First, try to get the header from local storage
        if let Some(header) = self.storage.read().blocks.get(&hash).map(|b| b.header().clone()) {
            return Ok(Some(header));
        }

        // If not found in local storage, fetch from RPC client
        let header = if let Some(rpc) = self.rpc() {
            rpc.block(Some(hash)).ok().flatten().map(|full| {
                let block = full.block.clone();
                self.storage
                    .write()
                    .blocks
                    .insert(hash, StoredBlock::Full(block.clone(), full.justifications));
                block.header().clone()
            })
        } else {
            None
        };

        if header.is_none() {
            log::warn!(
                target: super::LAZY_LOADING_LOG_TARGET,
                "Expected block {:x?} to exist.",
                &hash
            );
        }

        Ok(header)
    }

    fn info(&self) -> blockchain::Info<Block> {
        let storage = self.storage.read();
        // Return None for finalized_state when blockchain is empty or only has genesis block
        // This allows Client::new to properly initialize and complete genesis setup
        // finalized_state should only be Some() when there are blocks beyond genesis
        let finalized_state = if storage.blocks.len() <= 1 {
            None
        } else {
            Some((storage.finalized_hash, storage.finalized_number))
        };
        
        log::info!(
            target: super::LAZY_LOADING_LOG_TARGET,
            "📊 Blockchain::info() - blocks={}, best_hash={:?}, best_number={}, genesis_hash={:?}, finalized_hash={:?}, finalized_number={}, finalized_state={:?}",
            storage.blocks.len(),
            storage.best_hash,
            storage.best_number,
            storage.genesis_hash,
            storage.finalized_hash,
            storage.finalized_number,
            finalized_state
        );
        
        blockchain::Info {
            best_hash: storage.best_hash,
            best_number: storage.best_number,
            genesis_hash: storage.genesis_hash,
            finalized_hash: storage.finalized_hash,
            finalized_number: storage.finalized_number,
            finalized_state,
            number_leaves: storage.leaves.count(),
            block_gap: None,
        }
    }

    fn status(&self, hash: Block::Hash) -> sp_blockchain::Result<BlockStatus> {
        match self.storage.read().blocks.contains_key(&hash) {
            true => Ok(BlockStatus::InChain),
            false => Ok(BlockStatus::Unknown),
        }
    }

    fn number(&self, hash: Block::Hash) -> sp_blockchain::Result<Option<NumberFor<Block>>> {
        if let Some(b) = self.storage.read().blocks.get(&hash) {
            return Ok(Some(*b.header().number()));
        }
        match self.rpc() {
            Some(rpc) => match rpc.block(Some(hash)) {
                Ok(Some(block)) => Ok(Some(*block.block.header().number())),
                err => Err(sp_blockchain::Error::UnknownBlock(
                    format!("Failed to fetch block number from RPC: {:?}", err).into(),
                )),
            },
            None => Err(sp_blockchain::Error::UnknownBlock(
                "RPC not configured to resolve block number".into(),
            )),
        }
    }

    fn hash(
        &self,
        number: <<Block as BlockT>::Header as HeaderT>::Number,
    ) -> sp_blockchain::Result<Option<Block::Hash>> {
        Ok(self.id(BlockId::Number(number)))
    }
}

impl<Block: BlockT + DeserializeOwned> HeaderMetadata<Block> for Blockchain<Block> {
    type Error = sp_blockchain::Error;

    fn header_metadata(
        &self,
        hash: Block::Hash,
    ) -> Result<CachedHeaderMetadata<Block>, Self::Error> {
        self.header(hash)?.map(|header| CachedHeaderMetadata::from(&header)).ok_or_else(|| {
            sp_blockchain::Error::UnknownBlock(format!("header not found: {}", hash))
        })
    }

    fn insert_header_metadata(&self, _hash: Block::Hash, _metadata: CachedHeaderMetadata<Block>) {
        // No need to implement.
        unimplemented!("insert_header_metadata")
    }
    fn remove_header_metadata(&self, _hash: Block::Hash) {
        // No need to implement.
        unimplemented!("remove_header_metadata")
    }
}

impl<Block: BlockT + DeserializeOwned> blockchain::Backend<Block> for Blockchain<Block> {
    fn body(
        &self,
        hash: Block::Hash,
    ) -> sp_blockchain::Result<Option<Vec<<Block as BlockT>::Extrinsic>>> {
        if let Some(xs) =
            self.storage.read().blocks.get(&hash).and_then(|b| b.extrinsics().map(|x| x.to_vec()))
        {
            return Ok(Some(xs));
        }
        let extrinsics = self.rpc().and_then(|rpc| {
            rpc.block(Some(hash)).ok().flatten().map(|b| b.block.extrinsics().to_vec())
        });
        Ok(extrinsics)
    }

    fn justifications(&self, hash: Block::Hash) -> sp_blockchain::Result<Option<Justifications>> {
        Ok(self.storage.read().blocks.get(&hash).and_then(|b| b.justifications().cloned()))
    }

    fn last_finalized(&self) -> sp_blockchain::Result<Block::Hash> {
        let last_finalized = self.storage.read().finalized_hash;

        Ok(last_finalized)
    }

    fn leaves(&self) -> sp_blockchain::Result<Vec<Block::Hash>> {
        let leaves = self.storage.read().leaves.hashes();

        Ok(leaves)
    }

    fn children(&self, _parent_hash: Block::Hash) -> sp_blockchain::Result<Vec<Block::Hash>> {
        unimplemented!("Not supported by the `lazy-loading` backend.")
    }

    fn indexed_transaction(&self, _hash: Block::Hash) -> sp_blockchain::Result<Option<Vec<u8>>> {
        unimplemented!("Not supported by the `lazy-loading` backend.")
    }

    fn block_indexed_body(
        &self,
        _hash: Block::Hash,
    ) -> sp_blockchain::Result<Option<Vec<Vec<u8>>>> {
        unimplemented!("Not supported by the `lazy-loading` backend.")
    }
}

impl<Block: BlockT + DeserializeOwned> backend::AuxStore for Blockchain<Block> {
    fn insert_aux<
        'a,
        'b: 'a,
        'c: 'a,
        I: IntoIterator<Item = &'a (&'c [u8], &'c [u8])>,
        D: IntoIterator<Item = &'a &'b [u8]>,
    >(
        &self,
        insert: I,
        delete: D,
    ) -> sp_blockchain::Result<()> {
        let mut storage = self.storage.write();
        for (k, v) in insert {
            storage.aux.insert(k.to_vec(), v.to_vec());
        }
        for k in delete {
            storage.aux.remove(*k);
        }
        Ok(())
    }

    fn get_aux(&self, key: &[u8]) -> sp_blockchain::Result<Option<Vec<u8>>> {
        Ok(self.storage.read().aux.get(key).cloned())
    }
}

pub struct BlockImportOperation<Block: BlockT + DeserializeOwned> {
    pending_block: Option<PendingBlock<Block>>,
    old_state: ForkedLazyBackend<Block>,
    new_state: Option<BackendTransaction<HashingFor<Block>>>,
    aux: Vec<(Vec<u8>, Option<Vec<u8>>)>,
    storage_updates: StorageCollection,
    finalized_blocks: Vec<(Block::Hash, Option<Justification>)>,
    set_head: Option<Block::Hash>,
    pub(crate) before_fork: bool,
}

impl<Block: BlockT + DeserializeOwned> BlockImportOperation<Block> {
    fn apply_storage(
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
        state: NewBlockState,
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
        _child_update: ChildStorageCollection,
    ) -> sp_blockchain::Result<()> {
        self.storage_updates = update.clone();
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
        _index: Vec<IndexOperation>,
    ) -> sp_blockchain::Result<()> {
        Ok(())
    }

    fn set_create_gap(&mut self, _create_gap: bool) {
        // This implementation can be left empty or implemented as needed
        // For now, we're just implementing the trait method with no functionality
    }
}

/// DB-backed patricia trie state, transaction type is an overaay of changes to commit.
pub type DbState<B> = TrieBackend<Arc<dyn sp_state_machine::Storage<HashingFor<B>>>, HashingFor<B>>;

/// A struct containing arguments for iterating over the storage.
#[derive(Default)]
pub struct RawIterArgs {
    /// The prefix of the keys over which to iterate.
    pub prefix: Option<Vec<u8>>,

    /// The prefix from which to start the iteration from.
    ///
    /// This is inclusive and the iteration will include the key which is specified here.
    pub start_at: Option<Vec<u8>>,

    /// If this is `true` then the iteration will *not* include
    /// the key specified in `start_at`, if there is such a key.
    pub start_at_exclusive: bool,
}

/// A raw iterator over the `BenchmarkingState`.
pub struct RawIter<Block: BlockT + DeserializeOwned> {
    pub(crate) args: RawIterArgs,
    complete: bool,
    _phantom: PhantomData<Block>,
}

impl<Block: BlockT + DeserializeOwned> sp_state_machine::StorageIterator<HashingFor<Block>>
    for RawIter<Block>
{
    type Backend = ForkedLazyBackend<Block>;
    type Error = String;

    fn next_key(
        &mut self,
        backend: &Self::Backend,
    ) -> Option<Result<sp_state_machine::StorageKey, Self::Error>> {
        use sp_state_machine::Backend;

        let remote_fetch =
            |key: Option<StorageKey>, start_key: Option<StorageKey>, block: Option<Block::Hash>| {
                backend.rpc().and_then(|rpc| rpc.storage_keys_paged(key, 5, start_key, block).ok()).and_then(|keys| keys.first().map(|key| key.clone()))
            };

        let prefix = self.args.prefix.clone().map(|k| StorageKey(k));
        let start_key = self.args.start_at.clone().map(|k| StorageKey(k));

        let maybe_next_key = if backend.before_fork {
            // If RPC client is available, fetch remotely
            if backend.rpc().is_some() {
                remote_fetch(prefix, start_key, backend.block_hash)
            } else {
                // No RPC client, use local DB
                let mut iter_args = sp_state_machine::backend::IterArgs::default();
                iter_args.prefix = self.args.prefix.as_deref();
                iter_args.start_at = self.args.start_at.as_deref();
                iter_args.start_at_exclusive = true;
                iter_args.stop_on_incomplete_database = true;

                let readable_db = backend.db.read();
                readable_db
                    .raw_iter(iter_args)
                    .map(|mut iter| iter.next_key(&readable_db))
                    .map(|op| op.and_then(|result| result.ok()))
                    .ok()
                    .flatten()
            }
        } else {
            let mut iter_args = sp_state_machine::backend::IterArgs::default();
            iter_args.prefix = self.args.prefix.as_deref();
            iter_args.start_at = self.args.start_at.as_deref();
            iter_args.start_at_exclusive = true;
            iter_args.stop_on_incomplete_database = true;

            let readable_db = backend.db.read();
            let next_storage_key = readable_db
                .raw_iter(iter_args)
                .map(|mut iter| iter.next_key(&readable_db))
                .map(|op| op.and_then(|result| result.ok()))
                .ok()
                .flatten();

            // IMPORTANT: free storage read lock
            drop(readable_db);

            let removed_key = start_key
                .clone()
                .or(prefix.clone())
                .map(|key| backend.removed_keys.read().contains_key(&key.0))
                .unwrap_or(false);
            if next_storage_key.is_none() && !removed_key {
                let maybe_next_key = if backend.rpc().is_some() {
                    remote_fetch(prefix, start_key, Some(backend.fork_block))
                } else {
                    None
                };
                match maybe_next_key {
                    Some(key) if !backend.removed_keys.read().contains_key(&key) => Some(key),
                    _ => None,
                }
            } else {
                next_storage_key
            }
        };

        log::trace!(
            target: super::LAZY_LOADING_LOG_TARGET,
            "next_key: (prefix: {:?}, start_at: {:?}, next_key: {:?})",
            self.args.prefix.clone().map(|key| hex::encode(key)),
            self.args.start_at.clone().map(|key| hex::encode(key)),
            maybe_next_key.clone().map(|key| hex::encode(key))
        );

        if let Some(next_key) = maybe_next_key {
            if self
                .args
                .prefix
                .clone()
                .map(|filter_key| next_key.starts_with(&filter_key))
                .unwrap_or(false)
            {
                self.args.start_at = Some(next_key.clone());
                Some(Ok(next_key))
            } else {
                self.complete = true;
                None
            }
        } else {
            self.complete = true;
            None
        }
    }

    fn next_pair(
        &mut self,
        backend: &Self::Backend,
    ) -> Option<Result<(sp_state_machine::StorageKey, sp_state_machine::StorageValue), Self::Error>>
    {
        use sp_state_machine::Backend;

        let remote_fetch =
            |key: Option<StorageKey>, start_key: Option<StorageKey>, block: Option<Block::Hash>| {
                backend.rpc().and_then(|rpc| rpc.storage_keys_paged(key, 5, start_key, block).ok()).and_then(|keys| keys.first().map(|key| key.clone()))
            };

        let prefix = self.args.prefix.clone().map(|k| StorageKey(k));
        let start_key = self.args.start_at.clone().map(|k| StorageKey(k));

        let maybe_next_key = if backend.before_fork {
            // If RPC client is available, fetch remotely
            if backend.rpc().is_some() {
                remote_fetch(prefix, start_key, backend.block_hash)
            } else {
                // No RPC client, use local DB
                let mut iter_args = sp_state_machine::backend::IterArgs::default();
                iter_args.prefix = self.args.prefix.as_deref();
                iter_args.start_at = self.args.start_at.as_deref();
                iter_args.start_at_exclusive = true;
                iter_args.stop_on_incomplete_database = true;

                let readable_db = backend.db.read();
                readable_db
                    .raw_iter(iter_args)
                    .map(|mut iter| iter.next_key(&readable_db))
                    .map(|op| op.and_then(|result| result.ok()))
                    .ok()
                    .flatten()
            }
        } else {
            let mut iter_args = sp_state_machine::backend::IterArgs::default();
            iter_args.prefix = self.args.prefix.as_deref();
            iter_args.start_at = self.args.start_at.as_deref();
            iter_args.start_at_exclusive = true;
            iter_args.stop_on_incomplete_database = true;

            let readable_db = backend.db.read();
            let next_storage_key = readable_db
                .raw_iter(iter_args)
                .map(|mut iter| iter.next_key(&readable_db))
                .map(|op| op.and_then(|result| result.ok()))
                .ok()
                .flatten();

            // IMPORTANT: free storage read lock
            drop(readable_db);

            let removed_key = start_key
                .clone()
                .or(prefix.clone())
                .map(|key| backend.removed_keys.read().contains_key(&key.0))
                .unwrap_or(false);
            if next_storage_key.is_none() && !removed_key {
                let maybe_next_key = if backend.rpc().is_some() {
                    remote_fetch(prefix, start_key, Some(backend.fork_block))
                } else {
                    None
                };
                match maybe_next_key {
                    Some(key) if !backend.removed_keys.read().contains_key(&key) => Some(key),
                    _ => None,
                }
            } else {
                next_storage_key
            }
        };

        log::trace!(
            target: super::LAZY_LOADING_LOG_TARGET,
            "next_pair: (prefix: {:?}, start_at: {:?}, next_key: {:?})",
            self.args.prefix.clone().map(|key| hex::encode(key)),
            self.args.start_at.clone().map(|key| hex::encode(key)),
            maybe_next_key.clone().map(|key| hex::encode(key))
        );

        let maybe_value = maybe_next_key
            .clone()
            .and_then(|key| (*backend).storage(key.as_slice()).ok())
            .flatten();

        if let Some(next_key) = maybe_next_key {
            if self
                .args
                .prefix
                .clone()
                .map(|filter_key| next_key.starts_with(&filter_key))
                .unwrap_or(false)
            {
                self.args.start_at = Some(next_key.clone());

                match maybe_value {
                    Some(value) => Some(Ok((next_key, value))),
                    _ => None,
                }
            } else {
                self.complete = true;
                None
            }
        } else {
            self.complete = true;
            None
        }
    }

    fn was_complete(&self) -> bool {
        self.complete
    }
}

#[derive(Debug, Clone)]
pub struct ForkedLazyBackend<Block: BlockT + DeserializeOwned> {
    rpc_client: Option<Arc<dyn RPCClient<Block>>>,
    block_hash: Option<Block::Hash>,
    fork_block: Block::Hash,
    pub(crate) db: Arc<parking_lot::RwLock<sp_state_machine::InMemoryBackend<HashingFor<Block>>>>,
    pub(crate) removed_keys: Arc<parking_lot::RwLock<HashMap<Vec<u8>, ()>>>,
    before_fork: bool,
}

impl<Block: BlockT + DeserializeOwned> ForkedLazyBackend<Block> {
    fn update_storage(&self, key: &[u8], value: &Option<Vec<u8>>) {
        if let Some(val) = value {
            let mut entries: HashMap<Option<ChildInfo>, StorageCollection> = Default::default();
            entries.insert(None, vec![(key.to_vec(), Some(val.clone()))]);

            self.db.write().insert(entries, StateVersion::V1);
        }
    }

    #[inline]
    fn rpc(&self) -> Option<&dyn RPCClient<Block>> {
        self.rpc_client.as_deref()
    }
}

impl<Block: BlockT + DeserializeOwned> sp_state_machine::Backend<HashingFor<Block>>
    for ForkedLazyBackend<Block>
{
    type Error = <DbState<Block> as sp_state_machine::Backend<HashingFor<Block>>>::Error;
    type TrieBackendStorage = PrefixedMemoryDB<HashingFor<Block>>;
    type RawIter = RawIter<Block>;

    fn storage(&self, key: &[u8]) -> Result<Option<sp_state_machine::StorageValue>, Self::Error> {
        let remote_fetch = |block: Option<Block::Hash>| -> Option<Vec<u8>> {
            self.rpc()
                .and_then(|rpc| rpc.storage(StorageKey(key.to_vec()), block).ok())
                .flatten()
                .map(|v| v.0)
        };

        // When before_fork and no RPC client, return None (no data available)
        if self.before_fork {
            if self.rpc().is_some() {
                return Ok(remote_fetch(self.block_hash));
            } else {
                return Ok(None);
            }
        }

        let readable_db = self.db.read();
        let maybe_storage = readable_db.storage(key);
        let value = match maybe_storage {
            Ok(Some(data)) => Some(data),
            _ if !self.removed_keys.read().contains_key(key) => {
                // Only try remote fetch if RPC client is available
                let result = if self.rpc().is_some() {
                    remote_fetch(Some(self.fork_block))
                } else {
                    None
                };

                // Cache state
                drop(readable_db);
                self.update_storage(key, &result);

                result
            }
            _ => None,
        };

        Ok(value)
    }

    fn storage_hash(
        &self,
        key: &[u8],
    ) -> Result<Option<<HashingFor<Block> as sp_core::Hasher>::Out>, Self::Error> {
        let remote_fetch = |block: Option<Block::Hash>| -> Result<
            Option<<HashingFor<Block> as sp_core::Hasher>::Out>,
            Self::Error,
        > {
            match self.rpc() {
                Some(rpc) => rpc
                    .storage_hash(StorageKey(key.to_vec()), block)
                    .map_err(|e| format!("Failed to fetch storage hash from RPC: {:?}", e).into()),
                None => Ok(None),
            }
        };

        // When before_fork and no RPC client, return None
        if self.before_fork {
            if self.rpc().is_some() {
                return remote_fetch(self.block_hash);
            } else {
                return Ok(None);
            }
        }

        let storage_hash = self.db.read().storage_hash(key);
        match storage_hash {
            Ok(Some(hash)) => Ok(Some(hash)),
            _ if !self.removed_keys.read().contains_key(key) => {
                if self.rpc().is_some() {
                    remote_fetch(Some(self.fork_block))
                } else {
                    Ok(None)
                }
            }
            _ => Ok(None),
        }
    }

    fn closest_merkle_value(
        &self,
        _key: &[u8],
    ) -> Result<
        Option<sp_trie::MerkleValue<<HashingFor<Block> as sp_core::Hasher>::Out>>,
        Self::Error,
    > {
        unimplemented!("closest_merkle_value: unsupported feature for lazy loading")
    }

    fn child_closest_merkle_value(
        &self,
        _child_info: &sp_storage::ChildInfo,
        _key: &[u8],
    ) -> Result<
        Option<sp_trie::MerkleValue<<HashingFor<Block> as sp_core::Hasher>::Out>>,
        Self::Error,
    > {
        unimplemented!("child_closest_merkle_value: unsupported feature for lazy loading")
    }

    fn child_storage(
        &self,
        _child_info: &sp_storage::ChildInfo,
        _key: &[u8],
    ) -> Result<Option<sp_state_machine::StorageValue>, Self::Error> {
        unimplemented!("child_storage: unsupported feature for lazy loading");
    }

    fn child_storage_hash(
        &self,
        _child_info: &sp_storage::ChildInfo,
        _key: &[u8],
    ) -> Result<Option<<HashingFor<Block> as sp_core::Hasher>::Out>, Self::Error> {
        unimplemented!("child_storage_hash: unsupported feature for lazy loading");
    }

    fn next_storage_key(
        &self,
        key: &[u8],
    ) -> Result<Option<sp_state_machine::StorageKey>, Self::Error> {
        let remote_fetch = |block: Option<Block::Hash>| {
            let start_key = Some(StorageKey(key.to_vec()));
            self.rpc().and_then(|rpc| rpc.storage_keys_paged(start_key.clone(), 2, None, block).ok()).and_then(|keys| keys.last().map(|key| key.clone()))
        };

        let maybe_next_key = if self.before_fork {
            // Before the fork checkpoint, fetch remotely if RPC client is available
            if self.rpc().is_some() {
                remote_fetch(self.block_hash)
            } else {
                // No RPC client, try local DB
                self.db.read().next_storage_key(key).ok().flatten()
            }
        } else {
            // Try to get the next storage key from the local DB
            let next_storage_key = self.db.read().next_storage_key(key);
            match next_storage_key {
                Ok(Some(next_key)) => Some(next_key),
                // If not found locally and key is not marked as removed, fetch remotely
                _ if !self.removed_keys.read().contains_key(key) => {
                    if self.rpc().is_some() {
                        remote_fetch(Some(self.fork_block))
                    } else {
                        None
                    }
                }
                // Otherwise, there's no next key
                _ => None,
            }
        }
        .filter(|next_key| next_key != key);

        log::trace!(
            target: super::LAZY_LOADING_LOG_TARGET,
            "next_storage_key: (key: {:?}, next_key: {:?})",
            hex::encode(key),
            maybe_next_key.clone().map(|key| hex::encode(key))
        );

        Ok(maybe_next_key)
    }

    fn next_child_storage_key(
        &self,
        _child_info: &sp_storage::ChildInfo,
        _key: &[u8],
    ) -> Result<Option<sp_state_machine::StorageKey>, Self::Error> {
        unimplemented!("next_child_storage_key: unsupported feature for lazy loading");
    }

    fn storage_root<'a>(
        &self,
        delta: impl Iterator<Item = (&'a [u8], Option<&'a [u8]>)>,
        state_version: StateVersion,
    ) -> (<HashingFor<Block> as sp_core::Hasher>::Out, BackendTransaction<HashingFor<Block>>)
    where
        <HashingFor<Block> as sp_core::Hasher>::Out: Ord,
    {
        self.db.read().storage_root(delta, state_version)
    }

    fn child_storage_root<'a>(
        &self,
        _child_info: &sp_storage::ChildInfo,
        _delta: impl Iterator<Item = (&'a [u8], Option<&'a [u8]>)>,
        _state_version: StateVersion,
    ) -> (<HashingFor<Block> as sp_core::Hasher>::Out, bool, BackendTransaction<HashingFor<Block>>)
    where
        <HashingFor<Block> as sp_core::Hasher>::Out: Ord,
    {
        unimplemented!("child_storage_root: unsupported in lazy loading")
    }

    fn raw_iter(&self, args: sp_state_machine::IterArgs<'_>) -> Result<Self::RawIter, Self::Error> {
        let mut clone: RawIterArgs = Default::default();
        clone.start_at_exclusive = args.start_at_exclusive.clone();
        clone.prefix = args.prefix.map(|v| v.to_vec());
        clone.start_at = args.start_at.map(|v| v.to_vec());

        Ok(RawIter::<Block> { args: clone, complete: false, _phantom: Default::default() })
    }

    fn register_overlay_stats(&self, stats: &sp_state_machine::StateMachineStats) {
        self.db.read().register_overlay_stats(stats)
    }

    fn usage_info(&self) -> sp_state_machine::UsageInfo {
        self.db.read().usage_info()
    }
}

impl<B: BlockT + DeserializeOwned> sp_state_machine::backend::AsTrieBackend<HashingFor<B>>
    for ForkedLazyBackend<B>
{
    type TrieBackendStorage = PrefixedMemoryDB<HashingFor<B>>;

    fn as_trie_backend(
        &self,
    ) -> &sp_state_machine::TrieBackend<Self::TrieBackendStorage, HashingFor<B>> {
        unimplemented!("`as_trie_backend` is not supported in lazy loading mode.")
    }
}

/// Lazy loading (In-memory) backend. Keeps all states and blocks in memory.
pub struct Backend<Block: BlockT + DeserializeOwned> {
    pub(crate) rpc_client: Option<Arc<dyn RPCClient<Block>>>,
    pub(crate) fork_checkpoint: Block::Header,
    states: parking_lot::RwLock<HashMap<Block::Hash, ForkedLazyBackend<Block>>>,
    pub(crate) blockchain: Blockchain<Block>,
    import_lock: parking_lot::RwLock<()>,
    pinned_blocks: parking_lot::RwLock<HashMap<Block::Hash, i64>>,
}

impl<Block: BlockT + DeserializeOwned> Backend<Block> {
    fn new(rpc_client: Option<Arc<dyn RPCClient<Block>>>, fork_checkpoint: Block::Header) -> Self {
        Backend {
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

impl<Block: BlockT + DeserializeOwned> backend::AuxStore for Backend<Block> {
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
            finalized_blocks: Default::default(),
            set_head: None,
            before_fork: false,
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

            let new_removed_keys = old_state.removed_keys.clone();
            for (key, value) in operation.storage_updates.clone() {
                if value.is_some() {
                    new_removed_keys.write().remove(&key.clone());
                } else {
                    new_removed_keys.write().insert(key.clone(), ());
                }
            }

            let new_db = old_state.db.clone();
            new_db
                .write()
                .insert(vec![(None::<ChildInfo>, operation.storage_updates)], StateVersion::V1);
            let new_state = ForkedLazyBackend {
                rpc_client: self.rpc_client.clone(),
                block_hash: Some(hash.clone()),
                fork_block: self.fork_checkpoint.hash(),
                db: new_db,
                removed_keys: new_removed_keys,
                before_fork: operation.before_fork,
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
            return Ok(ForkedLazyBackend::<Block> {
                rpc_client: self.rpc_client.clone(),
                block_hash: Some(hash),
                fork_block: self.fork_checkpoint.hash(),
                db: Default::default(),
                removed_keys: Default::default(),
                before_fork: true,
            });
        }

        let (backend, should_write) =
            self.states.read().get(&hash).cloned().map(|state| Ok((state, false))).unwrap_or_else(
                || {
                    self.rpc().and_then(|rpc| rpc.header(Some(hash)).ok()).flatten()
                        .ok_or(sp_blockchain::Error::UnknownBlock(
                            format!("Failed to fetch block header: {:?}", hash).into(),
                        ))
                        .map(|header| {
                            let checkpoint = self.fork_checkpoint.clone();
                            let state = if header.number().gt(checkpoint.number()) {
                                let parent = self
                                    .state_at(*header.parent_hash(), TrieCacheContext::Trusted)
                                    .ok();

                                ForkedLazyBackend::<Block> {
                                    rpc_client: self.rpc_client.clone(),
                                    block_hash: Some(hash),
                                    fork_block: checkpoint.hash(),
                                    db: parent.clone().map_or(Default::default(), |p| p.db),
                                    removed_keys: parent
                                        .map_or(Default::default(), |p| p.removed_keys),
                                    before_fork: false,
                                }
                            } else {
                                ForkedLazyBackend::<Block> {
                                    rpc_client: self.rpc_client.clone(),
                                    block_hash: Some(hash),
                                    fork_block: checkpoint.hash(),
                                    db: Default::default(),
                                    removed_keys: Default::default(),
                                    before_fork: true,
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
        _n: NumberFor<Block>,
        _revert_finalized: bool,
    ) -> sp_blockchain::Result<(NumberFor<Block>, HashSet<Block::Hash>)> {
        Ok((Zero::zero(), HashSet::new()))
    }

    fn remove_leaf_block(&self, _hash: Block::Hash) -> sp_blockchain::Result<()> {
        Ok(())
    }

    fn get_import_lock(&self) -> &parking_lot::RwLock<()> {
        &self.import_lock
    }

    fn requires_full_sync(&self) -> bool {
        false
    }

    fn pin_block(&self, hash: <Block as BlockT>::Hash) -> blockchain::Result<()> {
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

/// Check that genesis storage is valid.
pub fn check_genesis_storage(storage: &Storage) -> sp_blockchain::Result<()> {
    if storage.top.iter().any(|(k, _)| well_known_keys::is_child_storage_key(k)) {
        return Err(sp_blockchain::Error::InvalidState);
    }

    if storage
        .children_default
        .keys()
        .any(|child_key| !well_known_keys::is_child_storage_key(child_key))
    {
        return Err(sp_blockchain::Error::InvalidState);
    }

    Ok(())
}

pub fn new_backend<Block>(
    rpc_client: Option<Arc<dyn RPCClient<Block>>>,
    checkpoint: Block::Header,
) -> Result<Arc<Backend<Block>>, Error>
where
    Block: BlockT + DeserializeOwned,
    Block::Hash: From<H256>,
{
    let backend = Arc::new(Backend::new(rpc_client, checkpoint));
    Ok(backend)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mock_rpc::{RPC, TestBlock, TestHeader};
    use polkadot_sdk::{
        sc_client_api::{Backend as BackendT, StateBackend},
        sp_runtime::{
            OpaqueExtrinsic,
            traits::{BlakeTwo256, Header as HeaderT},
        },
        sp_state_machine::StorageIterator,
        sp_storage::StorageData,
    };
    use std::{
        collections::BTreeMap,
        sync::atomic::{AtomicUsize, Ordering},
    };

    mod mock_rpc {
        use super::*;
        use crate::substrate_node::lazy_loading::rpc_client;
        use polkadot_sdk::sp_runtime::{
            generic::{Block as GenericBlock, Header, SignedBlock},
            traits::Header as HeaderT,
        };

        pub type TestHashing = BlakeTwo256;
        pub type TestHeader<N = u32> = Header<N, TestHashing>;
        pub type TestExtrinsic = OpaqueExtrinsic;
        pub type TestBlock<N = u32> = GenericBlock<TestHeader<N>, TestExtrinsic>;

        #[derive(Default, Debug)]
        pub struct Counters {
            pub storage_calls: AtomicUsize,
            pub storage_hash_calls: AtomicUsize,
            pub storage_keys_paged_calls: AtomicUsize,
            pub header_calls: AtomicUsize,
            pub block_calls: AtomicUsize,
            pub block_hash_calls: AtomicUsize,
        }

        /// Mockable RPC with interior mutability.
        #[derive(Clone, Default, Debug)]
        pub struct RPC<Block: BlockT + DeserializeOwned> {
            pub counters: std::sync::Arc<Counters>,
            /// storage[(block_hash, key)] = value
            pub storage: std::sync::Arc<
                parking_lot::RwLock<BTreeMap<(Block::Hash, StorageKey), StorageData>>,
            >,
            /// storage_hash[(block_hash, key)] = hash
            pub storage_hashes: std::sync::Arc<
                parking_lot::RwLock<BTreeMap<(Block::Hash, StorageKey), Block::Hash>>,
            >,
            /// storage_keys_paged[(block_hash, (prefix,start))] = Vec<keys>
            pub storage_keys_pages: std::sync::Arc<
                parking_lot::RwLock<BTreeMap<(Block::Hash, Vec<u8>), Vec<StorageKey>>>,
            >,
            /// headers[hash] = header
            pub headers: std::sync::Arc<parking_lot::RwLock<BTreeMap<Block::Hash, Block::Header>>>,
            /// blocks[hash] = SignedBlock
            pub blocks:
                std::sync::Arc<parking_lot::RwLock<BTreeMap<Block::Hash, SignedBlock<Block>>>>,
            /// block_hash_by_number[n] = hash
            pub block_hash_by_number:
                std::sync::Arc<parking_lot::RwLock<BTreeMap<u64, Block::Hash>>>,
        }

        impl<Block: BlockT + DeserializeOwned> RPC<Block> {
            pub fn new() -> Self {
                Self {
                    counters: std::sync::Arc::new(Counters::default()),
                    storage: std::sync::Arc::new(parking_lot::RwLock::new(BTreeMap::new())),
                    storage_hashes: std::sync::Arc::new(parking_lot::RwLock::new(BTreeMap::new())),
                    storage_keys_pages: std::sync::Arc::new(parking_lot::RwLock::new(
                        BTreeMap::new(),
                    )),
                    headers: std::sync::Arc::new(parking_lot::RwLock::new(BTreeMap::new())),
                    blocks: std::sync::Arc::new(parking_lot::RwLock::new(BTreeMap::new())),
                    block_hash_by_number: std::sync::Arc::new(parking_lot::RwLock::new(
                        BTreeMap::new(),
                    )),
                }
            }

            pub fn put_storage(&self, at: Block::Hash, key: StorageKey, val: StorageData) {
                self.storage.write().insert((at, key), val);
            }
            pub fn put_storage_keys_page(
                &self,
                at: Block::Hash,
                prefix: Vec<u8>,
                keys: Vec<StorageKey>,
            ) {
                self.storage_keys_pages.write().insert((at, prefix), keys);
            }
            pub fn put_header(&self, h: Block::Header) {
                self.headers.write().insert(h.hash(), h);
            }
            pub fn put_block(&self, block: Block, just: Option<Justifications>) {
                let full = SignedBlock { block, justifications: just };
                self.blocks.write().insert(full.block.header().hash(), full);
            }
        }

        impl<Block: BlockT + DeserializeOwned> RPCClient<Block> for RPC<Block> {
            fn storage(
                &self,
                key: StorageKey,
                at: Option<Block::Hash>,
            ) -> Result<Option<StorageData>, jsonrpsee::core::ClientError> {
                self.counters.storage_calls.fetch_add(1, Ordering::Relaxed);
                let map = self.storage.read();
                Ok(map.get(&(at.unwrap_or_default(), key.clone())).cloned())
            }

            fn storage_hash(
                &self,
                key: StorageKey,
                at: Option<Block::Hash>,
            ) -> Result<Option<Block::Hash>, jsonrpsee::core::ClientError> {
                self.counters.storage_hash_calls.fetch_add(1, Ordering::Relaxed);
                let bh = at.unwrap_or_default();
                let map = self.storage_hashes.read();
                Ok(map.get(&(bh, key.clone())).cloned())
            }

            fn storage_keys_paged(
                &self,
                key: Option<StorageKey>,
                count: u32,
                start_key: Option<StorageKey>,
                at: Option<Block::Hash>,
            ) -> Result<Vec<sp_state_machine::StorageKey>, jsonrpsee::core::ClientError>
            {
                self.counters.storage_keys_paged_calls.fetch_add(1, Ordering::Relaxed);

                use std::cmp::min;

                let bh = at.unwrap_or_default();
                let prefix = key.map(|k| k.0).unwrap_or_default(); // ✅ usar el prefix correcto
                let start = start_key.map(|k| k.0);

                let map = self.storage_keys_pages.read();
                let mut all = map.get(&(bh, prefix.clone())).cloned().unwrap_or_default();

                // Asegurar orden determinista (lexicográfico por bytes)
                all.sort_by(|a, b| a.0.cmp(&b.0));

                // Filtrar por prefix (defensivo)
                let mut filtered: Vec<StorageKey> =
                    all.into_iter().filter(|k| k.0.starts_with(&prefix)).collect();

                // Aplicar start_key EXCLUSIVO: devolver solo las > start
                if let Some(s) = start {
                    // buscar posición exacta, si existe
                    if let Some(pos) = filtered.iter().position(|k| k.0 == s) {
                        filtered = filtered.into_iter().skip(pos + 1).collect();
                    } else {
                        // si no está, devolver la primera mayor
                        filtered = filtered.into_iter().filter(|k| k.0 > s).collect();
                    }
                }

                // Aplicar count
                let take = min(filtered.len(), count as usize);
                Ok(filtered.into_iter().take(take).map(|k| k.0).collect())
            }

            fn header(
                &self,
                at: Option<Block::Hash>,
            ) -> Result<Option<Block::Header>, jsonrpsee::core::ClientError> {
                self.counters.header_calls.fetch_add(1, Ordering::Relaxed);
                let key = at.unwrap_or_default();
                let raw = self.headers.read().get(&key).cloned();
                Ok(raw)
            }

            fn block(
                &self,
                hash: Option<Block::Hash>,
            ) -> Result<Option<SignedBlock<Block>>, jsonrpsee::core::ClientError> {
                self.counters.block_calls.fetch_add(1, Ordering::Relaxed);
                let key = hash.unwrap_or_default();
                let raw = self.blocks.read().get(&key).cloned();
                Ok(raw)
            }

            fn block_hash(
                &self,
                num: Option<NumberFor<Block>>,
            ) -> Result<Option<Block::Hash>, jsonrpsee::core::ClientError> {
                todo!()
            }

            fn system_chain(&self) -> Result<String, jsonrpsee::core::ClientError> {
                todo!()
            }

            fn system_properties(
                &self,
            ) -> Result<polkadot_sdk::sc_chain_spec::Properties, jsonrpsee::core::ClientError>
            {
                todo!()
            }
        }
    }

    type N = u32;
    type TestBlockT = TestBlock<N>;

    fn make_header(number: N, parent: <TestBlock as BlockT>::Hash) -> TestHeader<N> {
        TestHeader::new(
            number.into(),
            Default::default(),
            Default::default(),
            parent,
            Default::default(),
        )
    }

    fn make_block(
        number: N,
        parent: <TestBlock as BlockT>::Hash,
        xts: Vec<OpaqueExtrinsic>,
    ) -> TestBlock<N> {
        let header = make_header(number, parent);
        TestBlock::new(header, xts)
    }

    fn checkpoint(n: N) -> TestHeader<N> {
        make_header(n, Default::default())
    }

    #[test]
    fn before_fork_reads_remote_only() {
        let rpc = std::sync::Arc::new(RPC::new());
        // fork checkpoint at #100
        let cp = checkpoint(100);
        let backend = Backend::<TestBlockT>::new(Some(rpc.clone()), cp.clone());

        // state_at(Default::default()) => before_fork=true
        let state = backend.state_at(Default::default(), TrieCacheContext::Trusted).unwrap();

        let key = b":foo".to_vec();
        // prepare remote value at "block_hash = Default::default()"
        let at = Default::default();
        rpc.put_storage(at, StorageKey(key.clone()), StorageData(b"bar".to_vec()));

        // read storage
        let v1 = state.storage(&key).unwrap();
        assert_eq!(v1, Some(b"bar".to_vec()));

        // not cached in DB: second read still goes to RPC
        let v2 = state.storage(&key).unwrap();
        assert_eq!(v2, Some(b"bar".to_vec()));
        assert!(rpc.counters.storage_calls.load(std::sync::atomic::Ordering::Relaxed) >= 2);
    }

    #[test]
    fn after_fork_first_fetch_caches_subsequent_hits_local() {
        let rpc = std::sync::Arc::new(RPC::new());
        let cp = checkpoint(10);
        let backend = Backend::<TestBlockT>::new(Some(rpc.clone()), cp.clone());

        // Build a block #11 > checkpoint (#10), with parent #10
        let parent = cp.hash();
        let b11 = make_block(11, parent, vec![]);
        let h11 = b11.header.hash();

        rpc.put_header(b11.header.clone());
        rpc.put_block(b11.clone(), None);

        // remote storage at fork block (checkpoint hash)
        let fork_hash = cp.hash();
        let key = b":k".to_vec();
        rpc.put_storage(fork_hash, StorageKey(key.clone()), StorageData(b"v".to_vec()));

        // Grab state_at(#11): after_fork=false; local DB empty
        let state = backend.state_at(h11, TrieCacheContext::Trusted).unwrap();

        // First read fetches remote and caches
        let v1 = state.storage(&key).unwrap();
        assert_eq!(v1, Some(b"v".to_vec()));

        // Mutate RPC to detect second call (remove remote value)
        // If second read still tries RPC, it would return None; but it should come from cache.
        // So we do not change the mock; instead, assert RPC call count increases only once.
        let calls_before = rpc.counters.storage_calls.load(std::sync::atomic::Ordering::Relaxed);
        let _ = state.storage(&key).unwrap();
        let calls_after = rpc.counters.storage_calls.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(calls_before, calls_after, "second hit should be served from cache");
    }

    #[test]
    fn removed_keys_prevents_remote_fetch() {
        let rpc = std::sync::Arc::new(RPC::new());
        let cp = checkpoint(5);
        let backend = Backend::<TestBlockT>::new(Some(rpc.clone()), cp.clone());

        // make block #6
        let b6 = make_block(6, cp.hash(), vec![]);
        rpc.put_header(b6.header.clone());
        rpc.put_block(b6.clone(), None);
        let state = backend.state_at(b6.header.hash(), TrieCacheContext::Trusted).unwrap();

        // mark key as removed
        let key = b":dead".to_vec();
        state.removed_keys.write().insert(key.clone(), ());

        // Even if remote has a value, backend must not fetch it
        rpc.put_storage(cp.hash(), StorageKey(key.clone()), StorageData(b"ghost".to_vec()));
        let calls_before = rpc.counters.storage_calls.load(std::sync::atomic::Ordering::Relaxed);
        let v = state.storage(&key).unwrap();
        let calls_after = rpc.counters.storage_calls.load(std::sync::atomic::Ordering::Relaxed);

        assert!(v.is_none());
        assert_eq!(calls_before, calls_after, "should not call RPC for removed keys");
    }

    #[test]
    fn raw_iter_merges_local_then_remote() {
        let rpc = std::sync::Arc::new(RPC::new());
        let cp = checkpoint(7);
        let backend = Backend::<TestBlockT>::new(Some(rpc.clone()), cp.clone());

        // block #8
        let b8 = make_block(8, cp.hash(), vec![]);
        rpc.put_header(b8.header.clone());
        rpc.put_block(b8.clone(), None);
        let state = backend.state_at(b8.header.hash(), TrieCacheContext::Trusted).unwrap();

        // Preload local DB with key "a1"
        state.update_storage(b"a1", &Some(b"v1".to_vec()));

        // Remote has "a2" under same prefix at fork block
        rpc.put_storage_keys_page(
            cp.hash(),
            b"a".to_vec(),
            vec![StorageKey(b"a1".to_vec()), StorageKey(b"a2".to_vec())],
        );
        rpc.put_storage(cp.hash(), StorageKey(b"a2".to_vec()), StorageData(b"v2".to_vec()));

        let mut args = sp_state_machine::IterArgs::default();
        args.prefix = Some(&b"a"[..]);
        let mut it = state.raw_iter(args).unwrap();

        // next_pair should return ("a1","v1") from local
        let p1 = it.next_pair(&state).unwrap().unwrap();
        assert_eq!(p1.0, b"a1".to_vec());
        assert_eq!(p1.1, b"v1".to_vec());

        // next_pair should now bring remote ("a2","v2")
        let p2 = it.next_pair(&state).unwrap().unwrap();
        assert_eq!(p2.0, b"a2".to_vec());
        assert_eq!(p2.1, b"v2".to_vec());

        // done
        assert!(it.next_pair(&state).is_none());
        assert!(it.was_complete());
    }

    #[test]
    fn blockchain_header_and_number_are_cached() {
        let rpc = std::sync::Arc::new(RPC::new());
        let cp = checkpoint(3);
        let backend = Backend::<TestBlockT>::new(Some(rpc.clone()), cp.clone());
        let chain = backend.blockchain();

        // prepare one block w/ extrinsics
        let xts: Vec<OpaqueExtrinsic> = vec![];
        let b4 = make_block(4, cp.hash(), xts.clone());
        let h4 = b4.header().hash();
        rpc.put_block(b4.clone(), None);

        // first header() fetches RPC and caches as Full
        let h = chain.header(h4).unwrap().unwrap();
        assert_eq!(h.hash(), h4);

        // number() should now return from cache (no extra RPC needed)
        let calls_before = rpc.counters.block_calls.load(std::sync::atomic::Ordering::Relaxed);
        let number = chain.number(h4).unwrap().unwrap();
        let calls_after = rpc.counters.block_calls.load(std::sync::atomic::Ordering::Relaxed);

        assert_eq!(number, 4);
        assert_eq!(
            calls_before, calls_after,
            "number() should be served from cache after header()"
        );
    }
}
