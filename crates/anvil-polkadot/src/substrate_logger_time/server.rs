use super::ApiRequest;
use crate::substrate_node::service::Service;
use anvil_core::eth::EthRequest;
use anvil_rpc::{error::RpcError, response::ResponseResult};
use foundry_common::sh_println;
use futures::{channel::mpsc, StreamExt};
use std::time::Instant;

pub struct ApiServer {
    req_receiver: mpsc::Receiver<ApiRequest>,
    start_time: Instant,
    first_request_received: bool,
}

impl ApiServer {
    pub fn new(
        _substrate_service: &Service,
        req_receiver: mpsc::Receiver<ApiRequest>,
        start_time: Instant,
    ) -> Self {
        Self { req_receiver, start_time, first_request_received: false }
    }

    pub async fn run(mut self) {
        while let Some(msg) = self.req_receiver.next().await {
            if !self.first_request_received {
                let elapsed = self.start_time.elapsed();
                println!("Node is ready for RPC request in: {:?}", elapsed);
                self.first_request_received = true;
            }

            sh_println!("GOT REQUEST: {:?}", msg.req).unwrap();

            let resp = self.execute(msg.req).await;

            msg.resp_sender.send(resp).expect("Dropped receiver");
        }
    }

    pub async fn execute(&mut self, _req: EthRequest) -> ResponseResult {
        ResponseResult::Error(RpcError::internal_error())
    }
}
