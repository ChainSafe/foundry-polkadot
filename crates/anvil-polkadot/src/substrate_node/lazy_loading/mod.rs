use polkadot_sdk::sp_core::H256;
use std::path::PathBuf;

mod backend;
mod rpc_client;

pub const LAZY_LOADING_LOG_TARGET: &'static str = "lazy-loading";

#[derive(Clone)]
pub struct LazyLoadingConfig {
	pub state_rpc: url::Url,
	pub from_block: Option<H256>,
	pub state_overrides_path: Option<PathBuf>,
	pub runtime_override: Option<PathBuf>,
	pub delay_between_requests: u32,
	pub max_retries_per_request: u32,
}