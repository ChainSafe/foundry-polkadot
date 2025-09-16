//! Genesis settings

use crate::config::AnvilNodeConfig;
use alloy_genesis::GenesisAccount;
use alloy_primitives::{Address, U256};
use foundry_evm::revm::primitives::AccountInfo;
use std::collections::BTreeMap;

// Hex-encode key: 0x9527366927478e710d3f7fb77c6d1f89
pub const CHAIN_ID_KEY: [u8; 16] = [
    149u8, 39u8, 54u8, 105u8, 39u8, 71u8, 142u8, 113u8, 13u8, 63u8, 127u8, 183u8, 124u8, 109u8,
    31u8, 137u8,
];

// Hex-encode key: 0xf0c365c3cf59d671eb72da0e7a4113c49f1f0515f462cdcf84e0f1d6045dfcbb
// twox_128(b"Timestamp") ++ twox_128(b"Now")
// corresponds to `Timestamp::Now` storage item in pallet-timestamp
pub const TIMESTAMP_KEY: [u8; 32] = [
    240u8, 195u8, 101u8, 195u8, 207u8, 89u8, 214u8, 113u8, 235u8, 114u8, 218u8, 14u8, 122u8, 65u8,
    19u8, 196u8, 159u8, 31u8, 5u8, 21u8, 244u8, 98u8, 205u8, 207u8, 132u8, 224u8, 241u8, 214u8,
    4u8, 93u8, 252u8, 187u8,
];

// Hex-encode key: 0x26aa394eea5630e07c48ae0c9558cef702a5c1b19ab7a04f536c519aca4983ac
// twox_128(b"System") ++ twox_128(b"Number")
// corresponds to `System::Number` storage item in pallet-system
pub const BLOCK_NUMBER_KEY: [u8; 32] = [
    38u8, 170u8, 57u8, 78u8, 234u8, 86u8, 48u8, 224u8, 124u8, 72u8, 174u8, 12u8, 149u8, 88u8,
    206u8, 247u8, 2u8, 165u8, 193u8, 177u8, 154u8, 183u8, 160u8, 79u8, 83u8, 108u8, 81u8, 154u8,
    202u8, 73u8, 131u8, 172u8,
];

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

impl GenesisConfig {
    pub fn as_storage_key_value(&self) -> Vec<(Vec<u8>, Vec<u8>)> {
        let mut storage = Vec::new();
        if let Some(chain_id) = self.chain_id {
            storage.push((CHAIN_ID_KEY.to_vec(), chain_id.encode()));
        }
        if let Some(timestamp) = self.timestamp {
            storage.push((TIMESTAMP_KEY.to_vec(), timestamp.encode()));
        }
        if let Some(number) = self.number {
            storage.push((BLOCK_NUMBER_KEY.to_vec(), number.encode()));
        }
        // TODO: add other fields
        storage
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sp_io::hashing::twox_128;

    #[test]
    fn test_number_storage_key() {
        let system_hash = twox_128(b"System");
        let number_hash = twox_128(b"Number");
        let mut concatenated_number_hash = [0u8; 32];
        concatenated_number_hash[..16].copy_from_slice(&system_hash);
        concatenated_number_hash[16..].copy_from_slice(&number_hash);
        assert_eq!(BLOCK_NUMBER_KEY, concatenated_number_hash);
    }

    #[test]
    fn test_timestamp_storage_key() {
        let timestamp_hash = twox_128(b"Timestamp");
        let now_hash = twox_128(b"Now");
        let mut concatenated_timestamp_hash = [0u8; 32];
        concatenated_timestamp_hash[..16].copy_from_slice(&timestamp_hash);
        concatenated_timestamp_hash[16..].copy_from_slice(&now_hash);
        assert_eq!(TIMESTAMP_KEY, concatenated_timestamp_hash);
    }
}
