use anvil_core::eth::EthRequest;
use anvil_rpc::{error::RpcError, response::ResponseResult};
use futures::{
    channel::{mpsc, oneshot},
    StreamExt,
};

use crate::substrate_node::service::Service;

pub struct Responder {
    req_receiver: mpsc::Receiver<ApiRequest>,
}

impl Responder {
    fn new(substrate_service: &Service, rx: mpsc::Receiver<ApiRequest>) -> Self {
        Self { req_receiver: rx }
    }

    async fn run(mut self) {
        while let Some(msg) = self.req_receiver.next().await {
            eprintln!("GOT REQUEST: {:?}", msg.req);
            msg.resp_sender
                .send(ResponseResult::Error(RpcError::internal_error()))
                .expect("Dropped receiver");
        }
    }
}

pub struct ApiRequest {
    pub req: EthRequest,
    pub resp_sender: oneshot::Sender<ResponseResult>,
}

pub type ResponderHandle = mpsc::Sender<ApiRequest>;

pub fn spawn(substrate_service: &Service) -> ResponderHandle {
    let (tx, rx) = mpsc::channel(100);

    let responder = Responder::new(&substrate_service, rx);

    let spawn_handle = substrate_service.task_manager.spawn_essential_handle();

    spawn_handle.spawn("anvil-api-responder", "anvil", responder.run());

    tx
}
