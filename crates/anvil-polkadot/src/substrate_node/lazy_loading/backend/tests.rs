use super::*;
use mock_rpc::{Rpc, TestBlock, TestHeader};
use polkadot_sdk::{
    sc_client_api::{Backend as BackendT, StateBackend},
    sp_runtime::{
        OpaqueExtrinsic,
        traits::{BlakeTwo256, Header as HeaderT},
    },
    sp_state_machine::{self, StorageIterator},
    sp_storage::{StorageData, StorageKey},
};
use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicUsize, Ordering},
};

#[cfg(test)]
mod mock_rpc {
    use super::*;
    use polkadot_sdk::sp_runtime::{
        Justifications,
        generic::{Block as GenericBlock, Header, SignedBlock},
        traits::Header as HeaderT,
    };

    pub type TestHashing = BlakeTwo256;
    pub type TestHeader<N = u32> = Header<N, TestHashing>;
    pub type TestExtrinsic = OpaqueExtrinsic;
    pub type TestBlock<N = u32> = GenericBlock<TestHeader<N>, TestExtrinsic>;

    #[derive(Default, Debug)]
    pub struct Counters {
        pub storage_calls: AtomicUsize,
        pub storage_hash_calls: AtomicUsize,
        pub storage_keys_paged_calls: AtomicUsize,
        pub header_calls: AtomicUsize,
        pub block_calls: AtomicUsize,
    }

    /// Mockable RPC with interior mutability.
    #[allow(clippy::type_complexity)]
    #[derive(Clone, Default, Debug)]
    pub struct Rpc<Block: BlockT + DeserializeOwned> {
        pub counters: std::sync::Arc<Counters>,
        /// storage[(block_hash, key)] = value
        pub storage: Arc<parking_lot::RwLock<BTreeMap<(Block::Hash, StorageKey), StorageData>>>,
        /// storage_hash[(block_hash, key)] = hash
        pub storage_hashes:
            Arc<parking_lot::RwLock<BTreeMap<(Block::Hash, StorageKey), Block::Hash>>>,
        /// storage_keys_paged[(block_hash, (prefix,start))] = Vec<keys>
        pub storage_keys_pages:
            Arc<parking_lot::RwLock<BTreeMap<(Block::Hash, Vec<u8>), Vec<StorageKey>>>>,
        /// headers[hash] = header
        pub headers: Arc<parking_lot::RwLock<BTreeMap<Block::Hash, Block::Header>>>,
        /// blocks[hash] = SignedBlock
        pub blocks: Arc<parking_lot::RwLock<BTreeMap<Block::Hash, SignedBlock<Block>>>>,
    }

    impl<Block: BlockT + DeserializeOwned> Rpc<Block> {
        pub fn new() -> Self {
            Self {
                counters: std::sync::Arc::new(Counters::default()),
                storage: std::sync::Arc::new(parking_lot::RwLock::new(BTreeMap::new())),
                storage_hashes: std::sync::Arc::new(parking_lot::RwLock::new(BTreeMap::new())),
                storage_keys_pages: std::sync::Arc::new(parking_lot::RwLock::new(BTreeMap::new())),
                headers: std::sync::Arc::new(parking_lot::RwLock::new(BTreeMap::new())),
                blocks: std::sync::Arc::new(parking_lot::RwLock::new(BTreeMap::new())),
            }
        }

        pub fn put_storage(&self, at: Block::Hash, key: StorageKey, val: StorageData) {
            self.storage.write().insert((at, key), val);
        }
        pub fn put_storage_keys_page(
            &self,
            at: Block::Hash,
            prefix: Vec<u8>,
            keys: Vec<StorageKey>,
        ) {
            self.storage_keys_pages.write().insert((at, prefix), keys);
        }
        pub fn put_header(&self, h: Block::Header) {
            self.headers.write().insert(h.hash(), h);
        }
        pub fn put_block(&self, block: Block, just: Option<Justifications>) {
            let full = SignedBlock { block, justifications: just };
            self.blocks.write().insert(full.block.header().hash(), full);
        }
    }

    impl<Block: BlockT + DeserializeOwned> RPCClient<Block> for Rpc<Block> {
        fn storage(
            &self,
            key: StorageKey,
            at: Option<Block::Hash>,
        ) -> Result<Option<StorageData>, jsonrpsee::core::ClientError> {
            self.counters.storage_calls.fetch_add(1, Ordering::Relaxed);
            let map = self.storage.read();
            Ok(map.get(&(at.unwrap_or_default(), key)).cloned())
        }

        fn storage_hash(
            &self,
            key: StorageKey,
            at: Option<Block::Hash>,
        ) -> Result<Option<Block::Hash>, jsonrpsee::core::ClientError> {
            self.counters.storage_hash_calls.fetch_add(1, Ordering::Relaxed);
            let bh = at.unwrap_or_default();
            let map = self.storage_hashes.read();
            Ok(map.get(&(bh, key)).copied())
        }

        fn storage_keys_paged(
            &self,
            key: Option<StorageKey>,
            count: u32,
            start_key: Option<StorageKey>,
            at: Option<Block::Hash>,
        ) -> Result<Vec<sp_state_machine::StorageKey>, jsonrpsee::core::ClientError> {
            self.counters.storage_keys_paged_calls.fetch_add(1, Ordering::Relaxed);

            use std::cmp::min;

            let bh = at.unwrap_or_default();
            let prefix = key.map(|k| k.0).unwrap_or_default();
            let start = start_key.map(|k| k.0);

            let map = self.storage_keys_pages.read();
            let mut all = map.get(&(bh, prefix.clone())).cloned().unwrap_or_default();

            all.sort_by(|a, b| a.0.cmp(&b.0));

            let mut filtered: Vec<StorageKey> =
                all.into_iter().filter(|k| k.0.starts_with(&prefix)).collect();

            if let Some(s) = start {
                if let Some(pos) = filtered.iter().position(|k| k.0 == s) {
                    filtered = filtered.into_iter().skip(pos + 1).collect();
                } else {
                    filtered.retain(|k| k.0 > s);
                }
            }

            let take = min(filtered.len(), count as usize);
            Ok(filtered.into_iter().take(take).map(|k| k.0).collect())
        }

        fn header(
            &self,
            at: Option<Block::Hash>,
        ) -> Result<Option<Block::Header>, jsonrpsee::core::ClientError> {
            self.counters.header_calls.fetch_add(1, Ordering::Relaxed);
            let key = at.unwrap_or_default();
            let raw = self.headers.read().get(&key).cloned();
            Ok(raw)
        }

        fn block(
            &self,
            hash: Option<Block::Hash>,
        ) -> Result<
            Option<polkadot_sdk::sp_runtime::generic::SignedBlock<Block>>,
            jsonrpsee::core::ClientError,
        > {
            self.counters.block_calls.fetch_add(1, Ordering::Relaxed);
            let key = hash.unwrap_or_default();
            let raw = self.blocks.read().get(&key).cloned();
            Ok(raw)
        }

        fn block_hash(
            &self,
            _num: Option<NumberFor<Block>>,
        ) -> Result<Option<Block::Hash>, jsonrpsee::core::ClientError> {
            todo!()
        }

        fn system_chain(&self) -> Result<String, jsonrpsee::core::ClientError> {
            todo!()
        }

        fn system_properties(
            &self,
        ) -> Result<polkadot_sdk::sc_chain_spec::Properties, jsonrpsee::core::ClientError> {
            todo!()
        }
    }
}

type N = u32;
type TestBlockT = TestBlock<N>;

fn make_header(number: N, parent: <TestBlock as BlockT>::Hash) -> TestHeader<N> {
    TestHeader::new(number, Default::default(), Default::default(), parent, Default::default())
}

fn make_block(
    number: N,
    parent: <TestBlock as BlockT>::Hash,
    xts: Vec<OpaqueExtrinsic>,
) -> TestBlock<N> {
    let header = make_header(number, parent);
    TestBlock::new(header, xts)
}

fn checkpoint(n: N) -> TestHeader<N> {
    make_header(n, Default::default())
}

#[test]
fn before_fork_reads_remote_only() {
    let rpc = std::sync::Arc::new(Rpc::new());
    // fork checkpoint at #100
    let cp = checkpoint(100);
    let backend = Backend::<TestBlockT>::new(Some(rpc.clone()), cp);

    // state_at(Default::default()) => before_fork=true
    let state = backend.state_at(Default::default(), TrieCacheContext::Trusted).unwrap();

    let key = b":foo".to_vec();
    // prepare remote value at "block_hash = Default::default()"
    let at = Default::default();
    rpc.put_storage(at, StorageKey(key.clone()), StorageData(b"bar".to_vec()));

    // read storage
    let v1 = state.storage(&key).unwrap();
    assert_eq!(v1, Some(b"bar".to_vec()));

    // not cached in DB: second read still goes to RPC
    let v2 = state.storage(&key).unwrap();
    assert_eq!(v2, Some(b"bar".to_vec()));
    assert!(rpc.counters.storage_calls.load(Ordering::Relaxed) >= 2);
}

#[test]
fn after_fork_first_fetch_caches_subsequent_hits_local() {
    let rpc = std::sync::Arc::new(Rpc::new());
    let cp = checkpoint(10);
    let backend = Backend::<TestBlockT>::new(Some(rpc.clone()), cp.clone());

    // Build a block #11 > checkpoint (#10), with parent #10
    let parent = cp.hash();
    let b11 = make_block(11, parent, vec![]);
    let h11 = b11.header.hash();

    rpc.put_header(b11.header.clone());
    rpc.put_block(b11, None);

    // remote storage at fork block (checkpoint hash)
    let fork_hash = cp.hash();
    let key = b":k".to_vec();
    rpc.put_storage(fork_hash, StorageKey(key.clone()), StorageData(b"v".to_vec()));

    // Grab state_at(#11): after_fork=false; local DB empty
    let state = backend.state_at(h11, TrieCacheContext::Trusted).unwrap();

    // First read fetches remote and caches
    let v1 = state.storage(&key).unwrap();
    assert_eq!(v1, Some(b"v".to_vec()));

    // Mutate RPC to detect second call (remove remote value)
    // If second read still tries RPC, it would return None; but it should come from cache.
    // So we do not change the mock; instead, assert RPC call count increases only once.
    let calls_before = rpc.counters.storage_calls.load(Ordering::Relaxed);
    let _ = state.storage(&key).unwrap();
    let calls_after = rpc.counters.storage_calls.load(Ordering::Relaxed);
    assert_eq!(calls_before, calls_after, "second hit should be served from cache");
}

#[test]
fn removed_keys_prevents_remote_fetch() {
    let rpc = std::sync::Arc::new(Rpc::new());
    let cp = checkpoint(5);
    let backend = Backend::<TestBlockT>::new(Some(rpc.clone()), cp.clone());

    // make block #6
    let b6 = make_block(6, cp.hash(), vec![]);
    rpc.put_header(b6.header.clone());
    rpc.put_block(b6.clone(), None);
    let state = backend.state_at(b6.header.hash(), TrieCacheContext::Trusted).unwrap();

    // mark key as removed
    let key = b":dead".to_vec();
    state.removed_keys.write().insert(key.clone(), ());

    // Even if remote has a value, backend must not fetch it
    rpc.put_storage(cp.hash(), StorageKey(key.clone()), StorageData(b"ghost".to_vec()));
    let calls_before = rpc.counters.storage_calls.load(Ordering::Relaxed);
    let v = state.storage(&key).unwrap();
    let calls_after = rpc.counters.storage_calls.load(Ordering::Relaxed);

    assert!(v.is_none());
    assert_eq!(calls_before, calls_after, "should not call RPC for removed keys");
}

#[test]
fn raw_iter_merges_local_then_remote() {
    let rpc = std::sync::Arc::new(Rpc::new());
    let cp = checkpoint(7);
    let backend = Backend::<TestBlockT>::new(Some(rpc.clone()), cp.clone());

    // block #8
    let b8 = make_block(8, cp.hash(), vec![]);
    rpc.put_header(b8.header.clone());
    rpc.put_block(b8.clone(), None);
    let state = backend.state_at(b8.header.hash(), TrieCacheContext::Trusted).unwrap();

    // Preload local DB with key "a1"
    state.update_storage(b"a1", &Some(b"v1".to_vec()));

    // Ensure storage_root is computed to make the key visible to raw_iter
    let _ = state
        .db
        .write()
        .storage_root(vec![(b"a1".as_ref(), Some(b"v1".as_ref()))].into_iter(), StateVersion::V1);

    // Remote has only "a2" under same prefix at fork block (not "a1")
    rpc.put_storage_keys_page(cp.hash(), b"a".to_vec(), vec![StorageKey(b"a2".to_vec())]);
    rpc.put_storage(cp.hash(), StorageKey(b"a2".to_vec()), StorageData(b"v2".to_vec()));

    let mut args = polkadot_sdk::sp_state_machine::IterArgs::default();
    args.prefix = Some(&b"a"[..]);
    let mut it = state.raw_iter(args).unwrap();

    // next_pair should return ("a1","v1") from local
    let p1 = it.next_pair(&state).unwrap().unwrap();
    assert_eq!(p1.0, b"a1".to_vec());
    assert_eq!(p1.1, b"v1".to_vec());

    // next_pair should now bring remote ("a2","v2")
    let p2 = it.next_pair(&state).unwrap().unwrap();
    assert_eq!(p2.0, b"a2".to_vec());
    assert_eq!(p2.1, b"v2".to_vec());

    // done
    assert!(it.next_pair(&state).is_none());
    assert!(it.was_complete());
}

#[test]
fn blockchain_header_and_number_are_cached() {
    let rpc = std::sync::Arc::new(Rpc::new());
    let cp = checkpoint(3);
    let backend = Backend::<TestBlockT>::new(Some(rpc.clone()), cp.clone());
    let chain = backend.blockchain();

    // prepare one block w/ extrinsics
    let xts: Vec<OpaqueExtrinsic> = vec![];
    let b4 = make_block(4, cp.hash(), xts);
    let h4 = b4.header().hash();
    rpc.put_block(b4, None);

    // first header() fetches RPC and caches as Full
    let h = chain.header(h4).unwrap().unwrap();
    assert_eq!(h.hash(), h4);

    // number() should now return from cache (no extra RPC needed)
    let calls_before = rpc.counters.block_calls.load(Ordering::Relaxed);
    let number = chain.number(h4).unwrap().unwrap();
    let calls_after = rpc.counters.block_calls.load(Ordering::Relaxed);

    assert_eq!(number, 4);
    assert_eq!(calls_before, calls_after, "number() should be served from cache after header()");
}
