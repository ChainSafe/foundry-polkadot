use jsonrpsee::core::ClientError;
use polkadot_sdk::{
    sc_chain_spec,
    sp_api::__private::HeaderT,
    sp_runtime::{generic::SignedBlock, traits::Block as BlockT},
    sp_state_machine,
    sp_storage::{StorageData, StorageKey},
};
use serde::de::DeserializeOwned;

pub trait RPCClient<Block: BlockT + DeserializeOwned>: Send + Sync + std::fmt::Debug {
    fn system_chain(&self) -> Result<String, jsonrpsee::core::ClientError>;

    fn system_properties(&self) -> Result<sc_chain_spec::Properties, jsonrpsee::core::ClientError>;

    fn block(
        &self,
        hash: Option<Block::Hash>,
    ) -> Result<Option<SignedBlock<Block>>, jsonrpsee::core::ClientError>;

    fn block_hash(
        &self,
        block_number: Option<<Block::Header as HeaderT>::Number>,
    ) -> Result<Option<Block::Hash>, jsonrpsee::core::ClientError>;

    fn header(
        &self,
        hash: Option<Block::Hash>,
    ) -> Result<Option<Block::Header>, jsonrpsee::core::ClientError>;

    fn storage(
        &self,
        key: StorageKey,
        at: Option<Block::Hash>,
    ) -> Result<Option<StorageData>, jsonrpsee::core::ClientError>;

    fn storage_hash(
        &self,
        key: StorageKey,
        at: Option<Block::Hash>,
    ) -> Result<Option<Block::Hash>, jsonrpsee::core::ClientError>;

    fn storage_keys_paged(
        &self,
        key: Option<StorageKey>,
        count: u32,
        start_key: Option<StorageKey>,
        at: Option<Block::Hash>,
    ) -> Result<Vec<sp_state_machine::StorageKey>, ClientError>;
}