use crate::substrate_node::service::Service;
use anvil_core::eth::EthRequest;
use anvil_rpc::response::ResponseResult;
use futures::channel::{mpsc, oneshot};
use server::ApiServer;
use std::time::Instant;

mod server;

pub type ApiHandle = mpsc::Sender<ApiRequest>;

pub struct ApiRequest {
    pub req: EthRequest,
    pub resp_sender: oneshot::Sender<ResponseResult>,
}

pub fn spawn(substrate_service: &Service, start_time: Instant) -> ApiHandle {
    let (api_handle, receiver) = mpsc::channel(100);

    let api_server = ApiServer::new(substrate_service, receiver, start_time);

    let spawn_handle = substrate_service.task_manager.spawn_essential_handle();
    spawn_handle.spawn("anvil-api-server", "anvil", api_server.run());

    api_handle
}
