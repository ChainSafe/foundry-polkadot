use crate::{
    AnvilNodeConfig,
    substrate_node::{
        mining_engine::{MiningEngine, MiningMode, run_mining_engine},
        rpc::spawn_rpc_server,
    },
};
use anvil::eth::backend::time::TimeManager;
use parking_lot::Mutex;
use polkadot_sdk::{
    parachains_common::opaque::Block,
    sc_basic_authorship, sc_consensus, sc_consensus_manual_seal,
    sc_service::{
        self, Configuration, RpcHandlers, SpawnTaskHandle, TaskManager,
        error::Error as ServiceError,
    },
    sp_wasm_interface::ExtendedHostFunctions,
    sc_transaction_pool::{self, TransactionPoolWrapper},
    sc_utils::mpsc::tracing_unbounded,
    sc_chain_spec,
    sp_io,
    sp_core::storage::well_known_keys,
    sp_keystore::KeystorePtr,
    sp_timestamp,
    substrate_frame_rpc_system::SystemApiServer,
};
use std::sync::Arc;
use tokio_stream::wrappers::ReceiverStream;
use tokio::runtime::Builder as TokioRtBuilder;

use serde_json::{json, Map, Value};

use jsonrpsee::http_client::{HttpClient, HttpClientBuilder, HeaderMap, HeaderValue};
use jsonrpsee::core::client::ClientT as JsonClientT;
use jsonrpsee::rpc_params;
use hex;

use crate::AnvilNodeConfig;

pub use backend::{BackendError, BackendWithOverlay, StorageOverrides};
pub use client::Client;

mod backend;
mod client;
mod executor;
pub mod storage;

pub type Backend = sc_service::TFullBackend<Block>;

pub type TransactionPoolHandle = sc_transaction_pool::TransactionPoolHandle<Block, Client>;

type SelectChain = sc_consensus::LongestChain<Backend, Block>;

#[derive(Clone)]
pub struct Service {
    pub spawn_handle: SpawnTaskHandle,
    pub client: Arc<Client>,
    pub backend: Arc<Backend>,
    pub tx_pool: Arc<TransactionPoolHandle>,
    pub rpc_handlers: RpcHandlers,
    pub mining_engine: Arc<MiningEngine>,
    pub storage_overrides: Arc<Mutex<StorageOverrides>>,
    pub genesis_block_number: u64,
}

async fn resolve_fork_hash_http(client: &HttpClient, fork_block_hash: Option<String>) -> eyre::Result<String> {
    if let Some(h) = fork_block_hash { return Ok(h); }
    let res: String = client.request("chain_getBlockHash", rpc_params![]).await?;
    Ok(res)
}

async fn fetch_runtime_code_at_http(client: &HttpClient, at_hex: &str) -> eyre::Result<Vec<u8>> {
    let key_hex = format!("0x{}", hex::encode(well_known_keys::CODE));
    let res: Option<String> = client
        .request("state_getStorage", rpc_params![key_hex, at_hex])
        .await?;
    let Some(code_hex) = res else { eyre::bail!("state_getStorage(:code) returned None at {at_hex}"); };
    let code_hex = code_hex.strip_prefix("0x").unwrap_or(&code_hex);
    Ok(hex::decode(code_hex)?)
}

fn build_forked_chainspec_from_json(
    config: &Configuration,
    wasm_code: Vec<u8>,
) -> sc_service::error::Result<Box<dyn sc_chain_spec::ChainSpec>> {
    let storage = config
        .chain_spec
        .as_storage_builder()
        .build_storage()
        .map_err(|e| ServiceError::Other(format!("build_storage failed: {e}")))?;

    let mut top_map: Map<String, Value> = Map::new();
    for (k, v) in storage.top.into_iter() {
        let key_hex = format!("0x{}", hex::encode(k));
        let val_hex = format!("0x{}", hex::encode(v));
        top_map.insert(key_hex, Value::String(val_hex));
    }
    let code_key = format!("0x{}", hex::encode(well_known_keys::CODE));
    top_map.insert(code_key, Value::String(format!("0x{}", hex::encode(wasm_code))));

    let children_default = serde_json::Map::<String, Value>::new();
    let spec_json = json!({
        "name": "Anvil Polkadot (Forked)",
        "id": "anvil-polkadot-forked",
        "chainType": "Development",
        "bootNodes": [],
        "telemetryEndpoints": null,
        "protocolId": null,
        "properties": null,
        "codeSubstitutes": {},
        "consensusEngine": null,
        "genesis": { "raw": { "top": top_map, "childrenDefault": children_default }}
    });

    let bytes = serde_json::to_vec(&spec_json)
        .map_err(|e| ServiceError::Other(format!("serialize spec json failed: {e}")))?;

    type EmptyExt = Option<()>;
    let new_spec: sc_chain_spec::GenericChainSpec<EmptyExt> =
        sc_chain_spec::GenericChainSpec::from_json_bytes(bytes)
            .map_err(|e| ServiceError::Other(format!("from_json_bytes failed: {e}")))?;
    Ok(Box::new(new_spec))
}

/// Builds a new service for a full client.
pub fn new(
    anvil_config: &AnvilNodeConfig,
    mut config: Configuration,
) -> Result<(Service, TaskManager), ServiceError> {
    let storage_overrides = Arc::new(Mutex::new(StorageOverrides::default()));

    let (client, backend, keystore, mut task_manager) = client::new_client(
        anvil_config.get_genesis_number(),
        &config,
        sc_service::new_wasm_executor(&config.executor),
        storage_overrides.clone(),
    )?;
    if let Some(ref fork_url) = anvil_config.fork_url {
        let http_url = fork_url.clone();
        let fork_block_hash = anvil_config.fork_block_hash.clone();

        let wasm_code = std::thread::spawn(move || -> eyre::Result<Vec<u8>> {
            let rt = TokioRtBuilder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| eyre::eyre!("tokio rt build error: {e}"))?;

            rt.block_on(async move {
                let mut headers = HeaderMap::new();
                headers.insert("Accept-Encoding", HeaderValue::from_static("gzip, deflate, br"));

                let http = HttpClientBuilder::default()
                    .set_headers(headers)
                    .build(http_url)
                    .map_err(|e| eyre::eyre!("http client build error: {e}"))?;

                let at_hex = resolve_fork_hash_http(&http, fork_block_hash).await?;
                fetch_runtime_code_at_http(&http, &at_hex).await
            })
        })
        .join()
        .map_err(|_| ServiceError::Other("tokio thread panicked".into()))?
        .map_err(|e| ServiceError::Other(format!("fork fetch failed: {e}")))?;

        let spec = build_forked_chainspec_from_json(&config, wasm_code)?;
        config.chain_spec = spec;
    }

    let transaction_pool = Arc::from(
        sc_transaction_pool::Builder::new(
            task_manager.spawn_essential_handle(),
            client.clone(),
            config.role.is_authority().into(),
        )
        .with_options(config.transaction_pool.clone())
        .build(),
    );

    task_manager.spawn_handle().spawn(
        "txpool-notifications",
        Some("transaction-pool"),
        sc_transaction_pool::notification_future(client.clone(), transaction_pool.clone()),
    );

    let (seal_engine_command_sender, commands_stream) = tokio::sync::mpsc::channel(1024);
    let commands_stream = ReceiverStream::new(commands_stream);

    let mining_mode =
        MiningMode::new(anvil_config.block_time, anvil_config.mixed_mining, anvil_config.no_mining);
    let time_manager = Arc::new(TimeManager::new_with_milliseconds(
        sp_timestamp::Timestamp::from(
            anvil_config
                .get_genesis_timestamp()
                .checked_mul(1000)
                .ok_or(ServiceError::Application("Genesis timestamp overflow".into()))?,
        )
        .into(),
    ));
    let mining_engine = Arc::new(MiningEngine::new(
        mining_mode,
        transaction_pool.clone(),
        time_manager.clone(),
        seal_engine_command_sender,
    ));

    let rpc_handlers = spawn_rpc_server(
        anvil_config.get_genesis_number(),
        &mut task_manager,
        client.clone(),
        config,
        transaction_pool.clone(),
        keystore,
        backend.clone(),
    )?;

    task_manager.spawn_handle().spawn(
        "mining_engine_task",
        Some("consensus"),
        run_mining_engine(mining_engine.clone()),
    );

    let proposer = sc_basic_authorship::ProposerFactory::new(
        task_manager.spawn_handle(),
        client.clone(),
        transaction_pool.clone(),
        None,
        None,
    );

    // Implement a dummy block production mechanism for now, just build an instantly finalized block
    // every 6 seconds. This will have to change.
    let default_block_time = 6000;
    let (mut sink, commands_stream) = futures::channel::mpsc::channel(1024);
    task_manager.spawn_handle().spawn("block_authoring", "anvil-polkadot", async move {
        loop {
            futures_timer::Delay::new(std::time::Duration::from_millis(default_block_time)).await;
            let _ = sink.try_send(sc_consensus_manual_seal::EngineCommand::SealNewBlock {
                create_empty: true,
                finalize: true,
                parent_hash: None,
                sender: None,
            });
        }
    };

    let params = sc_consensus_manual_seal::ManualSealParams {
        block_import: client.clone(),
        env: proposer,
        client: client.clone(),
        pool: transaction_pool.clone(),
        select_chain: SelectChain::new(backend.clone()),
        commands_stream: Box::pin(commands_stream),
        consensus_data_provider: None,
        create_inherent_data_providers,
    };
    let authorship_future = sc_consensus_manual_seal::run_manual_seal(params);

    task_manager.spawn_essential_handle().spawn_blocking(
        "manual-seal",
        "substrate",
        authorship_future,
    );

    Ok((
        Service {
            spawn_handle: task_manager.spawn_handle(),
            client,
            backend,
            tx_pool: transaction_pool,
            rpc_handlers,
            mining_engine,
            storage_overrides,
            genesis_block_number: anvil_config.get_genesis_number(),
        },
        task_manager,
    ))
}
