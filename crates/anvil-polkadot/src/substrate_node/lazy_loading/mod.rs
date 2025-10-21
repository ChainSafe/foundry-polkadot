use polkadot_sdk::{
    sc_chain_spec,
    sc_service::{ChainType, GenericChainSpec},
    sp_core::H256,
    sp_genesis_builder,
};
use std::path::PathBuf;
use substrate_runtime::WASM_BINARY;

pub mod backend;
pub mod call_executor;
pub mod rpc_client;

pub const LAZY_LOADING_LOG_TARGET: &str = "lazy-loading";

#[derive(Clone, Default)]
pub struct LazyLoadingConfig {
    pub state_rpc: String,
    pub from_block: Option<H256>,
    pub state_overrides_path: Option<PathBuf>,
    pub runtime_override: Option<PathBuf>,
    pub delay_between_requests: u32,
    pub max_retries_per_request: u32,
}

pub fn spec_builder() -> sc_chain_spec::ChainSpecBuilder {
    GenericChainSpec::builder(
        WASM_BINARY.expect("WASM binary was not build, please build it!"),
        Default::default(),
    )
    .with_name("Lazy Loading")
    .with_id("lazy_loading")
    .with_chain_type(ChainType::Development)
    .with_genesis_config_preset_name(sp_genesis_builder::DEV_RUNTIME_PRESET)
}
