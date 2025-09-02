use codec::Encode;
use polkadot_sdk::{
    parachains_common::{Hash, Header},
    sc_chain_spec::get_extension,
    sc_client_api::{
        self, backend::Finalizer, blockchain, client::ClientInfo,
        execution_extensions::ExecutionExtensions, Backend as BackendT, BadBlocks, BlockBackend,
        BlockchainEvents, CallExecutor, ClientImportOperation, ExecutorProvider, ForkBlocks,
        KeysIter, PairsIter, ProofProvider, StorageProvider, UsageProvider,
    },
    sc_consensus::{BlockCheckParams, BlockImport, BlockImportParams, ImportResult},
    sc_executor::{self, RuntimeVersion, RuntimeVersionOf, WasmExecutor},
    sc_service::{
        self, new_db_backend, GenesisBlockBuilder, KeystoreContainer, LocalCallExecutor,
        TaskManager,
    },
    sp_api::{
        self, ApiRef, CallApiAt, CallApiAtParams, CallContext, ConstructRuntimeApi, ProofRecorder,
        ProvideRuntimeApi,
    },
    sp_blockchain::{self, CachedHeaderMetadata, HeaderBackend, HeaderMetadata},
    sp_consensus,
    sp_core::{self, storage::StorageData},
    sp_externalities, sp_io,
    sp_keystore::KeystorePtr,
    sp_runtime::{
        generic::{BlockId, SignedBlock},
        traits::{BlockIdTo, HashingFor, NumberFor},
        Justification, Justifications,
    },
    sp_state_machine::{
        CompactProof, KeyValueStates, KeyValueStorageLevel, OverlayedChanges, StorageKey,
        StorageProof, StorageValue,
    },
    sp_storage::{self, ChildInfo},
    sp_trie::MerkleValue,
    sp_version,
};
use std::{cell::RefCell, collections::HashMap, sync::Arc};
use substrate_runtime::{Block, RuntimeApi, UncheckedExtrinsic};

use crate::substrate_node::service::Backend;

type InnerLocalCallExecutor = sc_service::client::LocalCallExecutor<
    Block,
    Backend,
    WasmExecutor<sp_io::SubstrateHostFunctions>,
>;

pub type Client = sc_service::client::Client<Backend, Executor, Block, RuntimeApi>;

#[derive(Clone)]
pub struct Executor {
    inner: InnerLocalCallExecutor,
    overrides: HashMap<StorageKey, StorageValue>,
    backend: Arc<Backend>,
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
            println!("Call: {}", method);
            let mut changes = OverlayedChanges::default();
            changes.set_storage(
                hex::decode("c2261276cc9d1f8598ea4b6a74b15c2f57c875e4cff74148e4628f264b974c80")
                    .unwrap(),
                Some(0u128.encode()),
            );

            let at_number =
                self.backend.blockchain().expect_block_number_from_id(&BlockId::Hash(at_hash))?;
            let extensions = self.execution_extensions().extensions(at_hash, at_number);

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
            changes.borrow_mut().set_storage(
                hex::decode("c2261276cc9d1f8598ea4b6a74b15c2f57c875e4cff74148e4628f264b974c80")
                    .unwrap(),
                Some(0u128.encode()),
            );
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
            Executor { inner: inner_executor, overrides: HashMap::new(), backend: backend.clone() };

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
