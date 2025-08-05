use futures::FutureExt;
use polkadot_sdk::{
    polkadot_service::Backend as TBackend,
    sc_basic_authorship, sc_consensus, sc_consensus_manual_seal,
    sc_executor::WasmExecutor,
    sc_network,
    sc_service::{self, error::Error as ServiceError, Configuration, RpcHandlers, TaskManager},
    sc_transaction_pool, sp_io,
    sp_runtime::traits::Block as BlockT,
    sp_timestamp,
    substrate_frame_rpc_system::{System as SystemRpc, SystemApiServer},
};
use std::sync::Arc;
use substrate_runtime::{OpaqueBlock as Block, RuntimeApi};

use crate::AnvilNodeConfig;

type HostFunctions = sp_io::SubstrateHostFunctions;

pub(crate) type FullClient =
    sc_service::TFullClient<Block, RuntimeApi, WasmExecutor<HostFunctions>>;

type Backend = sc_service::TFullBackend<Block>;
type SelectChain = sc_consensus::LongestChain<Backend, Block>;
type TransactionPoolHandle = sc_transaction_pool::TransactionPoolHandle<Block, FullClient>;

pub struct Service {
    pub task_manager: TaskManager,
    pub client: Arc<FullClient>,
    pub backend: Arc<Backend>,
    pub tx_pool: Arc<TransactionPoolHandle>,
    pub rpc_handlers: RpcHandlers,
}

/// Builds a new service for a full client.
pub fn new<Network: sc_network::NetworkBackend<Block, <Block as BlockT>::Hash>>(
    anvil_config: &AnvilNodeConfig,
    config: Configuration,
) -> Result<Service, ServiceError> {
    let (client, backend, keystore_container, mut task_manager) =
        sc_service::new_full_parts::<Block, RuntimeApi, _>(
            &config,
            None,
            sc_service::new_wasm_executor(&config.executor),
        )?;
    let client = Arc::new(client);

    let transaction_pool = Arc::from(
        sc_transaction_pool::Builder::new(
            task_manager.spawn_essential_handle(),
            client.clone(),
            config.role.is_authority().into(),
        )
        .with_options(config.transaction_pool.clone())
        .build(),
    );

    let import_queue = sc_consensus_manual_seal::import_queue(
        Box::new(client.clone()),
        &task_manager.spawn_essential_handle(),
        None,
    );

    let net_config = sc_network::config::FullNetworkConfiguration::<
        Block,
        <Block as BlockT>::Hash,
        Network,
    >::new(&config.network, None);
    let metrics = Network::register_notification_metrics(None);

    let (network, system_rpc_tx, tx_handler_controller, sync_service) =
        sc_service::build_network(sc_service::BuildNetworkParams {
            config: &config,
            net_config,
            client: client.clone(),
            transaction_pool: transaction_pool.clone(),
            spawn_handle: task_manager.spawn_handle(),
            import_queue,
            block_announce_validator_builder: None,
            warp_sync_config: None,
            block_relay: None,
            metrics,
        })?;

    let rpc_extensions_builder = {
        let client = client.clone();
        let pool = transaction_pool.clone();

        Box::new(move |_| {
            Ok(polkadot_sdk::substrate_frame_rpc_system::System::new(client.clone(), pool.clone())
                .into_rpc())
        })
    };

    let rpc_handlers = sc_service::spawn_tasks(sc_service::SpawnTasksParams {
        network,
        client: client.clone(),
        keystore: keystore_container.keystore(),
        task_manager: &mut task_manager,
        transaction_pool: transaction_pool.clone(),
        rpc_builder: rpc_extensions_builder,
        backend: backend.clone(),
        system_rpc_tx,
        tx_handler_controller,
        sync_service,
        config,
        telemetry: None,
    })?;

    let proposer = sc_basic_authorship::ProposerFactory::new(
        task_manager.spawn_handle(),
        client.clone(),
        transaction_pool.clone(),
        None,
        None,
    );

    let default_block_time = 6000;

    let (mut sink, commands_stream) = futures::channel::mpsc::channel(1024);
    task_manager.spawn_handle().spawn("block_authoring", None, async move {
        loop {
            futures_timer::Delay::new(std::time::Duration::from_millis(default_block_time)).await;
            sink.try_send(sc_consensus_manual_seal::EngineCommand::SealNewBlock {
                create_empty: true,
                finalize: true,
                parent_hash: None,
                sender: None,
            })
            .unwrap();
        }
    });

    let params = sc_consensus_manual_seal::ManualSealParams {
        block_import: client.clone(),
        env: proposer,
        client: client.clone(),
        pool: transaction_pool.clone(),
        select_chain: sc_consensus::LongestChain::new(backend.clone()),
        commands_stream: Box::pin(commands_stream),
        consensus_data_provider: None,
        create_inherent_data_providers: move |_, ()| async move {
            Ok(sp_timestamp::InherentDataProvider::from_system_time())
        },
    };
    let authorship_future = sc_consensus_manual_seal::run_manual_seal(params);

    task_manager.spawn_essential_handle().spawn_blocking("manual-seal", None, authorship_future);

    Ok(Service { task_manager, client, backend, tx_pool: transaction_pool, rpc_handlers })
}
