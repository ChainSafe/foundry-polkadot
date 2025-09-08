use parking_lot::Mutex;
use polkadot_sdk::{
    sc_chain_spec::get_extension,
    sc_client_api::{execution_extensions::ExecutionExtensions, BadBlocks, ForkBlocks},
    sc_executor::WasmExecutor,
    sc_service::{
        self, new_db_backend, GenesisBlockBuilder, KeystoreContainer, LocalCallExecutor,
        TaskManager,
    },
    sp_keystore::KeystorePtr,
};
use std::{collections::HashMap, sync::Arc};
use substrate_runtime::{Block, RuntimeApi};

use crate::substrate_node::service::{backend::StorageOverrides, executor::Executor, Backend};

pub type Client = sc_service::client::Client<Backend, Executor, Block, RuntimeApi>;

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
        let executor = Executor::new(inner_executor, storage_overrides, backend.clone());

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
