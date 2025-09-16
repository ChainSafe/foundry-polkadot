use crate::{
    api_server::{
        error::{Error, Result},
        ApiRequest,
    },
    logging::LoggingManager,
    macros::node_info,
    substrate_node::{error::ToRpcResponseResult, mining_engine::MiningEngine},
};
use alloy_primitives::{B256, U256, U64};
use anvil_core::eth::EthRequest;
use anvil_rpc::{error::RpcError, response::ResponseResult};
use futures::{channel::mpsc, StreamExt};
use polkadot_sdk::{
    pallet_revive::evm::{Account, ReceiptInfo},
    pallet_revive_eth_rpc::{
        client::Client as EthRpcClient, subxt_client::SrcChainConfig, ReceiptExtractor,
        ReceiptProvider, SubxtBlockInfoProvider,
    },
    sc_service::RpcHandlers,
};
use sqlx::sqlite::SqlitePoolOptions;
use std::{sync::Arc, time::Duration};
use subxt::{
    backend::rpc::{RawRpcFuture, RawRpcSubscription, RawValue, RpcClient, RpcClientT},
    ext::{
        jsonrpsee::core::traits::ToRpcParams,
        subxt_rpcs::{Error as SubxtRpcError, LegacyRpcMethods},
    },
    OnlineClient,
};

pub struct Wallet {
    _accounts: Vec<Account>,
}

pub struct ApiServer {
    req_receiver: mpsc::Receiver<ApiRequest>,
    logging_manager: LoggingManager,
    mining_engine: Arc<MiningEngine>,
    eth_rpc_client: EthRpcClient,
    _wallet: Wallet,
}

struct InMemoryRpcClient(RpcHandlers);

struct Params(Option<Box<RawValue>>);

impl ToRpcParams for Params {
    fn to_rpc_params(self) -> std::result::Result<Option<Box<RawValue>>, serde_json::Error> {
        Ok(self.0)
    }
}

impl RpcClientT for InMemoryRpcClient {
    fn request_raw<'a>(
        &'a self,
        method: &'a str,
        params: Option<Box<RawValue>>,
    ) -> RawRpcFuture<'a, Box<RawValue>> {
        Box::pin(async move {
            self.0
                .handle()
                .call(method, Params(params))
                .await
                .map_err(|err| SubxtRpcError::Client(Box::new(err)))
        })
    }

    fn subscribe_raw<'a>(
        &'a self,
        _sub: &'a str,
        _params: Option<Box<RawValue>>,
        _unsub: &'a str,
    ) -> RawRpcFuture<'a, RawRpcSubscription> {
        unimplemented!("Not needed")
    }
}

impl ApiServer {
    pub async fn new(
        mining_engine: Arc<MiningEngine>,
        rpc_handlers: RpcHandlers,
        req_receiver: mpsc::Receiver<ApiRequest>,
        logging_manager: LoggingManager,
    ) -> Self {
        let rpc_client = RpcClient::new(InMemoryRpcClient(rpc_handlers));
        let api =
            OnlineClient::<SrcChainConfig>::from_rpc_client(rpc_client.clone()).await.unwrap();
        let rpc = LegacyRpcMethods::<SrcChainConfig>::new(rpc_client.clone());

        let block_provider = SubxtBlockInfoProvider::new(api.clone(), rpc.clone()).await.unwrap();

        let (pool, keep_latest_n_blocks) = {
            // see sqlite in-memory issue: https://github.com/launchbadge/sqlx/issues/2510
            let pool = SqlitePoolOptions::new()
                .max_connections(1)
                .idle_timeout(None)
                .max_lifetime(None)
                .connect("sqlite::memory:")
                .await
                .unwrap();

            (pool, Some(100))
        };

        let receipt_extractor = ReceiptExtractor::new(api.clone(), None).await.unwrap();

        let receipt_provider = ReceiptProvider::new(
            pool,
            block_provider.clone(),
            receipt_extractor.clone(),
            keep_latest_n_blocks,
        )
        .await
        .unwrap();

        let eth_rpc_client =
            EthRpcClient::new(api, rpc_client, rpc, block_provider, receipt_provider)
                .await
                .unwrap();

        Self {
            req_receiver,
            logging_manager,
            mining_engine,
            eth_rpc_client,
            _wallet: Wallet { _accounts: vec![] },
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
            EthRequest::SetLogging(enabled) => self.set_logging(enabled).to_rpc_result(),
            EthRequest::EthChainId(()) => self.eth_chain_id().to_rpc_result(),
            EthRequest::EthNetworkId(()) => self.network_id().to_rpc_result(),
            EthRequest::NetListening(()) => self.net_listening().to_rpc_result(),
            EthRequest::EthSyncing(()) => self.syncing().to_rpc_result(),
            EthRequest::EthGetTransactionReceipt(tx_hash) => {
                self.transaction_receipt(tx_hash).await.to_rpc_result()
            }
            EthRequest::Mine(blocks, interval) => {
                if blocks.is_some_and(|b| u64::try_from(b).is_err()) {
                    return ResponseResult::Error(RpcError::invalid_params(
                        "The number of blocks is too large",
                    ));
                }
                if interval.is_some_and(|i| u64::try_from(i).is_err()) {
                    return ResponseResult::Error(RpcError::invalid_params(
                        "The interval between blocks is too large",
                    ));
                }
                self.mining_engine
                    .mine(blocks.map(|b| b.to()), interval.map(|i| Duration::from_secs(i.to())))
                    .await
                    .to_rpc_result()
            }
            EthRequest::SetIntervalMining(interval) => self
                .mining_engine
                .set_interval_mining(Duration::from_secs(interval))
                .to_rpc_result(),
            EthRequest::GetIntervalMining(()) => {
                self.mining_engine.get_interval_mining().to_rpc_result()
            }
            EthRequest::GetAutoMine(()) => self.mining_engine.get_auto_mine().to_rpc_result(),
            EthRequest::SetAutomine(enabled) => {
                self.mining_engine.set_auto_mine(enabled).to_rpc_result()
            }
            EthRequest::EvmMine(mine) => {
                self.mining_engine.evm_mine(mine.and_then(|p| p.params)).await.to_rpc_result()
            }
            EthRequest::EvmMineDetailed(_mine) => ResponseResult::Error(RpcError::internal_error()),
            //------- TimeMachine---------
            EthRequest::EvmSetBlockTimeStampInterval(time) => self
                .mining_engine
                .set_block_timestamp_interval(Duration::from_secs(time))
                .to_rpc_result(),
            EthRequest::EvmRemoveBlockTimeStampInterval(()) => {
                self.mining_engine.remove_block_timestamp_interval().to_rpc_result()
            }
            EthRequest::EvmSetNextBlockTimeStamp(time) => {
                if time >= U256::from(u64::MAX) {
                    return ResponseResult::Error(RpcError::invalid_params(
                        "The timestamp is too big",
                    ))
                }
                let time = time.to::<u64>();
                self.mining_engine
                    .set_next_block_timestamp(Duration::from_secs(time))
                    .to_rpc_result()
            }
            EthRequest::EvmIncreaseTime(time) => self
                .mining_engine
                .increase_time(Duration::from_secs(time.try_into().unwrap_or(0)))
                .to_rpc_result(),
            EthRequest::EvmSetTime(timestamp) => {
                if timestamp >= U256::from(u64::MAX) {
                    return ResponseResult::Error(RpcError::invalid_params(
                        "The timestamp is too big",
                    ))
                }
                // Make sure here we are not traveling back in time.
                let time = timestamp.to::<u64>();
                self.mining_engine.set_time(Duration::from_secs(time)).to_rpc_result()
            }
            EthRequest::EthEstimateGas(_call, _block, _overrides) => {
                //self.estimate_gas(call, block).await.to_rpc_result()
                Err::<(), _>(Error::RpcUnimplemented).to_rpc_result()
            }

            _ => Err::<(), _>(Error::RpcUnimplemented).to_rpc_result(),
        };

        if let ResponseResult::Error(err) = &res {
            node_info!("\nRPC request failed:");
            node_info!("    Request: {:?}", res);
            node_info!("    Error: {}\n", err);
        }

        res
    }

    fn set_logging(&self, enabled: bool) -> Result<()> {
        node_info!("anvil_setLoggingEnabled");
        self.logging_manager.set_enabled(enabled);
        Ok(())
    }

    fn eth_chain_id(&self) -> Result<U64> {
        node_info!("eth_chainId");
        Ok(U256::from(self.eth_rpc_client.chain_id()).to::<U64>())
    }

    fn network_id(&self) -> Result<u64> {
        node_info!("eth_networkId");
        Ok(self.eth_rpc_client.chain_id())
    }

    fn net_listening(&self) -> Result<bool> {
        node_info!("net_listening");
        Ok(true)
    }

    fn syncing(&self) -> Result<bool> {
        node_info!("eth_syncing");
        Ok(false)
    }

    async fn transaction_receipt(&self, tx_hash: B256) -> Result<Option<ReceiptInfo>> {
        node_info!("eth_getTransactionReceipt");
        // TODO: do we really need to return Ok(None) if the transaction is still in the pool?
        Ok(self.eth_rpc_client.receipt(&(tx_hash.0.into())).await)
    }

    //async fn estimate_gas(
    //    &self,
    //    request: WithOtherFields<TransactionRequest>,
    //    block: Option<alloy_rpc_types::BlockId>,
    //) -> Result<U256> {
    //    node_info!("eth_estimateGas");
    //
    //    let hash = self.eth_rpc_client.block_hash_for_tag(block.into()).await?;
    //    let runtime_api = self.eth_rpc_client.runtime_api(hash);
    //    /*
    //    GenericTransaction {
    //				from: Some(from),
    //				input: input.clone().into(),
    //				value: Some(value),
    //				gas_price: Some(gas_price),
    //				to,
    //				..Default::default()
    //			}, */
    //    let tr = request.into_inner();
    //    let dry_run = runtime_api.dry_run(GenericTransaction {
    //        from: Some(tr.from.unwrap().into()),
    //        ..Default::default()
    //    }).await?;
    //    Ok(dry_run.eth_gas)
    //}
}
