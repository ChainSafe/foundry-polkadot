use super::ApiRequest;
use crate::{
    api_server::mining::AnvilPolkadotMiningApiServer,
    substrate_node::{mining::MiningEngine, service::Service},
};
use alloy_primitives::U256;
use anvil_core::eth::EthRequest;
use anvil_rpc::{error::RpcError, response::ResponseResult};
use foundry_common::sh_println;
use futures::{channel::mpsc, StreamExt};
use jsonrpsee::core::RpcResult;
use std::sync::Arc;

pub struct ApiServer {
    req_receiver: mpsc::Receiver<ApiRequest>,
    mining_engine: Arc<MiningEngine>,
}

impl ApiServer {
    pub fn new(substrate_service: &Service, req_receiver: mpsc::Receiver<ApiRequest>) -> Self {
        Self { req_receiver, mining_engine: substrate_service.mining_engine.clone() }
    }

    pub async fn run(mut self) {
        while let Some(msg) = self.req_receiver.next().await {
            sh_println!("GOT REQUEST: {:?}", msg.req).unwrap();

            let resp = self.execute(msg.req).await;

            msg.resp_sender.send(resp).expect("Dropped receiver");
        }
    }

    pub async fn execute(&mut self, req: EthRequest) -> ResponseResult {
        match req {
            EthRequest::Mine(num_blocks, time_offset) => {
                match self.mine(num_blocks, time_offset).await {
                    Ok(()) => ResponseResult::success(()),
                    Err(_) => ResponseResult::Error(RpcError::internal_error()),
                }
            }
            _ => ResponseResult::Error(RpcError::internal_error()),
        }
    }
}

#[async_trait::async_trait]
impl AnvilPolkadotMiningApiServer for ApiServer {
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
