use super::ApiRequest;
use crate::{
    logging::LoggingManager,
    macros::node_info,
    substrate_node::service::{Service, StorageOverrides},
};
use anvil_core::eth::EthRequest;
use anvil_rpc::{error::RpcError, response::ResponseResult};
use futures::{channel::mpsc, StreamExt};
use parking_lot::Mutex;
use polkadot_sdk::sc_client_api::{Backend as _, HeaderBackend};
use std::sync::Arc;

pub struct ApiServer {
    req_receiver: mpsc::Receiver<ApiRequest>,
    storage_overrides: Arc<Mutex<StorageOverrides>>,
    backend: Arc<Backend>,
    logging_manager: LoggingManager,
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
            storage_overrides: substrate_service.storage_overrides.clone(),
            backend: substrate_service.backend.clone(),
        }
    }

    pub async fn run(mut self) {
        while let Some(msg) = self.req_receiver.next().await {
            let resp = self.execute(msg.req).await;

            msg.resp_sender.send(resp).expect("Dropped receiver");
        }
    }

    pub async fn execute(&mut self, req: EthRequest) -> ResponseResult {
        match req {
            EthRequest::SetChainId(chain_id) => {
                let latest_block = self.backend.blockchain().info().best_hash;

                {
                    let mut storage_overrides = self.storage_overrides.lock();
                    storage_overrides.set_chain_id(latest_block, chain_id);
                }

                ResponseResult::Success(serde_json::Value::Null)
            }
            EthRequest::SetLogging(enabled) => {
                node_info!("anvil_setLoggingEnabled");
                self.logging_manager.set_enabled(enabled);
                ResponseResult::Success(serde_json::Value::Bool(true))
            }
            _ => ResponseResult::Error(RpcError::internal_error()),
        }
    }
}
