use polkadot_sdk::{
    sc_client_api::StorageKey,
    sp_core,
    sp_runtime::{
        StateVersion,
        traits::{Block as BlockT, HashingFor},
    },
    sp_state_machine::{self, BackendTransaction, StorageCollection, TrieBackend},
    sp_storage::{self, ChildInfo},
    sp_trie::{self, PrefixedMemoryDB},
};

use std::{collections::HashMap, sync::Arc};

use super::rpc_client;
use parking_lot::RwLock;
use serde::de::DeserializeOwned;
use std::marker::PhantomData;

/// DB-backed patricia trie state, transaction type is an overlay of changes to commit.
pub type DbState<B> = TrieBackend<Arc<dyn sp_state_machine::Storage<HashingFor<B>>>, HashingFor<B>>;

/// A struct containing arguments for iterating over the storage.
#[derive(Default)]
pub struct RawIterArgs {
    /// The prefix of the keys over which to iterate.
    pub prefix: Option<Vec<u8>>,

    /// The prefix from which to start the iteration from.
    ///
    /// This is inclusive and the iteration will include the key which is specified here.
    pub start_at: Option<Vec<u8>>,

    /// If this is `true` then the iteration will *not* include
    /// the key specified in `start_at`, if there is such a key.
    pub start_at_exclusive: bool,
}

/// A raw iterator over the `BenchmarkingState`.
pub struct RawIter<Block: BlockT> {
    pub(crate) args: RawIterArgs,
    complete: bool,
    _phantom: PhantomData<Block>,
}

impl<Block: BlockT + DeserializeOwned> sp_state_machine::StorageIterator<HashingFor<Block>>
    for RawIter<Block>
{
    type Backend = ForkedLazyBackend<Block>;
    type Error = String;

    fn next_key(
        &mut self,
        backend: &Self::Backend,
    ) -> Option<Result<sp_state_machine::StorageKey, Self::Error>> {
        use sp_state_machine::Backend;

        let remote_fetch =
            |key: Option<StorageKey>, start_key: Option<StorageKey>, block: Option<Block::Hash>| {
                let result = backend.rpc_client.storage_keys_paged(key, 5, start_key, block);

                match result {
                    Ok(keys) => keys.first().map(|key| key.clone()),
                    Err(err) => {
                        log::trace!(
                            target: super::LAZY_LOADING_LOG_TARGET,
                            "Failed to fetch `next key` from RPC: {:?}",
                            err
                        );

                        None
                    }
                }
            };

        let prefix = self.args.prefix.clone().map(|k| StorageKey(k));
        let start_key = self.args.start_at.clone().map(|k| StorageKey(k));

        let maybe_next_key = if backend.before_fork {
            remote_fetch(prefix, start_key, backend.block_hash)
        } else {
            let mut iter_args = sp_state_machine::backend::IterArgs::default();
            iter_args.prefix = self.args.prefix.as_deref();
            iter_args.start_at = self.args.start_at.as_deref();
            iter_args.start_at_exclusive = true;
            iter_args.stop_on_incomplete_database = true;

            let readable_db = backend.db.read();
            let next_storage_key = readable_db
                .raw_iter(iter_args)
                .map(|mut iter| iter.next_key(&readable_db))
                .map(|op| op.and_then(|result| result.ok()))
                .ok()
                .flatten();

            // IMPORTANT: free storage read lock
            drop(readable_db);

            let removed_key = start_key
                .clone()
                .or(prefix.clone())
                .map(|key| backend.removed_keys.read().contains_key(&key.0))
                .unwrap_or(false);
            if next_storage_key.is_none() && !removed_key {
                let maybe_next_key = remote_fetch(prefix, start_key, Some(backend.fork_block));
                match maybe_next_key {
                    Some(key) if !backend.removed_keys.read().contains_key(&key) => Some(key),
                    _ => None,
                }
            } else {
                next_storage_key
            }
        };

        log::trace!(
            target: super::LAZY_LOADING_LOG_TARGET,
            "next_key: (prefix: {:?}, start_at: {:?}, next_key: {:?})",
            self.args.prefix.clone().map(|key| hex::encode(key)),
            self.args.start_at.clone().map(|key| hex::encode(key)),
            maybe_next_key.clone().map(|key| hex::encode(key))
        );

        if let Some(next_key) = maybe_next_key {
            if self
                .args
                .prefix
                .clone()
                .map(|filter_key| next_key.starts_with(&filter_key))
                .unwrap_or(false)
            {
                self.args.start_at = Some(next_key.clone());
                Some(Ok(next_key))
            } else {
                self.complete = true;
                None
            }
        } else {
            self.complete = true;
            None
        }
    }

    fn next_pair(
        &mut self,
        backend: &Self::Backend,
    ) -> Option<Result<(sp_state_machine::StorageKey, sp_state_machine::StorageValue), Self::Error>>
    {
        use sp_state_machine::Backend;

        let remote_fetch =
            |key: Option<StorageKey>, start_key: Option<StorageKey>, block: Option<Block::Hash>| {
                let result = backend.rpc_client.storage_keys_paged(key, 5, start_key, block);

                match result {
                    Ok(keys) => keys.first().map(|key| key.clone()),
                    Err(err) => {
                        log::trace!(
                            target: super::LAZY_LOADING_LOG_TARGET,
                            "Failed to fetch `next key` from RPC: {:?}",
                            err
                        );

                        None
                    }
                }
            };

        let prefix = self.args.prefix.clone().map(|k| StorageKey(k));
        let start_key = self.args.start_at.clone().map(|k| StorageKey(k));

        let maybe_next_key = if backend.before_fork {
            remote_fetch(prefix, start_key, backend.block_hash)
        } else {
            let mut iter_args = sp_state_machine::backend::IterArgs::default();
            iter_args.prefix = self.args.prefix.as_deref();
            iter_args.start_at = self.args.start_at.as_deref();
            iter_args.start_at_exclusive = true;
            iter_args.stop_on_incomplete_database = true;

            let readable_db = backend.db.read();
            let next_storage_key = readable_db
                .raw_iter(iter_args)
                .map(|mut iter| iter.next_key(&readable_db))
                .map(|op| op.and_then(|result| result.ok()))
                .ok()
                .flatten();

            // IMPORTANT: free storage read lock
            drop(readable_db);

            let removed_key = start_key
                .clone()
                .or(prefix.clone())
                .map(|key| backend.removed_keys.read().contains_key(&key.0))
                .unwrap_or(false);
            if next_storage_key.is_none() && !removed_key {
                let maybe_next_key = remote_fetch(prefix, start_key, Some(backend.fork_block));
                match maybe_next_key {
                    Some(key) if !backend.removed_keys.read().contains_key(&key) => Some(key),
                    _ => None,
                }
            } else {
                next_storage_key
            }
        };

        log::trace!(
            target: super::LAZY_LOADING_LOG_TARGET,
            "next_pair: (prefix: {:?}, start_at: {:?}, next_key: {:?})",
            self.args.prefix.clone().map(|key| hex::encode(key)),
            self.args.start_at.clone().map(|key| hex::encode(key)),
            maybe_next_key.clone().map(|key| hex::encode(key))
        );

        let maybe_value = maybe_next_key
            .clone()
            .and_then(|key| (*backend).storage(key.as_slice()).ok())
            .flatten();

        if let Some(next_key) = maybe_next_key {
            if self
                .args
                .prefix
                .clone()
                .map(|filter_key| next_key.starts_with(&filter_key))
                .unwrap_or(false)
            {
                self.args.start_at = Some(next_key.clone());

                match maybe_value {
                    Some(value) => Some(Ok((next_key, value))),
                    _ => None,
                }
            } else {
                self.complete = true;
                None
            }
        } else {
            self.complete = true;
            None
        }
    }

    fn was_complete(&self) -> bool {
        self.complete
    }
}

#[derive(Debug, Clone)]
pub struct ForkedLazyBackend<Block: BlockT> {
    rpc_client: Arc<rpc_client::RPC>,
    block_hash: Option<Block::Hash>,
    fork_block: Block::Hash,
    db: Arc<RwLock<sp_state_machine::InMemoryBackend<HashingFor<Block>>>>,
    removed_keys: Arc<RwLock<HashMap<Vec<u8>, ()>>>,
    before_fork: bool,
}

impl<Block: BlockT> ForkedLazyBackend<Block> {
    fn update_storage(&self, key: &[u8], value: &Option<Vec<u8>>) {
        if let Some(val) = value {
            let mut entries: HashMap<Option<ChildInfo>, StorageCollection> = Default::default();
            entries.insert(None, vec![(key.to_vec(), Some(val.clone()))]);

            self.db.write().insert(entries, StateVersion::V1);
        }
    }
}

impl<Block: BlockT + DeserializeOwned> sp_state_machine::Backend<HashingFor<Block>>
    for ForkedLazyBackend<Block>
{
    type Error = <DbState<Block> as sp_state_machine::Backend<HashingFor<Block>>>::Error;
    type TrieBackendStorage = PrefixedMemoryDB<HashingFor<Block>>;
    type RawIter = RawIter<Block>;

    fn storage(&self, key: &[u8]) -> Result<Option<sp_state_machine::StorageValue>, Self::Error> {
        let remote_fetch = |block: Option<Block::Hash>| {
            let result = self.rpc_client.storage(StorageKey(key.to_vec()), block);

            match result {
                Ok(data) => data.map(|v| v.0),
                Err(err) => {
                    log::debug!(
                        target: super::LAZY_LOADING_LOG_TARGET,
                        "Failed to fetch storage from live network: {:?}",
                        err
                    );
                    None
                }
            }
        };

        if self.before_fork {
            return Ok(remote_fetch(self.block_hash));
        }

        let readable_db = self.db.read();
        let maybe_storage = readable_db.storage(key);
        let value = match maybe_storage {
            Ok(Some(data)) => Some(data),
            _ if !self.removed_keys.read().contains_key(key) => {
                let result = remote_fetch(Some(self.fork_block));

                // Cache state
                drop(readable_db);
                self.update_storage(key, &result);

                result
            }
            _ => None,
        };

        Ok(value)
    }

    fn storage_hash(
        &self,
        key: &[u8],
    ) -> Result<Option<<HashingFor<Block> as sp_core::Hasher>::Out>, Self::Error> {
        let remote_fetch = |block: Option<Block::Hash>| {
            let result = self.rpc_client.storage_hash(StorageKey(key.to_vec()), block);

            match result {
                Ok(hash) => Ok(hash),
                Err(err) => Err(format!("Failed to fetch storage hash from RPC: {:?}", err).into()),
            }
        };

        if self.before_fork {
            return remote_fetch(self.block_hash);
        }

        let storage_hash = self.db.read().storage_hash(key);
        match storage_hash {
            Ok(Some(hash)) => Ok(Some(hash)),
            _ if !self.removed_keys.read().contains_key(key) => remote_fetch(Some(self.fork_block)),
            _ => Ok(None),
        }
    }

    fn closest_merkle_value(
        &self,
        _key: &[u8],
    ) -> Result<
        Option<sp_trie::MerkleValue<<HashingFor<Block> as sp_core::Hasher>::Out>>,
        Self::Error,
    > {
        unimplemented!("closest_merkle_value: unsupported feature for lazy loading")
    }

    fn child_closest_merkle_value(
        &self,
        _child_info: &sp_storage::ChildInfo,
        _key: &[u8],
    ) -> Result<
        Option<sp_trie::MerkleValue<<HashingFor<Block> as sp_core::Hasher>::Out>>,
        Self::Error,
    > {
        unimplemented!("child_closest_merkle_value: unsupported feature for lazy loading")
    }

    fn child_storage(
        &self,
        _child_info: &sp_storage::ChildInfo,
        _key: &[u8],
    ) -> Result<Option<sp_state_machine::StorageValue>, Self::Error> {
        unimplemented!("child_storage: unsupported feature for lazy loading");
    }

    fn child_storage_hash(
        &self,
        _child_info: &sp_storage::ChildInfo,
        _key: &[u8],
    ) -> Result<Option<<HashingFor<Block> as sp_core::Hasher>::Out>, Self::Error> {
        unimplemented!("child_storage_hash: unsupported feature for lazy loading");
    }

    fn next_storage_key(
        &self,
        key: &[u8],
    ) -> Result<Option<sp_state_machine::StorageKey>, Self::Error> {
        let remote_fetch = |block: Option<Block::Hash>| {
            let start_key = Some(StorageKey(key.to_vec()));
            let result = self.rpc_client.storage_keys_paged(start_key.clone(), 2, None, block);

            match result {
                Ok(keys) => keys.last().cloned(),
                Err(err) => {
                    log::trace!(
                        target: super::LAZY_LOADING_LOG_TARGET,
                        "Failed to fetch `next storage key` from RPC: {:?}",
                        err
                    );

                    None
                }
            }
        };

        let maybe_next_key = if self.before_fork {
            // Before the fork checkpoint, always fetch remotely
            remote_fetch(self.block_hash)
        } else {
            // Try to get the next storage key from the local DB
            let next_storage_key = self.db.read().next_storage_key(key);
            match next_storage_key {
                Ok(Some(next_key)) => Some(next_key),
                // If not found locally and key is not marked as removed, fetch remotely
                _ if !self.removed_keys.read().contains_key(key) => {
                    remote_fetch(Some(self.fork_block))
                }
                // Otherwise, there's no next key
                _ => None,
            }
        }
        .filter(|next_key| next_key != key);

        log::trace!(
            target: super::LAZY_LOADING_LOG_TARGET,
            "next_storage_key: (key: {:?}, next_key: {:?})",
            hex::encode(key),
            maybe_next_key.clone().map(|key| hex::encode(key))
        );

        Ok(maybe_next_key)
    }

    fn next_child_storage_key(
        &self,
        _child_info: &sp_storage::ChildInfo,
        _key: &[u8],
    ) -> Result<Option<sp_state_machine::StorageKey>, Self::Error> {
        unimplemented!("next_child_storage_key: unsupported feature for lazy loading");
    }

    fn storage_root<'a>(
        &self,
        delta: impl Iterator<Item = (&'a [u8], Option<&'a [u8]>)>,
        state_version: StateVersion,
    ) -> (<HashingFor<Block> as sp_core::Hasher>::Out, BackendTransaction<HashingFor<Block>>)
    where
        <HashingFor<Block> as sp_core::Hasher>::Out: Ord,
    {
        self.db.read().storage_root(delta, state_version)
    }

    fn child_storage_root<'a>(
        &self,
        _child_info: &sp_storage::ChildInfo,
        _delta: impl Iterator<Item = (&'a [u8], Option<&'a [u8]>)>,
        _state_version: StateVersion,
    ) -> (<HashingFor<Block> as sp_core::Hasher>::Out, bool, BackendTransaction<HashingFor<Block>>)
    where
        <HashingFor<Block> as sp_core::Hasher>::Out: Ord,
    {
        unimplemented!("child_storage_root: unsupported in lazy loading")
    }

    fn raw_iter(&self, args: sp_state_machine::IterArgs<'_>) -> Result<Self::RawIter, Self::Error> {
        let mut clone: RawIterArgs = Default::default();
        clone.start_at_exclusive = args.start_at_exclusive.clone();
        clone.prefix = args.prefix.map(|v| v.to_vec());
        clone.start_at = args.start_at.map(|v| v.to_vec());

        Ok(RawIter::<Block> { args: clone, complete: false, _phantom: Default::default() })
    }

    fn register_overlay_stats(&self, stats: &sp_state_machine::StateMachineStats) {
        self.db.read().register_overlay_stats(stats)
    }

    fn usage_info(&self) -> sp_state_machine::UsageInfo {
        self.db.read().usage_info()
    }
}

impl<B: BlockT> sp_state_machine::backend::AsTrieBackend<HashingFor<B>> for ForkedLazyBackend<B> {
    type TrieBackendStorage = PrefixedMemoryDB<HashingFor<B>>;

    fn as_trie_backend(
        &self,
    ) -> &sp_state_machine::TrieBackend<Self::TrieBackendStorage, HashingFor<B>> {
        unimplemented!("`as_trie_backend` is not supported in lazy loading mode.")
    }
}
