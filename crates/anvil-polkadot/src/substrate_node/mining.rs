use crate::substrate_node::service::TransactionPoolHandle;
use alloy_primitives::U256;
use futures::{
    stream::{select, FusedStream},
    task::AtomicWaker,
    SinkExt, StreamExt,
};
use jsonrpsee::core::RpcResult;
use parking_lot::{Mutex, RwLock};
use polkadot_sdk::{sc_consensus_manual_seal::EngineCommand, sc_service::TransactionPool, sp_core};
use std::{pin::Pin, sync::Arc};

pub enum MiningMode {
    /// Create a new block everytime there is a new transaction
    AutoMining,
    /// Create a new block every <tick> seconds
    Interval {
        tick: u64,
    },
    /// A mix of AutoMining and Interval mining
    /// Create a new block every <tick> seconds
    /// or every time there is a new transaction
    Mixed {
        tick: u64,
    },
    Manual,
}

#[derive(Default)]
struct MinnerInner {
    waker: AtomicWaker,
}

impl MinnerInner {
    pub fn wake(&self) {
        self.waker.wake();
    }

    pub fn register(&self, cx: &std::task::Context<'_>) {
        self.waker.register(cx.waker());
    }
}

type ConsensusCommandStream = Pin<Box<dyn FusedStream<Item = EngineCommand<sp_core::H256>> + Send>>;

pub struct MiningEngine {
    pub mining_mode: Arc<RwLock<MiningMode>>,
    inner: Arc<MinnerInner>,
    transaction_pool: Arc<TransactionPoolHandle>,
    manual_command_sender: Mutex<futures::channel::mpsc::Sender<EngineCommand<sp_core::H256>>>,
}

impl MiningEngine {
    pub fn new(
        mining_mode: MiningMode,
        transaction_pool: Arc<TransactionPoolHandle>,
    ) -> (Self, futures::channel::mpsc::Receiver<EngineCommand<sp_core::H256>>) {
        let (manual_command_sender, manual_command_receiver) = futures::channel::mpsc::channel(1); // Small buffer
        (
            Self {
                mining_mode: Arc::new(RwLock::new(mining_mode)),
                inner: Default::default(),
                transaction_pool,
                manual_command_sender: Mutex::new(manual_command_sender),
            },
            manual_command_receiver,
        )
    }

    pub fn seal_now(&self) {
        // Maybe take parameters for create_empty and finalize?
        let seal_command = EngineCommand::SealNewBlock {
            create_empty: true,
            finalize: true,
            parent_hash: None,
            sender: None,
        };
        // Lock the mutex to get a mutable reference to the Sender.
        let mut sender_guard = self.manual_command_sender.lock();

        // Now `try_send` will work.
        let _ = sender_guard.try_send(seal_command);
        self.inner.wake();
    }

    pub fn get_command_stream(
        &self,
        manual_commands: futures::channel::mpsc::Receiver<EngineCommand<sp_core::H256>>,
    ) -> ConsensusCommandStream {
        let mining_mode = self.mining_mode.read();
        match *mining_mode {
            MiningMode::Manual => {
                let s = manual_commands.fuse();
                Box::pin(s)
            }
            MiningMode::AutoMining => {
                let manual_s = manual_commands.fuse();
                let auto_s = self
                    .transaction_pool
                    .import_notification_stream()
                    .map(|_| EngineCommand::SealNewBlock {
                        create_empty: false,
                        finalize: true,
                        parent_hash: None,
                        sender: None,
                    })
                    .fuse();
                Box::pin(select(manual_s, auto_s))
            }
            _ => todo!(), // Handle other modes like `Interval` here.
        }
    }
}

pub async fn run_mininig_engine(
    _engine: Arc<MiningEngine>,
    mut command_stream: ConsensusCommandStream,
    mut command_sink: futures::channel::mpsc::Sender<EngineCommand<sp_core::H256>>,
) {
    loop {
        // Await on the receiver. This is the key. It will block until a message arrives
        // or the sender is dropped.
        let maybe_cmd = command_stream.next().await;

        if let Some(cmd) = maybe_cmd {
            // Send the command to the manual-seal engine.
            if let Err(_) = command_sink.send(cmd).await {
                // If the send fails, it means the manual-seal task has shut down.
                break;
            }
        } else {
            // The `manual_command_receiver.next().await` returned `None`,
            // which means the sender was dropped. This is the shutdown signal.
            break;
        }
    }
}

// Simple RPC API server for the RPC endpoints
// This is separate from the main ApiServer that runs as a background task
pub struct RpcApiServer {
    mining_engine: Arc<MiningEngine>,
}

impl RpcApiServer {
    pub fn new(mining_engine: Arc<MiningEngine>) -> Self {
        Self { mining_engine }
    }
}

#[async_trait::async_trait]
impl crate::api_server::mining::AnvilPolkadotMiningApiServer for RpcApiServer {
    async fn get_auto_mine(&self) -> RpcResult<bool> {
        todo!()
    }

    async fn set_auto_mine(&self, _enabled: bool) -> RpcResult<()> {
        todo!()
    }

    async fn get_interval_mining(&self) -> RpcResult<Option<u64>> {
        todo!()
    }

    async fn set_interval_mining(&self, _interval: u64) -> RpcResult<()> {
        todo!()
    }

    async fn set_block_timestamp_interval(&self, _interval: u64) -> RpcResult<()> {
        todo!()
    }

    async fn remove_block_timestamp_interval(&self) -> RpcResult<()> {
        todo!()
    }

    async fn mine(&self, num_blocks: Option<U256>, time_offset: Option<U256>) -> RpcResult<()> {
        let _ = num_blocks;
        let _ = time_offset;
        self.mining_engine.seal_now();
        Ok(())
    }

    async fn increase_time(&self, _increase_by_secs: U256) -> RpcResult<U256> {
        todo!()
    }

    async fn set_next_block_timestamp(&self, _timestamp: U256) -> RpcResult<()> {
        todo!()
    }

    async fn set_time(&self, _timestamp: U256) -> RpcResult<U256> {
        todo!()
    }
}
