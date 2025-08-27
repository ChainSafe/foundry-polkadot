use crate::substrate_node::service::TransactionPoolHandle;
use alloy_primitives::U256;
use futures::{
    stream::{select, unfold, FusedStream},
    task::AtomicWaker,
    SinkExt, StreamExt,
};
use jsonrpsee::core::RpcResult;
use parking_lot::{Mutex, RwLock};
use polkadot_sdk::{sc_consensus_manual_seal::EngineCommand, sc_service::TransactionPool, sp_core};
use std::{pin::Pin, sync::Arc};
use tokio::time::{interval, Duration};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
}

pub async fn run_mininig_engine(
    engine: Arc<MiningEngine>,
    mut manual_commands_receiver: futures::channel::mpsc::Receiver<EngineCommand<sp_core::H256>>,
    mut command_sink: futures::channel::mpsc::Sender<EngineCommand<sp_core::H256>>,
) {
    let mut auto_mine_stream: Option<
        Pin<Box<dyn FusedStream<Item = EngineCommand<sp_core::H256>> + Send>>,
    > = None;
    let mut interval_stream: Option<
        Pin<Box<dyn FusedStream<Item = EngineCommand<sp_core::H256>> + Send>>,
    > = None;
    let mut current_mode = None;

    loop {
        let mut rebuild_future = futures::stream::poll_fn(|cx| {
            let mode = { *engine.mining_mode.read() };
            let mode_changed = current_mode.as_ref().map_or(true, |m| *m != mode);

            if mode_changed {
                current_mode = Some(mode);
                std::task::Poll::Ready(Some(())) // Return a value to signal a change.
            } else {
                engine.inner.register(cx);
                std::task::Poll::Pending
            }
        });

        tokio::select! {
            _ = rebuild_future.next() => {
                // The mode has changed, so we rebuild the streams.
                let mode = current_mode.clone().unwrap();

                // Rebuild the auto-mine stream based on the current mode.
                auto_mine_stream = if matches!(mode, MiningMode::AutoMining | MiningMode::Mixed { .. }) {
                    let stream = engine
                        .transaction_pool
                        .import_notification_stream()
                        .map(|_| EngineCommand::SealNewBlock { create_empty: false, finalize: true, parent_hash: None, sender: None })
                        .fuse();
                    Some(Box::pin(stream))
                } else {
                    None
                };

                // Rebuild the interval stream based on the current mode.
                interval_stream = if let Some(tick) = match mode { MiningMode::Interval { tick } | MiningMode::Mixed { tick } => Some(tick), _ => None, } {
                    let stream = unfold(interval(Duration::from_secs(tick)), |mut interval| async {
                        interval.tick().await;
                        Some((EngineCommand::SealNewBlock { create_empty: true, finalize: true, parent_hash: None, sender: None }, interval))
                    }).fuse();
                    Some(Box::pin(stream))
                } else {
                    None
                };
            },

            // Poll the manual commands receiver
            maybe_cmd = manual_commands_receiver.next() => {
                if let Some(cmd) = maybe_cmd {
                    if command_sink.send(cmd).await.is_err() {
                        break;
                    }
                } else {
                    break;
                }
            },
            // Poll the auto-mine stream if it exists
            maybe_cmd = async {
                match auto_mine_stream.as_mut() {
                    Some(stream) => stream.next().await,
                    None => futures::future::pending().await,
                }
            } => {
                if let Some(cmd) = maybe_cmd {
                    if command_sink.send(cmd).await.is_err() {
                        break;
                    }
                }
            },
            // Poll the interval stream if it exists
            maybe_cmd = async {
                match interval_stream.as_mut() {
                    Some(stream) => stream.next().await,
                    None => futures::future::pending().await,
                }
            } => {
                if let Some(cmd) = maybe_cmd {
                    if command_sink.send(cmd).await.is_err() {
                        break;
                    }
                }
            },
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

    async fn set_interval_mining(&self, interval: u64) -> RpcResult<()> {
        let new_mode = if interval <= 0 {
            MiningMode::Manual
        } else {
            MiningMode::Interval { tick: interval }
        };
        *self.mining_engine.mining_mode.write() = new_mode;
        self.mining_engine.inner.wake();
        Ok(())
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
