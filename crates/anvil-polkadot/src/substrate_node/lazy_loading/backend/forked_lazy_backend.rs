use crate::substrate_node::lazy_loading::{LAZY_LOADING_LOG_TARGET, rpc_client::RPCClient};
use alloy_primitives::hex;
use polkadot_sdk::{
    sc_client_api::StorageKey,
    sp_core,
    sp_runtime::{
        StateVersion,
        traits::{Block as BlockT, HashingFor},
    },
    sp_state_machine::{
        self, BackendTransaction, InMemoryBackend, IterArgs, StorageCollection, StorageValue,
        TrieBackend, backend::AsTrieBackend,
    },
    sp_storage::ChildInfo,
    sp_trie::{self, PrefixedMemoryDB},
};
use serde::de::DeserializeOwned;
use std::{collections::HashMap, marker::PhantomData, sync::Arc};

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
}

/// A raw iterator over the storage keys.
pub struct RawIter<Block: BlockT + DeserializeOwned> {
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
                backend
                    .rpc()
                    .and_then(|rpc| rpc.storage_keys_paged(key, 5, start_key, block).ok())
                    .and_then(|keys| keys.first().cloned())
            };

        let prefix = self.args.prefix.clone().map(StorageKey);
        let start_key = self.args.start_at.clone().map(StorageKey);

        let maybe_next_key = if backend.before_fork {
            // If RPC client is available, fetch remotely
            if backend.rpc().is_some() {
                remote_fetch(prefix, start_key, backend.block_hash)
            } else {
                // No RPC client, use local DB
                let mut iter_args = sp_state_machine::backend::IterArgs::default();
                iter_args.prefix = self.args.prefix.as_deref();
                iter_args.start_at = self.args.start_at.as_deref();
                iter_args.stop_on_incomplete_database = true;

                let readable_db = backend.db.read();
                readable_db
                    .raw_iter(iter_args)
                    .map(|mut iter| iter.next_key(&readable_db))
                    .map(|op| op.and_then(|result| result.ok()))
                    .ok()
                    .flatten()
            }
        } else {
            // First, try to get next key from local DB
            let next_storage_key = if let Some(ref start) = self.args.start_at {
                // If we have a start_at, use next_storage_key to get the next one after it
                backend.db.read().next_storage_key(start).ok().flatten()
            } else {
                // No start_at, use raw_iter to get the first key with the prefix
                let mut iter_args = sp_state_machine::backend::IterArgs::default();
                iter_args.prefix = self.args.prefix.as_deref();
                iter_args.stop_on_incomplete_database = true;

                let readable_db = backend.db.read();
                readable_db
                    .raw_iter(iter_args)
                    .map(|mut iter| iter.next_key(&readable_db))
                    .map(|op| op.and_then(|result| result.ok()))
                    .ok()
                    .flatten()
            };

            // Filter by prefix if necessary
            let next_storage_key = next_storage_key
                .filter(|key| prefix.as_ref().map(|p| key.starts_with(&p.0)).unwrap_or(true));

            let removed_key = start_key
                .clone()
                .or(prefix.clone())
                .map(|key| backend.removed_keys.read().contains_key(&key.0))
                .unwrap_or(false);
            if next_storage_key.is_none() && !removed_key {
                let maybe_next_key = if backend.rpc().is_some() {
                    remote_fetch(prefix, start_key, backend.fork_block)
                } else {
                    None
                };
                match maybe_next_key {
                    Some(key) if !backend.removed_keys.read().contains_key(&key) => Some(key),
                    _ => None,
                }
            } else {
                next_storage_key
            }
        };

        tracing::trace!(
            target: LAZY_LOADING_LOG_TARGET,
            "next_key: (prefix: {:?}, start_at: {:?}, next_key: {:?})",
            self.args.prefix.clone().map(hex::encode),
            self.args.start_at.clone().map(hex::encode),
            maybe_next_key.clone().map(hex::encode)
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
                backend
                    .rpc()
                    .and_then(|rpc| rpc.storage_keys_paged(key, 5, start_key, block).ok())
                    .and_then(|keys| keys.first().cloned())
            };

        let prefix = self.args.prefix.clone().map(StorageKey);
        let start_key = self.args.start_at.clone().map(StorageKey);

        let maybe_next_key = if backend.before_fork {
            // If RPC client is available, fetch remotely
            if backend.rpc().is_some() {
                remote_fetch(prefix, start_key, backend.block_hash)
            } else {
                // No RPC client, use local DB
                let mut iter_args = sp_state_machine::backend::IterArgs::default();
                iter_args.prefix = self.args.prefix.as_deref();
                iter_args.start_at = self.args.start_at.as_deref();
                iter_args.stop_on_incomplete_database = true;

                let readable_db = backend.db.read();
                readable_db
                    .raw_iter(iter_args)
                    .map(|mut iter| iter.next_key(&readable_db))
                    .map(|op| op.and_then(|result| result.ok()))
                    .ok()
                    .flatten()
            }
        } else {
            // First, try to get next key from local DB
            let next_storage_key = if let Some(ref start) = self.args.start_at {
                // If we have a start_at, use next_storage_key to get the next one after it
                backend.db.read().next_storage_key(start).ok().flatten()
            } else {
                // No start_at, use raw_iter to get the first key with the prefix
                let mut iter_args = sp_state_machine::backend::IterArgs::default();
                iter_args.prefix = self.args.prefix.as_deref();
                iter_args.stop_on_incomplete_database = true;

                let readable_db = backend.db.read();
                readable_db
                    .raw_iter(iter_args)
                    .map(|mut iter| iter.next_key(&readable_db))
                    .map(|op| op.and_then(|result| result.ok()))
                    .ok()
                    .flatten()
            };

            // Filter by prefix if necessary
            let next_storage_key = next_storage_key
                .filter(|key| prefix.as_ref().map(|p| key.starts_with(&p.0)).unwrap_or(true));

            let removed_key = start_key
                .clone()
                .or(prefix.clone())
                .map(|key| backend.removed_keys.read().contains_key(&key.0))
                .unwrap_or(false);
            if next_storage_key.is_none() && !removed_key {
                let maybe_next_key = if backend.rpc().is_some() {
                    remote_fetch(prefix, start_key, backend.fork_block)
                } else {
                    None
                };
                match maybe_next_key {
                    Some(key) if !backend.removed_keys.read().contains_key(&key) => Some(key),
                    _ => None,
                }
            } else {
                next_storage_key
            }
        };

        tracing::trace!(
            target: LAZY_LOADING_LOG_TARGET,
            "next_pair: (prefix: {:?}, start_at: {:?}, next_key: {:?})",
            self.args.prefix.clone().map(hex::encode),
            self.args.start_at.clone().map(hex::encode),
            maybe_next_key.clone().map(hex::encode)
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
                maybe_value.map(|value| Ok((next_key, value)))
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
pub struct ForkedLazyBackend<Block: BlockT + DeserializeOwned> {
    pub(crate) rpc_client: Option<Arc<dyn RPCClient<Block>>>,
    pub(crate) block_hash: Option<Block::Hash>,
    pub(crate) fork_block: Option<Block::Hash>,
    pub(crate) db: Arc<parking_lot::RwLock<InMemoryBackend<HashingFor<Block>>>>,
    pub(crate) removed_keys: Arc<parking_lot::RwLock<HashMap<Vec<u8>, ()>>>,
    pub(crate) before_fork: bool,
}

impl<Block: BlockT + DeserializeOwned> ForkedLazyBackend<Block> {
    pub(crate) fn update_storage(&self, key: &[u8], value: &Option<Vec<u8>>) {
        if let Some(val) = value {
            let mut entries: HashMap<Option<ChildInfo>, StorageCollection> = Default::default();
            entries.insert(None, vec![(key.to_vec(), Some(val.clone()))]);

            self.db.write().insert(entries, StateVersion::V1);
        }
    }

    pub(crate) fn update_child_storage(
        &self,
        child_info: &ChildInfo,
        key: &[u8],
        value: &Option<Vec<u8>>,
    ) {
        if let Some(val) = value {
            let mut entries: HashMap<Option<ChildInfo>, StorageCollection> = Default::default();
            entries.insert(
                Some(child_info.clone()),
                vec![(key.to_vec(), Some(val.clone()))],
            );

            self.db.write().insert(entries, StateVersion::V1);
        }
    }

    #[inline]
    pub(crate) fn rpc(&self) -> Option<&dyn RPCClient<Block>> {
        self.rpc_client.as_deref()
    }
}

impl<Block: BlockT + DeserializeOwned> sp_state_machine::Backend<HashingFor<Block>>
    for ForkedLazyBackend<Block>
{
    type Error = <DbState<Block> as sp_state_machine::Backend<HashingFor<Block>>>::Error;
    type TrieBackendStorage = PrefixedMemoryDB<HashingFor<Block>>;
    type RawIter = RawIter<Block>;

    fn storage(&self, key: &[u8]) -> Result<Option<StorageValue>, Self::Error> {
        let remote_fetch = |block: Option<Block::Hash>| -> Option<Vec<u8>> {
            self.rpc()
                .and_then(|rpc| rpc.storage(StorageKey(key.to_vec()), block).ok())
                .flatten()
                .map(|v| v.0)
        };

        // When before_fork, try RPC first, then fall back to local DB
        if self.before_fork {
            if self.rpc().is_some() {
                return Ok(remote_fetch(self.block_hash));
            } else {
                // No RPC client, try to read from local DB
                let readable_db = self.db.read();
                return Ok(readable_db.storage(key).ok().flatten());
            }
        }

        let readable_db = self.db.read();
        let maybe_storage = readable_db.storage(key);
        let value = match maybe_storage {
            Ok(Some(data)) => Some(data),
            _ if !self.removed_keys.read().contains_key(key) => {
                let result =
                    if self.rpc().is_some() { remote_fetch(self.fork_block) } else { None };

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
        let remote_fetch = |block: Option<Block::Hash>| -> Result<
            Option<<HashingFor<Block> as sp_core::Hasher>::Out>,
            Self::Error,
        > {
            match self.rpc() {
                Some(rpc) => rpc
                    .storage_hash(StorageKey(key.to_vec()), block)
                    .map_err(|e| format!("Failed to fetch storage hash from RPC: {e:?}")),
                None => Ok(None),
            }
        };

        // When before_fork, try RPC first, then fall back to local DB
        if self.before_fork {
            if self.rpc().is_some() {
                return remote_fetch(self.block_hash);
            } else {
                // No RPC client, try to read from local DB
                return Ok(self.db.read().storage_hash(key).ok().flatten());
            }
        }

        let storage_hash = self.db.read().storage_hash(key);
        match storage_hash {
            Ok(Some(hash)) => Ok(Some(hash)),
            _ if !self.removed_keys.read().contains_key(key) => {
                if self.rpc().is_some() {
                    remote_fetch(self.fork_block)
                } else {
                    Ok(None)
                }
            }
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
        _child_info: &ChildInfo,
        _key: &[u8],
    ) -> Result<
        Option<sp_trie::MerkleValue<<HashingFor<Block> as sp_core::Hasher>::Out>>,
        Self::Error,
    > {
        unimplemented!("child_closest_merkle_value: unsupported feature for lazy loading")
    }

    fn child_storage(
        &self,
        child_info: &ChildInfo,
        key: &[u8],
    ) -> Result<Option<StorageValue>, Self::Error> {
        let remote_fetch = |block: Option<Block::Hash>| -> Option<Vec<u8>> {
            self.rpc()
                .and_then(|rpc| {
                    rpc.child_storage(child_info, StorageKey(key.to_vec()), block).ok()
                })
                .flatten()
                .map(|v| v.0)
        };

        // When before_fork, try RPC first, then fall back to local DB
        if self.before_fork {
            if self.rpc().is_some() {
                return Ok(remote_fetch(self.block_hash));
            } else {
                // No RPC client, try to read from local DB
                let readable_db = self.db.read();
                return Ok(readable_db.child_storage(child_info, key).ok().flatten());
            }
        }

        let readable_db = self.db.read();
        let maybe_storage = readable_db.child_storage(child_info, key);

        match maybe_storage {
            Ok(Some(value)) => Ok(Some(value)),
            Ok(None) => {
                if self.removed_keys.read().contains_key(key) {
                    return Ok(None);
                }

                if let Some(remote_value) = remote_fetch(self.fork_block) {
                    drop(readable_db);
                    self.update_child_storage(child_info, key, &Some(remote_value.clone()));
                    Ok(Some(remote_value))
                } else {
                    Ok(None)
                }
            }
            Err(e) => Err(e),
        }
    }

    fn child_storage_hash(
        &self,
        child_info: &ChildInfo,
        key: &[u8],
    ) -> Result<Option<<HashingFor<Block> as sp_core::Hasher>::Out>, Self::Error> {
        let remote_fetch = |block: Option<Block::Hash>| -> Option<Block::Hash> {
            self.rpc()
                .and_then(|rpc| {
                    rpc.child_storage_hash(child_info, StorageKey(key.to_vec()), block).ok()
                })
                .flatten()
        };

        // When before_fork, try RPC first, then fall back to local DB
        if self.before_fork {
            if self.rpc().is_some() {
                return Ok(remote_fetch(self.block_hash));
            } else {
                let readable_db = self.db.read();
                return Ok(readable_db.child_storage_hash(child_info, key).ok().flatten());
            }
        }

        let readable_db = self.db.read();
        let maybe_hash = readable_db.child_storage_hash(child_info, key);

        match maybe_hash {
            Ok(Some(hash)) => Ok(Some(hash)),
            Ok(None) => {
                if self.removed_keys.read().contains_key(key) {
                    return Ok(None);
                }

                Ok(remote_fetch(self.fork_block))
            }
            Err(e) => Err(e),
        }
    }

    fn next_storage_key(
        &self,
        key: &[u8],
    ) -> Result<Option<sp_state_machine::StorageKey>, Self::Error> {
        let remote_fetch = |block: Option<Block::Hash>| {
            let start_key = Some(StorageKey(key.to_vec()));
            self.rpc()
                .and_then(|rpc| rpc.storage_keys_paged(start_key.clone(), 2, None, block).ok())
                .and_then(|keys| keys.last().cloned())
        };

        // When before_fork, try RPC first, then fall back to local DB
        let maybe_next_key = if self.before_fork {
            if self.rpc().is_some() {
                remote_fetch(self.block_hash)
            } else {
                self.db.read().next_storage_key(key).ok().flatten()
            }
        } else {
            let next_storage_key = self.db.read().next_storage_key(key);
            match next_storage_key {
                Ok(Some(next_key)) => Some(next_key),
                _ if !self.removed_keys.read().contains_key(key) => {
                    if self.rpc().is_some() {
                        remote_fetch(self.fork_block)
                    } else {
                        None
                    }
                }
                // Otherwise, there's no next key
                _ => None,
            }
        }
        .filter(|next_key| next_key != key);

        tracing::trace!(
            target: LAZY_LOADING_LOG_TARGET,
            "next_storage_key: (key: {:?}, next_key: {:?})",
            hex::encode(key),
            maybe_next_key.clone().map(hex::encode)
        );

        Ok(maybe_next_key)
    }

    fn next_child_storage_key(
        &self,
        child_info: &ChildInfo,
        key: &[u8],
    ) -> Result<Option<sp_state_machine::StorageKey>, Self::Error> {
        let remote_fetch = |block: Option<Block::Hash>| {
            let start_key = Some(StorageKey(key.to_vec()));
            self.rpc()
                .and_then(|rpc| {
                    rpc.child_storage_keys_paged(child_info, None, 2, start_key.clone(), block).ok()
                })
                .and_then(|keys| keys.last().cloned())
        };

        // When before_fork, try RPC first, then fall back to local DB
        let maybe_next_key = if self.before_fork {
            if self.rpc().is_some() {
                remote_fetch(self.block_hash)
            } else {
                self.db.read().next_child_storage_key(child_info, key).ok().flatten()
            }
        } else {
            let next_child_key = self.db.read().next_child_storage_key(child_info, key);
            match next_child_key {
                Ok(Some(next_key)) => Some(next_key),
                _ if !self.removed_keys.read().contains_key(key) => {
                    if self.rpc().is_some() {
                        remote_fetch(self.fork_block)
                    } else {
                        None
                    }
                }
                // Otherwise, there's no next key
                _ => None,
            }
        }
        .filter(|next_key| next_key != key);

        tracing::trace!(
            target: LAZY_LOADING_LOG_TARGET,
            "next_child_storage_key: (child_info: {:?}, key: {:?}, next_key: {:?})",
            child_info,
            hex::encode(key),
            maybe_next_key.clone().map(hex::encode)
        );

        Ok(maybe_next_key)
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
        child_info: &ChildInfo,
        delta: impl Iterator<Item = (&'a [u8], Option<&'a [u8]>)>,
        state_version: StateVersion,
    ) -> (<HashingFor<Block> as sp_core::Hasher>::Out, bool, BackendTransaction<HashingFor<Block>>)
    where
        <HashingFor<Block> as sp_core::Hasher>::Out: Ord,
    {
        self.db.read().child_storage_root(child_info, delta, state_version)
    }

    fn raw_iter(&self, args: IterArgs<'_>) -> Result<Self::RawIter, Self::Error> {
        let clone = RawIterArgs {
            prefix: args.prefix.map(|v| v.to_vec()),
            start_at: args.start_at.map(|v| v.to_vec()),
        };

        Ok(RawIter::<Block> { args: clone, complete: false, _phantom: Default::default() })
    }

    fn register_overlay_stats(&self, stats: &sp_state_machine::StateMachineStats) {
        self.db.read().register_overlay_stats(stats)
    }

    fn usage_info(&self) -> sp_state_machine::UsageInfo {
        self.db.read().usage_info()
    }
}

impl<B: BlockT + DeserializeOwned> AsTrieBackend<HashingFor<B>> for ForkedLazyBackend<B> {
    type TrieBackendStorage = PrefixedMemoryDB<HashingFor<B>>;

    fn as_trie_backend(
        &self,
    ) -> &sp_state_machine::TrieBackend<Self::TrieBackendStorage, HashingFor<B>> {
        unimplemented!("`as_trie_backend` is not supported in lazy loading mode.")
    }
}
