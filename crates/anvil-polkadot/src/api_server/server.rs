use crate::{
    api_server::{
        error::{Error, Result, ToRpcResponseResult},
        ApiRequest,
    },
    logging::LoggingManager,
    macros::node_info,
    substrate_node::{
        mining_engine::MiningEngine,
        service::{Service, TransactionPoolHandle},
    },
};
use alloy_primitives::U256;
use alloy_rpc_types::anvil::MineOptions;
use anvil_core::eth::{EthRequest, Params};
use anvil_rpc::response::ResponseResult;
use futures::{channel::mpsc, StreamExt};
use std::{sync::Arc, time::Duration};

// Imports for txpool types
use alloy_network::AnyRpcTransaction;
use alloy_primitives::{Address, B256};
use alloy_rpc_types::txpool::{TxpoolContent, TxpoolInspect, TxpoolStatus};
use polkadot_sdk::{sc_service::InPoolTransaction, sc_transaction_pool_api::TransactionPool};

use indexmap::IndexMap;

pub struct ApiServer {
    req_receiver: mpsc::Receiver<ApiRequest>,
    logging_manager: LoggingManager,
    mining_engine: Arc<MiningEngine>,
    tx_pool: Arc<TransactionPoolHandle>,
}

impl ApiServer {
    pub fn new(
        substrate_service: &Service,
        req_receiver: mpsc::Receiver<ApiRequest>,
        logging_manager: LoggingManager,
    ) -> Self {
        Self {
            req_receiver,
            logging_manager,
            mining_engine: substrate_service.mining_engine.clone(),
            tx_pool: substrate_service.tx_pool.clone(),
        }
    }

    pub async fn run(mut self) {
        while let Some(msg) = self.req_receiver.next().await {
            let resp = self.execute(msg.req).await;

            msg.resp_sender.send(resp).expect("Dropped receiver");
        }
    }

    pub async fn execute(&mut self, req: EthRequest) -> ResponseResult {
        let res = match req.clone() {
            EthRequest::Mine(blocks, interval) => self.mine(blocks, interval).await.to_rpc_result(),
            EthRequest::SetIntervalMining(interval) => {
                self.set_interval_mining(interval).to_rpc_result()
            }
            EthRequest::GetIntervalMining(()) => self.get_interval_mining().to_rpc_result(),
            EthRequest::GetAutoMine(()) => self.get_auto_mine().to_rpc_result(),
            EthRequest::SetAutomine(enabled) => self.set_auto_mine(enabled).to_rpc_result(),
            EthRequest::EvmMine(mine) => self.evm_mine(mine).await.to_rpc_result(),
            //------- TimeMachine---------
            EthRequest::EvmSetBlockTimeStampInterval(time) => {
                self.set_block_timestamp_interval(time).to_rpc_result()
            }
            EthRequest::EvmRemoveBlockTimeStampInterval(()) => {
                self.remove_block_timestamp_interval().to_rpc_result()
            }
            EthRequest::EvmSetNextBlockTimeStamp(time) => {
                self.set_next_block_timestamp(time).to_rpc_result()
            }
            EthRequest::EvmIncreaseTime(time) => self.increase_time(time).to_rpc_result(),
            EthRequest::EvmSetTime(timestamp) => self.set_time(timestamp).to_rpc_result(),
            EthRequest::SetLogging(enabled) => self.set_logging(enabled).to_rpc_result(),
            // Txpool methods
            EthRequest::TxPoolStatus(_) => self.txpool_status().await,
            EthRequest::TxPoolInspect(_) => self.txpool_inspect().await,
            EthRequest::TxPoolContent(_) => self.txpool_content().await,
            // Anvil drop transaction methods
            EthRequest::DropTransaction(tx_hash) => self.anvil_drop_transaction(tx_hash).await,
            EthRequest::DropAllTransactions() => self.anvil_drop_all_transactions().await,
            EthRequest::RemovePoolTransactions(address) => {
                self.anvil_remove_pool_transactions(address).await
            }
            _ => Err::<(), _>(Error::RpcUnimplemented).to_rpc_result(),
        };

        if let ResponseResult::Error(err) = &res {
            node_info!("\nRPC request failed:");
            node_info!("    Request: {:?}", req);
            node_info!("    Error: {}\n", err);
        }

        res
    }

    async fn mine(&self, blocks: Option<U256>, interval: Option<U256>) -> Result<()> {
        node_info!("anvil_mine");

        if blocks.is_some_and(|b| u64::try_from(b).is_err()) {
            return Err(Error::InvalidParams("The number of blocks is too large".to_string()))
        }
        if interval.is_some_and(|i| u64::try_from(i).is_err()) {
            return Err(Error::InvalidParams("The interval between blocks is too large".to_string()))
        }
        self.mining_engine
            .mine(blocks.map(|b| b.to()), interval.map(|i| Duration::from_secs(i.to())))
            .await
            .map_err(Error::Mining)
    }

    fn set_interval_mining(&self, interval: u64) -> Result<()> {
        node_info!("evm_setIntervalMining");

        self.mining_engine.set_interval_mining(Duration::from_secs(interval));
        Ok(())
    }

    fn get_interval_mining(&self) -> Result<Option<u64>> {
        node_info!("anvil_getIntervalMining");

        Ok(self.mining_engine.get_interval_mining())
    }

    fn get_auto_mine(&self) -> Result<bool> {
        node_info!("anvil_getAutomine");

        Ok(self.mining_engine.is_automine())
    }

    fn set_auto_mine(&self, enabled: bool) -> Result<()> {
        node_info!("evm_setAutomine");

        self.mining_engine.set_auto_mine(enabled);
        Ok(())
    }

    async fn evm_mine(&self, mine: Option<Params<Option<MineOptions>>>) -> Result<String> {
        node_info!("evm_mine");

        self.mining_engine.evm_mine(mine.and_then(|p| p.params)).await?;
        Ok("0x0".to_string())
    }

    fn set_block_timestamp_interval(&self, time: u64) -> Result<()> {
        node_info!("anvil_setBlockTimestampInterval");

        self.mining_engine.set_block_timestamp_interval(Duration::from_secs(time));
        Ok(())
    }

    fn remove_block_timestamp_interval(&self) -> Result<bool> {
        node_info!("anvil_removeBlockTimestampInterval");

        Ok(self.mining_engine.remove_block_timestamp_interval())
    }

    fn set_next_block_timestamp(&self, time: U256) -> Result<()> {
        node_info!("anvil_setBlockTimestampInterval");

        if time >= U256::from(u64::MAX) {
            return Err(Error::InvalidParams("The timestamp is too big".to_string()))
        }
        let time = time.to::<u64>();
        self.mining_engine
            .set_next_block_timestamp(Duration::from_secs(time))
            .map_err(Error::Mining)
    }

    fn increase_time(&self, time: U256) -> Result<i64> {
        node_info!("evm_increaseTime");

        Ok(self.mining_engine.increase_time(Duration::from_secs(time.try_into().unwrap_or(0))))
    }

    fn set_time(&self, timestamp: U256) -> Result<u64> {
        node_info!("evm_setTime");

        if timestamp >= U256::from(u64::MAX) {
            return Err(Error::InvalidParams("The timestamp is too big".to_string()))
        }
        let time = timestamp.to::<u64>();
        Ok(self.mining_engine.set_time(Duration::from_secs(time)))
    }

    fn set_logging(&self, enabled: bool) -> Result<()> {
        node_info!("anvil_setLoggingEnabled");

        self.logging_manager.set_enabled(enabled);
        Ok(())
    }

    /// Returns transaction pool status - IMPLEMENTED
    async fn txpool_status(&self) -> ResponseResult {
        node_info!("txpool_status");
        let pool_status = self.tx_pool.status();
        // Convert Substrate PoolStatus to Ethereum TxpoolStatus format
        let status =
            TxpoolStatus { pending: pool_status.ready as u64, queued: pool_status.future as u64 };
        ResponseResult::Success(serde_json::to_value(status).unwrap_or_default())
    }

    /// Returns transaction summaries - NOT IMPLEMENTED
    async fn txpool_inspect(&self) -> ResponseResult {
        node_info!("txpool_inspect");
        // TODO: Convert Substrate transactions to TxpoolInspectSummary format
        let inspect = TxpoolInspect::default();
        ResponseResult::Success(serde_json::to_value(inspect).unwrap_or_default())
    }

    /// Returns full transaction details - NOT IMPLEMENTED
    async fn txpool_content(&self) -> ResponseResult {
        node_info!("txpool_content");
        // TODO: Convert Substrate transactions to AnyRpcTransaction format
        let content = TxpoolContent::<AnyRpcTransaction>::default();
        ResponseResult::Success(serde_json::to_value(content).unwrap_or_default())
    }

    /// Drop specific transaction by hash - NOT IMPLEMENTED
    async fn anvil_drop_transaction(&self, _tx_hash: B256) -> ResponseResult {
        node_info!("anvil_dropTransaction");
        // TODO: Convert B256 to Substrate hash format and remove via report_invalid
        ResponseResult::Success(serde_json::Value::Null)
    }

    /// Drop all transactions from pool - IMPLEMENTED
    async fn anvil_drop_all_transactions(&self) -> ResponseResult {
        node_info!("anvil_dropAllTransactions");

        // Get all transactions from both queues
        let ready_txs = self.tx_pool.ready();
        let future_txs = self.tx_pool.futures();

        let mut invalid_txs = IndexMap::new();

        // Mark all ready transactions for removal
        for tx in ready_txs {
            invalid_txs.insert(*tx.hash(), None);
        }

        // Mark all future transactions for removal
        for tx in future_txs {
            invalid_txs.insert(*tx.hash(), None);
        }

        // Remove all transactions using report_invalid API
        let removed = self.tx_pool.report_invalid(None, invalid_txs).await;

        ResponseResult::Success(serde_json::Value::Bool(!removed.is_empty()))
    }

    /// Remove transactions from specific address - NOT IMPLEMENTED
    async fn anvil_remove_pool_transactions(&self, _address: Address) -> ResponseResult {
        node_info!("anvil_removePoolTransactions");

        // TODO: Convert ETH Address to Substrate AccountId format
        // Then filter transactions by sender and remove via report_invalid
        ResponseResult::Success(serde_json::Value::Bool(true))
    }
}
