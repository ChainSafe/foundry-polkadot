use crate::{
    AnvilNodeConfig,
    substrate_node::{
        lazy_loading::backend::Backend as LazyLoadingBackend,
        mining_engine::{MiningEngine, MiningMode, run_mining_engine},
        rpc::spawn_rpc_server,
        service::consensus::SameSlotConsensusDataProvider,
        service::storage::well_known_keys,
        service::consensus::AuraConsensusDataProvider,
    },
};
use codec::Encode;
use std::marker::PhantomData;
use anvil::eth::backend::time::TimeManager;
use parking_lot::Mutex;
use polkadot_sdk::{
    cumulus_primitives_core::GetParachainInfo,
    sc_consensus_manual_seal::{self, ManualSealParams, run_manual_seal, ConsensusDataProvider, Error},
    parachains_common::{SLOT_DURATION, opaque::Block, Hash},
    sc_basic_authorship, sc_consensus::{self, BlockImportParams}, sc_executor,
    sc_service::{
        self, Configuration, RpcHandlers, SpawnTaskHandle, TaskManager,
        error::Error as ServiceError,
    },
        sc_transaction_pool::{self, TransactionPoolWrapper}, sp_io, sp_timestamp,
    sp_wasm_interface::ExtendedHostFunctions,
    sp_keystore::KeystorePtr,
    sc_consensus_aura,
    sp_consensus_aura::{
        digests::CompatibleDigestItem,
        sr25519::{AuthorityId, AuthoritySignature},
        AuraApi,
    },
    cumulus_client_parachain_inherent::MockValidationDataInherentDataProvider, 
    sp_arithmetic::traits::UniqueSaturatedInto,
    substrate_frame_rpc_system::SystemApiServer,
     sc_chain_spec,
     polkadot_primitives::{self, Id, Slot, PersistedValidationData, UpgradeGoAhead},
   sp_api::{ApiExt, ProvideRuntimeApi},
   cumulus_primitives_aura::{AuraUnincludedSegmentApi},
   cumulus_primitives_core::{relay_chain},
   sp_inherents::{self, InherentData},
   sc_client_api::{AuxStore, UsageProvider},
   sp_runtime::{traits::Block as BlockT, Digest, DigestItem},
   sp_timestamp::TimestampInherentData,
};
use std::sync::Arc;
use tokio_stream::wrappers::ReceiverStream;
use std::sync::atomic::{AtomicU64, Ordering};

pub use backend::{BackendError, BackendWithOverlay, StorageOverrides};
pub use client::Client;

mod backend;
mod client;
mod consensus;
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

const RELAY_CHAIN_SLOT_DURATION_MILLIS: u64 = 6_000;

static TIMESTAMP: AtomicU64 = AtomicU64::new(0);

/// Provide a mock duration starting at 0 in millisecond for timestamp inherent.
/// Each call will increment timestamp by slot_duration making Aura think time has passed.
struct MockTimestampInherentDataProvider;

impl MockTimestampInherentDataProvider {
    fn advance_timestamp(slot_duration: u64) {
        if TIMESTAMP.load(Ordering::SeqCst) == 0 {
            // Initialize timestamp inherent provider
            //TIMESTAMP.store()
            TIMESTAMP.store(sp_timestamp::Timestamp::current().as_millis(), Ordering::SeqCst);
        } else {
            TIMESTAMP.fetch_add(slot_duration, Ordering::SeqCst);
        }
    }
}

#[async_trait::async_trait]
impl sp_inherents::InherentDataProvider for MockTimestampInherentDataProvider {
    async fn provide_inherent_data(
        &self,
        inherent_data: &mut InherentData,
    ) -> Result<(), sp_inherents::Error> {
        inherent_data.put_data(sp_timestamp::INHERENT_IDENTIFIER, &TIMESTAMP.load(Ordering::SeqCst))
    }

    async fn try_handle_error(
        &self,
        _identifier: &sp_inherents::InherentIdentifier,
        _error: &[u8],
    ) -> Option<Result<(), sp_inherents::Error>> {
        // The pallet never reports error.
        None
    }
}


fn create_manual_seal_inherent_data_providers(
		client: Arc<Client>,
		// para_id: Id,
		// slot_duration: sc_consensus_aura::SlotDuration,
        anvil_config: AnvilNodeConfig,
	) -> impl Fn(
		Hash,
		(),
	) ->
		futures::future::Ready<
		Result<
			(MockTimestampInherentDataProvider, MockValidationDataInherentDataProvider<()>),
			Box<dyn std::error::Error + Send + Sync>,
		>,
	> + Send
	       + Sync{
		move |block: Hash, ()| {

        MockTimestampInherentDataProvider::advance_timestamp(RELAY_CHAIN_SLOT_DURATION_MILLIS);
        print!("time c {}", TIMESTAMP.load(Ordering::SeqCst));

        let current_para_head = client
            .header(block)
            .expect("Header lookup should succeed")
            .expect("Header passed in as parent should be present in backend.");

          let slot_duration = client.runtime_api().slot_duration(current_para_head.hash()).unwrap();

          let para_id = client.runtime_api().parachain_id(current_para_head.hash()).unwrap();

        // The API version is relevant here because the constraints in the runtime changed
        // in https://github.com/paritytech/polkadot-sdk/pull/6825. In general, the logic
        // here assumes that we are using the aura-ext consensushook in the parachain
        // runtime.
        // Note: Taken from https://github.com/paritytech/polkadot-sdk/issues/7341, but unsure fi needed or not
        // let requires_relay_progress = client
        //     .runtime_api()
        //     .has_api_with::<dyn AuraUnincludedSegmentApi<Block>, _>(block, |version| version > 1)
        //     .ok()
        //     .unwrap_or_default();


        let current_para_block_head =
            Some(polkadot_primitives::HeadData(current_para_head.hash().as_bytes().to_vec()));

        let current_block_number =
            UniqueSaturatedInto::<u32>::unique_saturated_into(current_para_head.number) + 1;
        print!("current block num {}", current_para_head.number);

        // // Unsure here but triggers new error than before
        //let time = anvil_config.get_genesis_timestamp();
        let time = TIMESTAMP.load(Ordering::SeqCst);

        let mocked_parachain = MockValidationDataInherentDataProvider::<()> {
            current_para_block: current_para_head.number,
            para_id,
            current_para_block_head,
            relay_offset:  time as u32,
           // relay_blocks_per_para_block: requires_relay_progress.then(|| 1).unwrap_or_default(),
            relay_blocks_per_para_block: 1,
            para_blocks_per_relay_epoch: 10,
            ..Default::default()
        };

        futures::future::ready(Ok((MockTimestampInherentDataProvider, mocked_parachain)))
		}
	}

/// Builds a new service for a full client.
pub fn new(
    anvil_config: &AnvilNodeConfig,
    mut config: Configuration,
) -> Result<(Service, TaskManager), ServiceError> {
    let storage_overrides = Arc::new(Mutex::new(StorageOverrides::default()));
    let executor = sc_service::new_wasm_executor(&config.executor);

    let (client, backend, keystore, mut task_manager) =
        client::new_client(anvil_config, &mut config, executor, storage_overrides.clone())?;

    let transaction_pool = Arc::from(
        sc_transaction_pool::Builder::new(
            task_manager.spawn_essential_handle(),
            client.clone(),
            config.role.is_authority().into(),
        )
        .with_options(config.transaction_pool.clone())
        .build(),
    );

    // Inform the tx pool about imported and finalized blocks.
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

    let slot_duration= sc_consensus_aura::SlotDuration::from_millis(6000);
    //let slot_duration = client.runtime_api().slot_duration();

	let aura_digest_provider = AuraConsensusDataProvider::new_with_slot_duration(slot_duration);
 //  let aura_digest_provider = AuraConsensusDataProvider::new(client);

    let create_inherent_data_providers = create_manual_seal_inherent_data_providers(
			client.clone(),
            anvil_config.clone(),
		);

    let params = sc_consensus_manual_seal::ManualSealParams {
        block_import: client.clone(),
        env: proposer,
        client: client.clone(),
        pool: transaction_pool.clone(),
        select_chain: SelectChain::new(backend.clone()),
        commands_stream: Box::pin(commands_stream),
        consensus_data_provider: Some(Box::new(aura_digest_provider)),
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
