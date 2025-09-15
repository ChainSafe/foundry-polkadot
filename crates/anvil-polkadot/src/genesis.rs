//! Genesis settings

use crate::config::AnvilNodeConfig;
use alloy_genesis::GenesisAccount;
use alloy_primitives::{Address, U256};
use foundry_evm::revm::primitives::AccountInfo;
use std::collections::BTreeMap;

/// Genesis settings
#[derive(Clone, Debug, Default)]
pub struct GenesisConfig {
    /// The chain id of the Substrate chain, if provided.
    pub chain_id: Option<u64>,
    /// The initial timestamp for the genesis block
    pub timestamp: Option<u64>,
    /// The genesis block author address, if provided.
    pub coinbase: Option<Address>,
    /// All accounts that should be initialised at genesis with their info.
    pub alloc: Option<BTreeMap<Address, GenesisAccount>>,
    /// The initial number for the genesis block
    pub number: Option<u64>,
    /// The genesis header base fee
    pub base_fee_per_gas: Option<u128>,
    /// The genesis header gas limit.
    pub gas_limit: Option<u64>,
}

impl From<AnvilNodeConfig> for GenesisConfig {
    fn from(anvil_config: AnvilNodeConfig) -> Self {
        Self {
            chain_id: anvil_config.get_chain_id(),
            timestamp: anvil_config.get_genesis_timestamp(),
            coinbase: anvil_config.genesis.as_ref().map(|g| g.coinbase),
            alloc: anvil_config.genesis.as_ref().map(|g| g.alloc),
            number: anvil_config.get_genesis_number(),
            base_fee_per_gas: anvil_config.get_base_fee(),
            gas_limit: if cfg.disable_block_gas_limit {
                None
            } else {
                Some(anvil_config.gas_limit)
            },
        }
    }
}
