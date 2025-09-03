use codec::Encode;
use lru::LruCache;
use parking_lot::Mutex;
use polkadot_sdk::{
    parachains_common::Hash,
    sc_chain_spec::get_extension,
    sc_client_api::{
        execution_extensions::ExecutionExtensions, Backend as BackendT, BadBlocks, CallExecutor,
        ForkBlocks,
    },
    sc_executor::{self, RuntimeVersion, RuntimeVersionOf, WasmExecutor},
    sc_service::{
        self, new_db_backend, GenesisBlockBuilder, KeystoreContainer, LocalCallExecutor,
        TaskManager,
    },
    sp_api::{CallContext, ProofRecorder},
    sp_blockchain::{self, HeaderBackend},
    sp_core::{self},
    sp_externalities, sp_io,
    sp_keystore::KeystorePtr,
    sp_runtime::{generic::BlockId, traits::HashingFor},
    sp_state_machine::{OverlayedChanges, StorageKey, StorageProof, StorageValue},
    sp_version,
};
use std::{cell::RefCell, collections::HashMap, num::NonZeroUsize, sync::Arc};
use substrate_runtime::{Block, RuntimeApi};

use crate::substrate_node::service::Backend;

type InnerLocalCallExecutor = sc_service::client::LocalCallExecutor<
    Block,
    Backend,
    WasmExecutor<sp_io::SubstrateHostFunctions>,
>;

pub type Client = sc_service::client::Client<Backend, Executor, Block, RuntimeApi>;

mod well_known_keys {
    // Hex-encode key: 0x9527366927478e710d3f7fb77c6d1f89
    pub const CHAIN_ID: [u8; 16] = [
        149u8, 39u8, 54u8, 105u8, 39u8, 71u8, 142u8, 113u8, 13u8, 63u8, 127u8, 183u8, 124u8, 109u8,
        31u8, 137u8,
    ];

    // Hex-encoded key: 0xf0c365c3cf59d671eb72da0e7a4113c49f1f0515f462cdcf84e0f1d6045dfcbb
    pub const TIMESTAMP: [u8; 32] = [
        240, 195, 101, 195, 207, 89, 214, 113, 235, 114, 218, 14, 122, 65, 19, 196, 159, 31, 5, 21,
        244, 98, 205, 207, 132, 224, 241, 214, 4, 93, 252, 187,
    ];
}

pub struct StorageOverrides {
    per_block: LruCache<Hash, HashMap<StorageKey, StorageValue>>,
}

impl StorageOverrides {
    pub fn new() -> Self {
        Self { per_block: LruCache::new(NonZeroUsize::new(10).expect("10 is greater than 0")) }
    }

    pub fn set_chain_id(&mut self, latest_block: Hash, id: u64) {
        let mut changeset = HashMap::with_capacity(1);
        changeset.insert(well_known_keys::CHAIN_ID.to_vec(), id.encode());

        self.add(latest_block, changeset);
    }

    pub fn set_timestamp(&mut self, latest_block: Hash, timestamp: u64) {
        let mut changeset = HashMap::with_capacity(1);
        changeset.insert(well_known_keys::TIMESTAMP.to_vec(), timestamp.encode());

        self.add(latest_block, changeset);
    }

    fn add(&mut self, latest_block: Hash, changeset: HashMap<StorageKey, StorageValue>) {
        if let Some(per_block) = self.per_block.get_mut(&latest_block) {
            per_block.extend(changeset.into_iter());
        } else {
            self.per_block.put(latest_block, changeset);
        }
    }
}

#[derive(Clone)]
pub struct Executor {
    inner: InnerLocalCallExecutor,
    storage_overrides: Arc<Mutex<StorageOverrides>>,
    backend: Arc<Backend>,
}

impl Executor {
    fn apply_overrides(&self, hash: &Hash, overlay: &mut OverlayedChanges<HashingFor<Block>>) {
        let Some(overrides) = self.storage_overrides.lock().per_block.get(hash).cloned() else {
            return
        };

        for (key, val) in overrides {
            overlay.set_storage(key, Some(val));
        }
    }
}

impl CallExecutor<Block> for Executor {
    type Error = <InnerLocalCallExecutor as CallExecutor<Block>>::Error;

    type Backend = Backend;

    fn execution_extensions(&self) -> &ExecutionExtensions<Block> {
        self.inner.execution_extensions()
    }

    fn call(
        &self,
        at_hash: Hash,
        method: &str,
        call_data: &[u8],
        context: CallContext,
    ) -> sp_blockchain::Result<Vec<u8>> {
        if context == CallContext::Offchain {
            let at_number =
                self.backend.blockchain().expect_block_number_from_id(&BlockId::Hash(at_hash))?;
            let extensions = self.execution_extensions().extensions(at_hash, at_number);

            let mut changes = OverlayedChanges::default();

            self.apply_overrides(&at_hash, &mut changes);

            self.contextual_call(
                at_hash,
                method,
                call_data,
                &RefCell::new(changes),
                &None,
                context,
                &RefCell::new(extensions),
            )
        } else {
            self.call(at_hash, method, call_data, context)
        }
    }

    fn contextual_call(
        &self,
        at_hash: Hash,
        method: &str,
        call_data: &[u8],
        changes: &RefCell<OverlayedChanges<HashingFor<Block>>>,
        recorder: &Option<ProofRecorder<Block>>,
        call_context: CallContext,
        extensions: &RefCell<sp_externalities::Extensions>,
    ) -> Result<Vec<u8>, sp_blockchain::Error> {
        if method == "Core_initialize_block" && call_context == CallContext::Onchain {
            self.apply_overrides(&at_hash, &mut changes.borrow_mut());
        }

        self.inner.contextual_call(
            at_hash,
            method,
            call_data,
            changes,
            recorder,
            call_context,
            extensions,
        )
    }

    fn runtime_version(&self, at_hash: Hash) -> sp_blockchain::Result<RuntimeVersion> {
        CallExecutor::runtime_version(&self.inner, at_hash)
    }

    fn prove_execution(
        &self,
        at_hash: Hash,
        method: &str,
        call_data: &[u8],
    ) -> sp_blockchain::Result<(Vec<u8>, StorageProof)> {
        self.inner.prove_execution(at_hash, method, call_data)
    }
}

impl RuntimeVersionOf for Executor {
    fn runtime_version(
        &self,
        ext: &mut dyn sp_externalities::Externalities,
        runtime_code: &sp_core::traits::RuntimeCode<'_>,
    ) -> Result<sp_version::RuntimeVersion, sc_executor::error::Error> {
        RuntimeVersionOf::runtime_version(&self.inner, ext, runtime_code)
    }
}

pub fn new_client(
    config: &sc_service::Configuration,
    executor: WasmExecutor,
    storage_overrides: Arc<Mutex<StorageOverrides>>,
) -> Result<(Arc<Client>, Arc<Backend>, KeystorePtr, TaskManager), sc_service::error::Error> {
    let backend = new_db_backend(config.db_config())?;

    let genesis_block_builder = GenesisBlockBuilder::new(
        config.chain_spec.as_storage_builder(),
        !config.no_genesis(),
        backend.clone(),
        executor.clone(),
    )?;

    let keystore_container = KeystoreContainer::new(&config.keystore)?;

    let task_manager = {
        let registry = config.prometheus_config.as_ref().map(|cfg| &cfg.registry);
        TaskManager::new(config.tokio_handle.clone(), registry)?
    };

    let chain_spec = &config.chain_spec;
    let fork_blocks =
        get_extension::<ForkBlocks<Block>>(chain_spec.extensions()).cloned().unwrap_or_default();

    let bad_blocks =
        get_extension::<BadBlocks<Block>>(chain_spec.extensions()).cloned().unwrap_or_default();

    let execution_extensions = ExecutionExtensions::new(None, Arc::new(executor.clone()));

    let wasm_runtime_substitutes = HashMap::new();

    let client = {
        let client_config = sc_service::ClientConfig {
            offchain_worker_enabled: config.offchain_worker.enabled,
            offchain_indexing_api: config.offchain_worker.indexing_enabled,
            wasm_runtime_overrides: config.wasm_runtime_overrides.clone(),
            no_genesis: config.no_genesis(),
            wasm_runtime_substitutes,
            enable_import_proof_recording: false,
        };
        let inner_executor = LocalCallExecutor::new(
            backend.clone(),
            executor,
            client_config.clone(),
            execution_extensions,
        )?;
        let executor =
            Executor { inner: inner_executor, storage_overrides, backend: backend.clone() };

        Client::new(
            backend.clone(),
            executor,
            Box::new(task_manager.spawn_handle()),
            genesis_block_builder,
            fork_blocks,
            bad_blocks,
            None,
            None,
            client_config,
        )?
    };

    Ok((Arc::new(client), backend, keystore_container.keystore(), task_manager))
}
