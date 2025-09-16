use crate::substrate_node::error::{to_rpc_result, ToRpcResponseResult};
use anvil_rpc::{error::RpcError, response::ResponseResult};
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Rpc Endpoint not implemented")]
    RpcUnimplemented,
}

pub type Result<T> = std::result::Result<T, Error>;

impl<T: Serialize> ToRpcResponseResult for Result<T> {
    fn to_rpc_result(self) -> ResponseResult {
        match self {
            Ok(val) => to_rpc_result(val),
            Err(err) => RpcError::internal_error_with(err.to_string()).into(),
        }
    }
}
