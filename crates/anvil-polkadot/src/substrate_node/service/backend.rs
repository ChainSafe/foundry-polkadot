use crate::substrate_node::service::{
    storage::{well_known_keys, AccountInfo},
    Backend,
};
use alloy_primitives::Address;
use codec::{Decode, Encode};
use lru::LruCache;
use parking_lot::Mutex;
use polkadot_sdk::{
    pallet_balances::AccountData,
    parachains_common::{AccountId, Hash, Nonce},
    sc_client_api::{Backend as BackendT, StateBackend, TrieCacheContext},
    sc_client_db::BlockchainDb,
    sp_blockchain,
    sp_core::H160,
    sp_state_machine::{StorageKey, StorageValue},
};
use std::{collections::HashMap, num::NonZeroUsize, sync::Arc};
use substrate_runtime::{Balance, Block};

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("Inner client error: {0}")]
    Client(#[from] sp_blockchain::Error),
    #[error("Could not find total issuance in the state")]
    MissingTotalIssuance,
    #[error("Unable to decode total issuance")]
    DecodeTotalIssuance(codec::Error),
    #[error("Unable to decode balance")]
    DecodeBalance(codec::Error),
    #[error("Unable to decode account info")]
    DecodeAccountInfo(codec::Error),
}

type Result<T> = std::result::Result<T, BackendError>;

pub struct BackendWithOverlay {
    backend: Arc<Backend>,
    overrides: Arc<Mutex<StorageOverrides>>,
}

impl BackendWithOverlay {
    pub fn new(backend: Arc<Backend>, overrides: Arc<Mutex<StorageOverrides>>) -> Self {
        Self { backend, overrides }
    }

    pub fn blockchain(&self) -> &BlockchainDb<Block> {
        self.backend.blockchain()
    }

    pub fn read_balance(
        &self,
        hash: Hash,
        account_id: AccountId,
    ) -> Result<Option<AccountData<Balance>>> {
        let key = well_known_keys::balance(account_id);

        self.read_state(hash, key)?
            .map(|value| {
                AccountData::<Balance>::decode(&mut &value[..])
                    .map_err(|err| BackendError::DecodeBalance(err))
            })
            .transpose()
    }

    pub fn read_total_issuance(&self, hash: Hash) -> Result<Balance> {
        let key = hex::decode(well_known_keys::TOTAL_ISSUANCE).unwrap();

        println!("total issuance key: {:?}", key.clone());

        let value = self.read_state(hash, key)?.ok_or(BackendError::MissingTotalIssuance)?;
        Balance::decode(&mut &value[..]).map_err(|err| BackendError::DecodeTotalIssuance(err))
    }

    pub fn read_account_info(&self, hash: Hash, address: Address) -> Result<Option<AccountInfo>> {
        let key = well_known_keys::account_info(H160::from_slice(address.as_slice()));

        self.read_state(hash, key)?
            .map(|value| {
                AccountInfo::decode(&mut &value[..])
                    .map_err(|err| BackendError::DecodeAccountInfo(err))
            })
            .transpose()
    }

    pub fn inject_nonce(&self, at: Hash, account_id: AccountId, value: Nonce) {
        let mut overrides = self.overrides.lock();
        overrides.set_nonce(at, account_id, value);
    }

    pub fn inject_chain_id(&self, at: Hash, chain_id: u64) {
        let mut overrides = self.overrides.lock();
        overrides.set_chain_id(at, chain_id);
    }

    pub fn inject_total_issuance(&self, at: Hash, value: Balance) {
        let mut overrides = self.overrides.lock();
        overrides.set_total_issuance(at, value);
    }

    pub fn inject_balance(&self, at: Hash, account_id: AccountId, value: AccountData<Balance>) {
        let mut overrides = self.overrides.lock();
        overrides.set_balance(at, account_id, value);
    }

    pub fn inject_account_info(&self, at: Hash, address: Address, info: AccountInfo) {
        let mut overrides = self.overrides.lock();
        overrides.set_account_info(at, address, info);
    }

    fn read_state(&self, hash: Hash, key: StorageKey) -> Result<Option<StorageValue>> {
        let maybe_overriden_val = {
            let mut guard = self.overrides.lock();

            guard.per_block.get(&hash).and_then(|overrides| overrides.get(&key).cloned())
        };

        if let Some(overriden_val) = maybe_overriden_val {
            return Ok(Some(overriden_val))
        }

        let state = self.backend.state_at(hash, TrieCacheContext::Trusted)?;
        Ok(state
            .storage(key.as_slice())
            .map_err(|e| sp_blockchain::Error::from_state(Box::new(e)))?)
    }
}

pub struct StorageOverrides {
    per_block: LruCache<Hash, HashMap<StorageKey, StorageValue>>,
}

impl StorageOverrides {
    pub fn new() -> Self {
        Self { per_block: LruCache::new(NonZeroUsize::new(10).expect("10 is greater than 0")) }
    }

    pub fn get(&mut self, block: &Hash) -> Option<HashMap<StorageKey, StorageValue>> {
        self.per_block.get(block).cloned()
    }

    fn set_chain_id(&mut self, latest_block: Hash, id: u64) {
        let mut changeset = HashMap::with_capacity(1);
        changeset.insert(well_known_keys::CHAIN_ID.to_vec(), id.encode());

        self.add(latest_block, changeset);
    }

    fn set_timestamp(&mut self, latest_block: Hash, timestamp: u64) {
        let mut changeset = HashMap::with_capacity(1);
        changeset.insert(well_known_keys::TIMESTAMP.to_vec(), timestamp.encode());

        self.add(latest_block, changeset);
    }

    fn set_nonce(&mut self, latest_block: Hash, account_id: AccountId, nonce: Nonce) {
        let mut changeset = HashMap::with_capacity(1);
        changeset.insert(well_known_keys::nonce(account_id), nonce.encode());

        self.add(latest_block, changeset);
    }

    fn set_total_issuance(&mut self, latest_block: Hash, value: Balance) {
        let mut changeset = HashMap::with_capacity(1);
        changeset
            .insert(hex::decode(well_known_keys::TOTAL_ISSUANCE).unwrap().to_vec(), value.encode());

        self.add(latest_block, changeset);
    }

    fn set_balance(
        &mut self,
        latest_block: Hash,
        account_id: AccountId,
        value: AccountData<Balance>,
    ) {
        let mut changeset = HashMap::with_capacity(1);
        changeset.insert(well_known_keys::balance(account_id), value.encode());

        self.add(latest_block, changeset);
    }

    fn set_account_info(&mut self, latest_block: Hash, address: Address, info: AccountInfo) {
        let mut changeset = HashMap::with_capacity(1);
        changeset.insert(
            well_known_keys::account_info(H160::from_slice(address.as_slice())),
            info.encode(),
        );

        self.add(latest_block, changeset);
    }

    fn add(&mut self, latest_block: Hash, changeset: HashMap<StorageKey, StorageValue>) {
        if let Some(per_block) = self.per_block.get_mut(&latest_block) {
            per_block.extend(changeset.into_iter());
        } else {
            self.per_block.put(latest_block, changeset);
        }
    }
}
