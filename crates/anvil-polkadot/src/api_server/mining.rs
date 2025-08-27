use alloy_primitives::U256;
use jsonrpsee::{core::RpcResult, proc_macros::rpc};

#[rpc(client, server)]
pub trait AnvilPolkadotMiningApi {
    // --- Mining Control ---
    #[method(name = "getAutomine")]
    async fn get_auto_mine(&self) -> RpcResult<bool>;
    #[method(name = "setAutomine")]
    async fn set_auto_mine(&self, enabled: bool) -> RpcResult<()>;
    #[method(name = "eth_getIntervalMining")]
    async fn get_interval_mining(&self) -> RpcResult<Option<u64>>;
    #[method(name = "eth_setIntervalMining")]
    async fn set_interval_mining(&self, interval: u64) -> RpcResult<()>;
    #[method(name = "setBlockTimestampInterval")]
    async fn set_block_timestamp_interval(&self, interval: u64) -> RpcResult<()>;
    #[method(name = "removeBlockTimestampInterval")]
    async fn remove_block_timestamp_interval(&self) -> RpcResult<()>;

    // --- Manual Mining ---
    // Corrected signature to match the original
    #[method(name = "mine")]
    async fn mine(&self, num_blocks: Option<U256>, time_offset: Option<U256>) -> RpcResult<()>;
    // Corrected signature
    //#[method(name = "evm_mine")]
    //async fn evm_mine(&self, options: Option<Params<Option<MineOptions>>>) -> RpcResult<()>;
    //// Corrected signature
    //#[method(name = "evm_mine_detailed")]
    //async fn evm_mine_detailed(&self, options: Option<Params<Option<MineOptions>>>) ->
    // RpcResult<()>;

    // --- Timestamp and Time ---
    // Corrected signatures to use U256
    #[method(name = "evm_increaseTime")]
    async fn increase_time(&self, increase_by_secs: U256) -> RpcResult<U256>;
    #[method(name = "evm_setNextBlockTimestamp")]
    async fn set_next_block_timestamp(&self, timestamp: U256) -> RpcResult<()>;
    #[method(name = "evm_setTime")]
    async fn set_time(&self, timestamp: U256) -> RpcResult<U256>;
}
