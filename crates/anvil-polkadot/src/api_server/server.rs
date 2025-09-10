use super::ApiRequest;
use crate::{logging::LoggingManager, macros::node_info};
use anvil_core::eth::EthRequest;
use anvil_rpc::{error::RpcError, response::ResponseResult};
use futures::{channel::mpsc, StreamExt};
use polkadot_sdk::{
    pallet_revive::evm::Account as EthRpcAccount,
    pallet_revive_eth_rpc::{
        client::Client as EthRpcClient, subxt_client::SrcChainConfig, EthRpcServerImpl,
        ReceiptExtractor, ReceiptProvider, SubxtBlockInfoProvider,
    },
    sc_service::RpcHandlers,
};
use sqlx::sqlite::SqlitePoolOptions;
use subxt::{
    backend::rpc::{RawRpcFuture, RawRpcSubscription, RawValue, RpcClient, RpcClientT},
    ext::{
        jsonrpsee::core::traits::ToRpcParams,
        subxt_rpcs::{Error as SubxtRpcError, LegacyRpcMethods},
    },
    OnlineClient,
};

pub struct ApiServer {
    req_receiver: mpsc::Receiver<ApiRequest>,
    logging_manager: LoggingManager,
    eth_rpc_client: EthRpcClient,
}

struct InMemoryRpcClient(RpcHandlers);

struct Params(Option<Box<RawValue>>);

impl ToRpcParams for Params {
    fn to_rpc_params(self) -> Result<Option<Box<RawValue>>, serde_json::Error> {
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

        // let eth_rpc_server =
        //     EthRpcServerImpl::new(eth_rpc_client).with_accounts(vec![EthRpcAccount::default()]);

        Self { req_receiver, logging_manager, eth_rpc_client }
    }

    pub async fn run(mut self) {
        while let Some(msg) = self.req_receiver.next().await {
            let resp = self.execute(msg.req).await;

            msg.resp_sender.send(resp).expect("Dropped receiver");
        }
    }

    pub async fn execute(&mut self, req: EthRequest) -> ResponseResult {
        match req {
            EthRequest::SetLogging(enabled) => {
                node_info!("anvil_setLoggingEnabled");
                self.logging_manager.set_enabled(enabled);
                ResponseResult::Success(serde_json::Value::Bool(true))
            }
            EthRequest::EthChainId(()) => {
                node_info!("eth_chainId");
                let chain_id = self.eth_rpc_client.chain_id();
                ResponseResult::Success(serde_json::Value::Number(chain_id.into()))
            }
            _ => ResponseResult::Error(RpcError::internal_error()),
        }
    }
}
