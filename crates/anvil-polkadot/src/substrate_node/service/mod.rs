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
    sp_keystore::KeystorePtr,
    sp_timestamp,
    substrate_frame_rpc_system::SystemApiServer,
    cumulus_client_service::ParachainHostFunctions,
cumulus_primitives_parachain_inherent::ParachainInherentData,
polkadot_primitives::PersistedValidationData,
cumulus_test_relay_sproof_builder::RelayStateSproofBuilder,
};
use std::sync::Arc;
use substrate_runtime::{OpaqueBlock as Block, RuntimeApi, Hash};
use tokio_stream::wrappers::ReceiverStream;
use tokio::runtime::Builder as TokioRtBuilder;
//use substrate_runtime::Hash;
//use crate::substrate_node::service::sc_consensus_manual_seal::consensus::aura::AuraConsensusDataProvider;

use serde_json::{json, Map, Value};

use jsonrpsee::http_client::{HttpClient, HttpClientBuilder, HeaderMap, HeaderValue};
use jsonrpsee::core::client::ClientT as JsonClientT;
use jsonrpsee::rpc_params;
use indicatif::{ProgressBar, ProgressStyle};

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

async fn fetch_sync_spec_http(client: &HttpClient, at_hex_opt: Option<String>) -> eyre::Result<Vec<u8>> {
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::with_template("{spinner:.green} {msg}")
            .unwrap()
            .tick_chars("/|\\- "),
    );
    pb.enable_steady_tick(std::time::Duration::from_millis(120));
    pb.set_message("Downloading sync state spec...");

    let raw = true;
    let spec_json: serde_json::Value = client
        .request("sync_state_genSyncSpec", rpc_params![raw, at_hex_opt])
        .await?;

    pb.finish_with_message("Sync state spec downloaded ✔");

    Ok(serde_json::to_vec(&spec_json)?)
}

async fn fetch_all_keys_paged(client: &HttpClient, at_hex: &str) -> eyre::Result<Vec<String>> {
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::with_template("{spinner:.green} Fetching key pages... {pos} pages collected")
            .unwrap()
            .tick_chars("/|\\- "),
    );
    pb.enable_steady_tick(std::time::Duration::from_millis(120));

    let mut keys = Vec::new();
    let mut start_key: Option<String> = None;
    let mut page_count: u64 = 0;
    loop {
        let page: Vec<String> = client
            .request(
                "state_getKeysPaged",
                rpc_params!["0x", 1000u32, start_key.clone(), at_hex],
            )
            .await?;
        if page.is_empty() { break; }
        start_key = page.last().cloned();
        keys.extend(page.into_iter());
        page_count += 1;
        pb.set_position(page_count);
    }

    pb.finish_with_message(format!("All keys fetched ✔ (total: {})", keys.len()));
    Ok(keys)
}

async fn fetch_top_state_map_http(client: &HttpClient, at_hex: &str) -> eyre::Result<Map<String, Value>> {
    let keys = fetch_all_keys_paged(client, at_hex).await?;

    let pb = ProgressBar::new(keys.len() as u64);
    pb.set_style(
        ProgressStyle::with_template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} values")
            .unwrap()
            .progress_chars("=>-"),
    );
    pb.set_message("Downloading values...");

    let mut top_map: Map<String, Value> = Map::new();
    for k in keys.iter() {
        let v: Option<String> = client.request("state_getStorage", rpc_params![k.clone(), at_hex]).await?;
        if let Some(val_hex) = v {
            top_map.insert(k.clone(), Value::String(val_hex));
        }
        pb.inc(1);
    }

    pb.finish_with_message("All values downloaded ✔");
    Ok(top_map)
}

fn build_forked_chainspec_from_raw_top(
    top_map: Map<String, Value>,
) -> sc_service::error::Result<Box<dyn sc_chain_spec::ChainSpec>> {
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

fn create_manual_seal_inherent_data_providers(
		client: Arc<FullClient>,
		para_id: Id,
		slot_duration: sc_consensus_aura::SlotDuration,
	) -> impl Fn(
		Hash,
		(),
	) ->
		futures::future::Ready<
		Result<
			(sp_timestamp::InherentDataProvider, MockValidationDataInherentDataProvider<()>),
			Box<dyn std::error::Error + Send + Sync>,
		>,
	> + Send
	       + Sync{
		move |block: Hash, ()| {
			let current_para_head = client
				.header(block)
				.expect("Header lookup should succeed")
				.expect("Header passed in as parent should be present in backend.");

        let current_para_block_head =
				Some(polkadot_primitives::HeadData(current_para_head.hash().as_bytes().to_vec()));

        let current_block_number =
        UniqueSaturatedInto::<u32>::unique_saturated_into(*current_para_head.number()) + 1;

        let mocked_parachain = MockValidationDataInherentDataProvider::<()> {
            current_para_block: current_block_number,
            para_id: para_id,
            current_para_block_head,
            relay_blocks_per_para_block: 1,
            para_blocks_per_relay_epoch: 10,
            // upgrade_go_ahead: should_send_go_ahead.then(|| {
            //     log::info!("Detected pending validation code, sending go-ahead signal.");
            //     UpgradeGoAhead::GoAhead
            // }),
            ..Default::default()
        };

        let timestamp_provider = sp_timestamp::InherentDataProvider::new(
            (slot_duration.as_millis() * current_block_number as u64).into(),
        );


        futures::future::ready(Ok((timestamp_provider, mocked_parachain)))
        //Ok((timestamp_provider, mocked_parachain))
		}
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

        let spec_or_top = std::thread::spawn(move || -> eyre::Result<Result<Vec<u8>, Map<String, Value>>> {
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
                let try_sync = fetch_sync_spec_http(&http, Some(at_hex.clone())).await;
                match try_sync {
                    Ok(spec_bytes) => Ok(Ok(spec_bytes)),
                    Err(_) => {
                        let top = fetch_top_state_map_http(&http, &at_hex).await?;
                        Ok(Err(top))
                    }
                }
            })
        })
        .join()
        .map_err(|_| ServiceError::Other("tokio thread panicked".into()))?
        .map_err(|e| ServiceError::Other(format!("fork fetch failed: {e}")))?;

        match spec_or_top {
            Ok(spec_bytes) => {
                type EmptyExt = Option<()>;
                let new_spec: sc_chain_spec::GenericChainSpec<EmptyExt> =
                    sc_chain_spec::GenericChainSpec::from_json_bytes(spec_bytes)
                        .map_err(|e| ServiceError::Other(format!("from_json_bytes failed: {e}")))?;
                config.chain_spec = Box::new(new_spec);
            }
            Err(top_map) => {
                config.chain_spec = build_forked_chainspec_from_raw_top(top_map)?;
            }
        }
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

    // let create_inherent_data_providers = {
    //     move |_, ()| {
    //         let next_timestamp = sp_timestamp::Timestamp::current();
    //         async move { Ok(sp_timestamp::InherentDataProvider::new(next_timestamp.into())) }
    //     }
    // };

    // Note: Changing slot durations are currently not supported
		// let slot_duration = sc_consensus_aura::slot_duration(&*client)
		// 	.expect("slot_duration is always present; qed.");

       let slot_duration= sc_consensus_aura::SlotDuration::from_millis(6000);

        // The aura digest provider will provide digests that match the provided timestamp data.
		// Without this, the AURA parachain runtimes complain about slot mismatches.
	//	let aura_digest_provider = AuraConsensusDataProvider::new(client);

    let para_id = Id::new(0);

    let create_inherent_data_providers = create_manual_seal_inherent_data_providers(
			client.clone(),
			para_id,
			slot_duration,
		);

    //  let create_inherent_data_providers = move |block: Hash, ()| async move {
	// 	let current_para_head = client
	// 			.header(block)
	// 			.expect("Header lookup should succeed")
	// 			.expect("Header passed in as parent should be present in backend.");

    //     let current_para_block_head =
	// 			Some(polkadot_primitives::HeadData(current_para_head.hash().as_bytes().to_vec()));

    //             let current_block_number =
	// 			UniqueSaturatedInto::<u32>::unique_saturated_into(*current_para_head.number()) + 1;

    //     let mocked_parachain = MockValidationDataInherentDataProvider::<()> {
    //         current_para_block: 0,
    //         para_id: para_id,
    //         current_para_block_head,
    //         relay_blocks_per_para_block: 1,
    //         para_blocks_per_relay_epoch: 10,
    //         // upgrade_go_ahead: should_send_go_ahead.then(|| {
    //         //     log::info!("Detected pending validation code, sending go-ahead signal.");
    //         //     UpgradeGoAhead::GoAhead
    //         // }),
    //         ..Default::default()
    //     };

    //     let timestamp_provider = sp_timestamp::InherentDataProvider::new(
    //         (slot_duration.as_millis() * current_block_number as u64).into(),
    //     );


    //     Ok((timestamp_provider, mocked_parachain))
	// };


    //   let create_inherent_data_providers = move |_, _| async move {
	// 	//let time = sp_timestamp::InherentDataProvider::from_system_time();
    //     let time = sp_timestamp::InherentDataProvider::new(sp_timestamp::Timestamp::new(0));
	// 	// Create a dummy parachain inherent data provider which is required to pass
	// 	// the checks by the para chain system. We use dummy values because in the 'pending context'
	// 	// neither do we have access to the real values nor do we need them.
	// 	let (relay_parent_storage_root, relay_chain_state) =
	// 		RelayStateSproofBuilder::default().into_state_root_and_proof();
	// 	let vfp = PersistedValidationData {
	// 		// This is a hack to make `cumulus_pallet_parachain_system::RelayNumberStrictlyIncreases`
	// 		// happy. Relay parent number can't be bigger than u32::MAX.
	// 		relay_parent_number: u32::MAX,
	// 		relay_parent_storage_root,
	// 		..Default::default()
	// 	};

	// 	let parachain_inherent_data = ParachainInherentData {
	// 		validation_data: vfp,
	// 		relay_chain_state,
	// 		downward_messages: Default::default(),
	// 		horizontal_messages: Default::default(),
    //         relay_parent_descendants: Vec::new(),
	// 		collator_peer_id: None,
	// 	};
	// 	Ok((time, parachain_inherent_data))
	// };

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
