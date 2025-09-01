use codec::Encode;
use polkadot_sdk::{
    parachains_common::{Hash, Header},
    sc_client_api::{
        self, backend::Finalizer, blockchain, client::ClientInfo,
        execution_extensions::ExecutionExtensions, Backend as BackendT, BlockBackend,
        BlockchainEvents, ClientImportOperation, ExecutorProvider, KeysIter, PairsIter,
        ProofProvider, StorageProvider, UsageProvider,
    },
    sc_consensus::{BlockCheckParams, BlockImport, BlockImportParams, ImportResult},
    sc_executor::{RuntimeVersion, WasmExecutor},
    sc_service,
    sp_api::{self, ApiRef, CallApiAt, CallApiAtParams, ConstructRuntimeApi, ProvideRuntimeApi},
    sp_blockchain::{self, CachedHeaderMetadata, HeaderBackend, HeaderMetadata},
    sp_consensus,
    sp_core::storage::StorageData,
    sp_externalities, sp_io,
    sp_runtime::{
        generic::{BlockId, SignedBlock},
        traits::{BlockIdTo, NumberFor},
        Justification, Justifications,
    },
    sp_state_machine::{
        CompactProof, KeyValueStates, KeyValueStorageLevel, StorageKey, StorageProof, StorageValue,
    },
    sp_storage::{self, ChildInfo},
    sp_trie::MerkleValue,
};
use std::collections::HashMap;
use substrate_runtime::{Block, RuntimeApi, UncheckedExtrinsic};

use crate::substrate_node::service::Backend;

type InnerClient =
    sc_service::TFullClient<Block, RuntimeApi, WasmExecutor<sp_io::SubstrateHostFunctions>>;

pub struct Client {
    inner: InnerClient,
    overrides: HashMap<StorageKey, StorageValue>,
}

impl Client {
    pub fn new(inner: InnerClient) -> Self {
        let mut overrides = HashMap::new();

        overrides.insert(
            b"0xf0c365c3cf59d671eb72da0e7a4113c49f1f0515f462cdcf84e0f1d6045dfcbb".to_vec(),
            b"".to_vec(),
        );

        Self { inner, overrides }
    }

    /// Insert a storage override.
    pub fn insert_storage_override(&mut self, key: StorageKey, value: StorageValue) {
        self.overrides.insert(key, value);
    }

    /// Remove a storage override.
    pub fn remove_storage_override(&mut self, key: &StorageKey) {
        self.overrides.remove(key);
    }

    /// Clear all storage overrides.
    pub fn clear_storage_overrides(&mut self) {
        self.overrides.clear();
    }
}

impl ProvideRuntimeApi<Block> for Client {
    type Api = <RuntimeApi as ConstructRuntimeApi<Block, Self>>::RuntimeApi;

    fn runtime_api(&self) -> ApiRef<'_, Self::Api> {
        RuntimeApi::construct_runtime_api(self)
    }
}

impl BlockBackend<Block> for Client {
    fn block_body(&self, hash: Hash) -> sp_blockchain::Result<Option<Vec<UncheckedExtrinsic>>> {
        self.inner.body(hash)
    }

    fn block(&self, hash: Hash) -> sp_blockchain::Result<Option<SignedBlock<Block>>> {
        self.inner.block(hash)
    }

    fn block_status(&self, hash: Hash) -> sp_blockchain::Result<sp_consensus::BlockStatus> {
        self.inner.block_status(hash)
    }

    fn justifications(&self, hash: Hash) -> sp_blockchain::Result<Option<Justifications>> {
        self.inner.justifications(hash)
    }

    fn block_hash(&self, number: NumberFor<Block>) -> sp_blockchain::Result<Option<Hash>> {
        self.inner.block_hash(number)
    }

    fn indexed_transaction(&self, hash: Hash) -> sp_blockchain::Result<Option<Vec<u8>>> {
        self.inner.indexed_transaction(hash)
    }

    fn has_indexed_transaction(&self, hash: Hash) -> sp_blockchain::Result<bool> {
        self.inner.has_indexed_transaction(hash)
    }

    fn block_indexed_body(&self, hash: Hash) -> sp_blockchain::Result<Option<Vec<Vec<u8>>>> {
        self.inner.block_indexed_body(hash)
    }

    fn requires_full_sync(&self) -> bool {
        self.inner.requires_full_sync()
    }
}

impl BlockchainEvents<Block> for Client {
    fn import_notification_stream(&self) -> sc_client_api::ImportNotifications<Block> {
        self.inner.import_notification_stream()
    }

    fn finality_notification_stream(&self) -> sc_client_api::FinalityNotifications<Block> {
        self.inner.finality_notification_stream()
    }

    fn storage_changes_notification_stream(
        &self,
        filter_keys: Option<&[sp_storage::StorageKey]>,
        child_filter_keys: Option<&[(sp_storage::StorageKey, Option<Vec<sp_storage::StorageKey>>)]>,
    ) -> sp_blockchain::Result<sc_client_api::StorageEventStream<Hash>> {
        self.inner.storage_changes_notification_stream(filter_keys, child_filter_keys)
    }

    fn every_import_notification_stream(&self) -> sc_client_api::ImportNotifications<Block> {
        self.inner.every_import_notification_stream()
    }
}

impl HeaderBackend<Block> for Client {
    fn header(&self, hash: Hash) -> sp_blockchain::Result<Option<Header>> {
        self.inner.header(hash)
    }

    fn info(&self) -> blockchain::Info<Block> {
        self.inner.info()
    }

    fn status(&self, hash: Hash) -> sp_blockchain::Result<blockchain::BlockStatus> {
        self.inner.status(hash)
    }

    fn number(&self, hash: Hash) -> sp_blockchain::Result<Option<NumberFor<Block>>> {
        self.inner.number(hash)
    }

    fn hash(&self, number: NumberFor<Block>) -> sp_blockchain::Result<Option<Hash>> {
        self.inner.hash(number)
    }
}

impl HeaderMetadata<Block> for Client {
    type Error = <InnerClient as HeaderMetadata<Block>>::Error;

    fn header_metadata(&self, hash: Hash) -> Result<CachedHeaderMetadata<Block>, Self::Error> {
        self.inner.header_metadata(hash)
    }

    fn insert_header_metadata(&self, hash: Hash, metadata: CachedHeaderMetadata<Block>) {
        self.inner.insert_header_metadata(hash, metadata)
    }

    fn remove_header_metadata(&self, hash: Hash) {
        self.inner.remove_header_metadata(hash)
    }
}

impl ExecutorProvider<Block> for Client {
    type Executor = <InnerClient as ExecutorProvider<Block>>::Executor;

    fn executor(&self) -> &Self::Executor {
        self.inner.executor()
    }

    fn execution_extensions(&self) -> &ExecutionExtensions<Block> {
        self.inner.execution_extensions()
    }
}

impl CallApiAt<Block> for Client {
    type StateBackend = <InnerClient as CallApiAt<Block>>::StateBackend;

    fn call_api_at(&self, params: CallApiAtParams<'_, Block>) -> Result<Vec<u8>, sp_api::ApiError> {
        tracing::error!("Calling API method: {}", params.function);
        // if params.function == "Core_initialize_block" {
        //     params.overlayed_changes.borrow_mut().set_storage(
        //         hex::decode("c2261276cc9d1f8598ea4b6a74b15c2f57c875e4cff74148e4628f264b974c80")
        //             .unwrap(),
        //         Some(0u128.encode()),
        //     );
        // }

        // if params.function == "BlockBuilder_finalize_block" {
        //     params.overlayed_changes.borrow_mut().set_storage(
        //         hex::decode("f0c365c3cf59d671eb72da0e7a4113c49f1f0515f462cdcf84e0f1d6045dfcbb")
        //             .unwrap(),
        //         Some(1893456000u64.encode()),
        //     );
        // }

        // for (key, val) in self.overrides.clone() {
        //     params.overlayed_changes.borrow_mut().set_storage(key, None);
        // }
        self.inner.call_api_at(params)
    }

    fn runtime_version_at(&self, hash: Hash) -> Result<RuntimeVersion, sp_api::ApiError> {
        CallApiAt::runtime_version_at(&self.inner, hash)
    }

    fn state_at(&self, at: Hash) -> Result<Self::StateBackend, sp_api::ApiError> {
        CallApiAt::state_at(&self.inner, at)
    }

    fn initialize_extensions(
        &self,
        at: Hash,
        extensions: &mut sp_externalities::Extensions,
    ) -> Result<(), sp_api::ApiError> {
        self.inner.initialize_extensions(at, extensions)
    }
}

impl ProofProvider<Block> for Client {
    fn read_proof(
        &self,
        hash: Hash,
        keys: &mut dyn Iterator<Item = &[u8]>,
    ) -> sp_blockchain::Result<StorageProof> {
        self.inner.read_proof(hash, keys)
    }

    fn read_child_proof(
        &self,
        hash: Hash,
        child_info: &ChildInfo,
        keys: &mut dyn Iterator<Item = &[u8]>,
    ) -> sp_blockchain::Result<StorageProof> {
        self.inner.read_child_proof(hash, child_info, keys)
    }

    fn execution_proof(
        &self,
        hash: Hash,
        method: &str,
        call_data: &[u8],
    ) -> sp_blockchain::Result<(Vec<u8>, StorageProof)> {
        self.inner.execution_proof(hash, method, call_data)
    }

    fn read_proof_collection(
        &self,
        hash: Hash,
        start_key: &[Vec<u8>],
        size_limit: usize,
    ) -> sp_blockchain::Result<(CompactProof, u32)> {
        self.inner.read_proof_collection(hash, start_key, size_limit)
    }

    fn storage_collection(
        &self,
        hash: Hash,
        start_key: &[Vec<u8>],
        size_limit: usize,
    ) -> sp_blockchain::Result<Vec<(KeyValueStorageLevel, bool)>> {
        self.inner.storage_collection(hash, start_key, size_limit)
    }

    fn verify_range_proof(
        &self,
        root: Hash,
        proof: CompactProof,
        start_key: &[Vec<u8>],
    ) -> sp_blockchain::Result<(KeyValueStates, usize)> {
        self.inner.verify_range_proof(root, proof, start_key)
    }
}

impl BlockIdTo<Block> for Client {
    type Error = <InnerClient as BlockIdTo<Block>>::Error;

    fn to_hash(&self, block_id: &BlockId<Block>) -> sp_blockchain::Result<Option<Hash>> {
        self.inner.to_hash(block_id)
    }

    fn to_number(
        &self,
        block_id: &BlockId<Block>,
    ) -> sp_blockchain::Result<Option<NumberFor<Block>>> {
        self.inner.to_number(block_id)
    }
}

impl StorageProvider<Block, Backend> for Client {
    fn storage_keys(
        &self,
        hash: Hash,
        prefix: Option<&sp_storage::StorageKey>,
        start_key: Option<&sp_storage::StorageKey>,
    ) -> sp_blockchain::Result<KeysIter<<Backend as BackendT<Block>>::State, Block>> {
        self.inner.storage_keys(hash, prefix, start_key)
    }

    fn child_storage_keys(
        &self,
        hash: Hash,
        child_info: ChildInfo,
        prefix: Option<&sp_storage::StorageKey>,
        start_key: Option<&sp_storage::StorageKey>,
    ) -> sp_blockchain::Result<KeysIter<<Backend as BackendT<Block>>::State, Block>> {
        self.inner.child_storage_keys(hash, child_info, prefix, start_key)
    }

    fn storage_pairs(
        &self,
        hash: Hash,
        prefix: Option<&sp_storage::StorageKey>,
        start_key: Option<&sp_storage::StorageKey>,
    ) -> sp_blockchain::Result<PairsIter<<Backend as BackendT<Block>>::State, Block>> {
        self.inner.storage_pairs(hash, prefix, start_key)
    }

    fn storage(
        &self,
        hash: Hash,
        key: &sp_storage::StorageKey,
    ) -> sp_blockchain::Result<Option<StorageData>> {
        self.inner.storage(hash, key)
    }

    fn storage_hash(
        &self,
        hash: Hash,
        key: &sp_storage::StorageKey,
    ) -> sp_blockchain::Result<Option<Hash>> {
        self.inner.storage_hash(hash, key)
    }

    fn child_storage(
        &self,
        hash: Hash,
        child_info: &ChildInfo,
        key: &sp_storage::StorageKey,
    ) -> sp_blockchain::Result<Option<StorageData>> {
        self.inner.child_storage(hash, child_info, key)
    }

    fn child_storage_hash(
        &self,
        hash: Hash,
        child_info: &ChildInfo,
        key: &sp_storage::StorageKey,
    ) -> sp_blockchain::Result<Option<Hash>> {
        self.inner.child_storage_hash(hash, child_info, key)
    }

    fn closest_merkle_value(
        &self,
        hash: Hash,
        key: &sp_storage::StorageKey,
    ) -> blockchain::Result<Option<MerkleValue<Hash>>> {
        self.inner.closest_merkle_value(hash, key)
    }

    fn child_closest_merkle_value(
        &self,
        hash: Hash,
        child_info: &ChildInfo,
        key: &sp_storage::StorageKey,
    ) -> blockchain::Result<Option<MerkleValue<Hash>>> {
        self.inner.child_closest_merkle_value(hash, child_info, key)
    }
}

impl UsageProvider<Block> for Client {
    fn usage_info(&self) -> ClientInfo<Block> {
        self.inner.usage_info()
    }
}

impl Finalizer<Block, Backend> for Client {
    fn apply_finality(
        &self,
        operation: &mut ClientImportOperation<Block, Backend>,
        hash: Hash,
        justification: Option<Justification>,
        notify: bool,
    ) -> sp_blockchain::Result<()> {
        self.inner.apply_finality(operation, hash, justification, notify)
    }

    fn finalize_block(
        &self,
        hash: Hash,
        justification: Option<Justification>,
        notify: bool,
    ) -> sp_blockchain::Result<()> {
        self.inner.finalize_block(hash, justification, notify)
    }
}

#[async_trait::async_trait]
impl BlockImport<Block> for &Client {
    type Error = <InnerClient as BlockImport<Block>>::Error;

    async fn check_block(
        &self,
        block: BlockCheckParams<Block>,
    ) -> Result<ImportResult, Self::Error> {
        self.inner.check_block(block).await
    }

    async fn import_block(
        &self,
        import_block: BlockImportParams<Block>,
    ) -> Result<ImportResult, Self::Error> {
        self.inner.import_block(import_block).await
    }
}
