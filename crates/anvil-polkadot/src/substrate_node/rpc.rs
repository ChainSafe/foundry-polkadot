#![warn(missing_docs)]

use jsonrpsee::RpcModule;

use polkadot_sdk::{
    sc_transaction_pool_api::TransactionPool,
    sp_blockchain::{Error as BlockChainError, HeaderBackend, HeaderMetadata},
    *,
};

use std::sync::Arc;

// use substrate_runtime::Runtime;

mod interface {
    use polkadot_sdk::{polkadot_sdk_frame as frame, *};
    use substrate_runtime::Runtime;

    pub use frame::runtime::types_common::OpaqueBlock;
    pub type AccountId = <Runtime as frame_system::Config>::AccountId;
    pub type Nonce = <Runtime as frame_system::Config>::Nonce;
}

use super::mining::MiningEngine;
use crate::substrate_node::mining::RpcApiServer;
use interface::{AccountId, Nonce, OpaqueBlock};

/// Full client dependencies.

pub struct FullDeps<C, P> {
    /// The client instance to use.
    pub client: Arc<C>,
    /// Transaction pool instance.
    pub pool: Arc<P>,
    /// Used by RPC to forward commands to the block engine.
    //pub command_sink: Option<
    //    futures::channel::mpsc::Sender<
    //        sc_consensus_manual_seal::rpc::EngineCommand<interface::Hash>,
    //    >,
    //>,
    pub mining_engine: Arc<MiningEngine>,
}

/// Instantiate all full RPC extensions.

pub fn create_full<C, P>(
    deps: FullDeps<C, P>,
) -> Result<RpcModule<()>, Box<dyn std::error::Error + Send + Sync>>
where
    C: Send
        + Sync
        + 'static
        + sp_api::ProvideRuntimeApi<OpaqueBlock>
        + HeaderBackend<OpaqueBlock>
        + HeaderMetadata<OpaqueBlock, Error = BlockChainError>
        + 'static,

    C::Api: sp_block_builder::BlockBuilder<OpaqueBlock>,

    C::Api: substrate_frame_rpc_system::AccountNonceApi<OpaqueBlock, AccountId, Nonce>,

    P: TransactionPool + 'static,
{
    use crate::api_server::mining::AnvilPolkadotMiningApiServer;
    use polkadot_sdk::substrate_frame_rpc_system::{System, SystemApiServer};

    let mut module = RpcModule::new(());
    let FullDeps { client, pool, mining_engine } = deps;
    module.merge(System::new(client.clone(), pool.clone()).into_rpc())?;
    //if let Some(sink) = command_sink {
    //    module.merge(ManualSeal::new(sink).into_rpc())?;
    //}
    module.merge(RpcApiServer::new(mining_engine.clone()).into_rpc())?;

    Ok(module)
}
