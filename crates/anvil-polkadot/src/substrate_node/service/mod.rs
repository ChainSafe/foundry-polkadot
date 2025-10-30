use crate::{
    AnvilNodeConfig,
    substrate_node::{
        lazy_loading::{
            backend::Backend as LazyLoadingBackend, rpc_client::RPC as LazyLoadingRPCClient,
        },
        mining_engine::{MiningEngine, MiningMode, run_mining_engine},
        rpc::spawn_rpc_server,
    },
};
use anvil::eth::backend::time::TimeManager;
use parking_lot::Mutex;
use polkadot_sdk::{
    cumulus_client_parachain_inherent::MockValidationDataInherentDataProvider,
    cumulus_primitives_aura::AuraUnincludedSegmentApi,
    parachains_common::{Hash, SLOT_DURATION, opaque::Block},
    polkadot_primitives::{self, Id, PersistedValidationData, UpgradeGoAhead},
    sc_basic_authorship, sc_chain_spec, sc_consensus,
    sc_consensus_aura::{self, AuraApi},
    sc_consensus_manual_seal::{
        ManualSealParams, consensus::aura::AuraConsensusDataProvider, run_manual_seal,
    },
    sc_executor,
    sc_service::{
        self, Configuration, RpcHandlers, SpawnTaskHandle, TaskManager,
        error::Error as ServiceError,
    },
    sc_transaction_pool::{self, TransactionPoolWrapper},
    sp_api::{ApiExt, ProvideRuntimeApi},
    sp_arithmetic::traits::UniqueSaturatedInto,
    sp_core::H256,
    sp_io,
    sp_keystore::KeystorePtr,
    sp_timestamp,
    sp_wasm_interface::ExtendedHostFunctions,
    substrate_frame_rpc_system::SystemApiServer,
};
use std::marker::PhantomData;

use std::sync::Arc;
use tokio::runtime::Builder as TokioRtBuilder;
use tokio_stream::wrappers::ReceiverStream;

use serde_json::{Map, Value, json};

use indicatif::{ProgressBar, ProgressStyle};
use jsonrpsee::{
    core::client::ClientT as JsonClientT,
    http_client::{HeaderMap, HeaderValue, HttpClient, HttpClientBuilder},
    rpc_params,
};

pub use backend::{BackendError, BackendWithOverlay, StorageOverrides};
pub use client::Client;

mod backend;
mod client;
mod executor;
pub mod storage;

pub type Backend = LazyLoadingBackend<Block>;

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

fn create_manual_seal_inherent_data_providers(
    client: Arc<Client>,
    para_id: Id,
    slot_duration: sc_consensus_aura::SlotDuration,
) -> impl Fn(
    Hash,
    (),
) -> futures::future::Ready<
    Result<
        (sp_timestamp::InherentDataProvider, MockValidationDataInherentDataProvider<()>),
        Box<dyn std::error::Error + Send + Sync>,
    >,
> + Send
+ Sync {
    move |block: Hash, ()| {
        let current_para_head = client
            .header(block)
            .expect("Header lookup should succeed")
            .expect("Header passed in as parent should be present in backend.");

        // NOTE: Our runtime API doesnt seem to have collect_collation_info available
        // let should_send_go_ahead = client
        //     .runtime_api()
        //     .collect_collation_info(block, &current_para_head)
        //     .map(|info| info.new_validation_code.is_some())
        //     .unwrap_or_default();

        // The API version is relevant here because the constraints in the runtime changed
        // in https://github.com/paritytech/polkadot-sdk/pull/6825. In general, the logic
        // here assumes that we are using the aura-ext consensushook in the parachain
        // runtime.
        // Note: Taken from https://github.com/paritytech/polkadot-sdk/issues/7341, but unsure if needed or not
        let requires_relay_progress = client
            .runtime_api()
            .has_api_with::<dyn AuraUnincludedSegmentApi<Block>, _>(block, |version| version > 1)
            .ok()
            .unwrap_or_default();

        let current_para_block_head =
            Some(polkadot_primitives::HeadData(current_para_head.hash().as_bytes().to_vec()));

        let current_block_number =
            UniqueSaturatedInto::<u32>::unique_saturated_into(current_para_head.number) + 1;

        let mocked_parachain = MockValidationDataInherentDataProvider::<()> {
            current_para_block: current_block_number,
            para_id,
            current_para_block_head,
            relay_offset: 0,
            relay_blocks_per_para_block: requires_relay_progress.then(|| 1).unwrap_or_default(),
            para_blocks_per_relay_epoch: 1,
            // upgrade_go_ahead: should_send_go_ahead.then(|| {
            //     //log::info!("Detected pending validation code, sending go-ahead signal.");
            //     UpgradeGoAhead::GoAhead
            // }),
            ..Default::default()
        };

        let timestamp_provider = sp_timestamp::InherentDataProvider::new(
            (slot_duration.as_millis() * current_block_number as u64).into(),
        );

        futures::future::ready(Ok((timestamp_provider, mocked_parachain)))
    }
}

/// Builds a new service for a full client.
pub fn new(
    anvil_config: &AnvilNodeConfig,
    mut config: Configuration,
) -> Result<(Service, TaskManager), ServiceError> {
    let storage_overrides = Arc::new(Mutex::new(StorageOverrides::default()));
    let executor = sc_service::new_wasm_executor(&config.executor);

    let (client, backend, keystore, mut task_manager) = client::new_client(
        anvil_config,
        anvil_config.get_genesis_number(),
        &mut config,
        executor,
        storage_overrides.clone(),
    )?;

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

    let slot_duration = sc_consensus_aura::SlotDuration::from_millis(SLOT_DURATION);

    // Polkadot-sdk doesnt seem to use the latest changes here, so this function isnt available yet.
    // Can use `new()` instead but our client doesnt implement all the needed traits
    // let aura_digest_provider = AuraConsensusDataProvider::new_with_slot_duration(slot_duration);

    let para_id = Id::new(anvil_config.get_chain_id().try_into().unwrap());

    let create_inherent_data_providers =
        create_manual_seal_inherent_data_providers(client.clone(), para_id, slot_duration);

    let params = ManualSealParams {
        block_import: client.clone(),
        env: proposer,
        client: client.clone(),
        pool: transaction_pool.clone(),
        select_chain: SelectChain::new(backend.clone()),
        commands_stream: Box::pin(commands_stream),
        consensus_data_provider: None,
        create_inherent_data_providers,
    };
    let authorship_future = run_manual_seal(params);

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
