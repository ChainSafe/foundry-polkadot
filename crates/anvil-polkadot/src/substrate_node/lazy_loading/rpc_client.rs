use jsonrpsee::{core::ClientError, http_client::HttpClient};
use polkadot_core_primitives::BlockNumber;
use polkadot_sdk::{
    sc_chain_spec,
    sp_api::__private::HeaderT,
    sp_core::H256,
    sp_rpc::{list::ListOrValue, number::NumberOrHex},
    sp_runtime::{self, generic::SignedBlock, traits::Block as BlockT},
    sp_state_machine,
    sp_storage::{StorageData, StorageKey},
    substrate_rpc_client,
};
use serde::de::DeserializeOwned;
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio_retry::{Retry, strategy::FixedInterval};

pub trait RPCClient: Send + Sync + std::fmt::Debug + Clone {
    fn system_chain(&self) -> Result<String, jsonrpsee::core::ClientError>;

    fn system_properties(&self) -> Result<sc_chain_spec::Properties, jsonrpsee::core::ClientError>;

    fn block<Block, Hash: Clone>(
        &self,
        hash: Option<Hash>,
    ) -> Result<Option<SignedBlock<Block>>, jsonrpsee::core::ClientError>
    where
        Block: BlockT + DeserializeOwned,
        Hash: 'static + Send + Sync + sp_runtime::Serialize + DeserializeOwned;

    fn block_hash<Block: BlockT + DeserializeOwned>(
        &self,
        block_number: Option<<Block::Header as HeaderT>::Number>,
    ) -> Result<Option<Block::Hash>, jsonrpsee::core::ClientError>;

    fn header<Block: BlockT + DeserializeOwned>(
        &self,
        hash: Option<Block::Hash>,
    ) -> Result<Option<Block::Header>, jsonrpsee::core::ClientError>;

    fn storage<
        Hash: 'static + Clone + Sync + Send + DeserializeOwned + sp_runtime::Serialize + core::fmt::Debug,
    >(
        &self,
        key: StorageKey,
        at: Option<Hash>,
    ) -> Result<Option<StorageData>, jsonrpsee::core::ClientError>;

    fn storage_hash<
        Hash: 'static + Clone + Sync + Send + DeserializeOwned + sp_runtime::Serialize + core::fmt::Debug,
    >(
        &self,
        key: StorageKey,
        at: Option<Hash>,
    ) -> Result<Option<Hash>, jsonrpsee::core::ClientError>;

    fn storage_keys_paged<
        Hash: 'static + Clone + Sync + Send + DeserializeOwned + sp_runtime::Serialize,
    >(
        &self,
        key: Option<StorageKey>,
        count: u32,
        start_key: Option<StorageKey>,
        at: Option<Hash>,
    ) -> Result<Vec<sp_state_machine::StorageKey>, ClientError>;
}

#[derive(Debug, Clone)]
pub struct RPC {
    http_client: HttpClient,
    delay_between_requests_ms: u32,
    max_retries_per_request: u32,
    counter: Arc<AtomicU64>,
}

impl RPC {
    pub fn new(
        http_client: HttpClient,
        delay_between_requests_ms: u32,
        max_retries_per_request: u32,
    ) -> Self {
        Self {
            http_client,
            delay_between_requests_ms,
            max_retries_per_request,
            counter: Default::default(),
        }
    }

    fn block_on<F, T, E>(&self, f: &dyn Fn() -> F) -> Result<T, E>
    where
        F: Future<Output = Result<T, E>>,
    {
        use tokio::runtime::Handle;

        let id = self.counter.fetch_add(1, Ordering::SeqCst);
        let start = std::time::Instant::now();

        tokio::task::block_in_place(move || {
            Handle::current().block_on(async move {
				let delay_between_requests =
					Duration::from_millis(self.delay_between_requests_ms.into());

				let start_req = std::time::Instant::now();
				log::debug!(
					target: super::LAZY_LOADING_LOG_TARGET,
					"Sending request: {}",
					id
				);

				// Explicit request delay, to avoid getting 429 errors
				let _ = tokio::time::sleep(delay_between_requests).await;

				// Retry request in case of failure
				// The maximum number of retries is specified by `self.max_retries_per_request`
				let retry_strategy = FixedInterval::new(delay_between_requests)
					.take(self.max_retries_per_request.saturating_sub(1) as usize);
				let result = Retry::spawn(retry_strategy, f).await;

				log::debug!(
					target: super::LAZY_LOADING_LOG_TARGET,
					"Completed request (id: {}, successful: {}, elapsed_time: {:?}, query_time: {:?})",
					id,
					result.is_ok(),
					start.elapsed(),
					start_req.elapsed()
				);

				result
			})
        })
    }
}

impl RPCClient for RPC {
    fn system_chain(&self) -> Result<String, jsonrpsee::core::ClientError> {
        let request = &|| {
            substrate_rpc_client::SystemApi::<H256, BlockNumber>::system_chain(&self.http_client)
        };

        self.block_on(request)
    }

    fn system_properties(&self) -> Result<sc_chain_spec::Properties, jsonrpsee::core::ClientError> {
        let request = &|| {
            substrate_rpc_client::SystemApi::<H256, BlockNumber>::system_properties(
                &self.http_client,
            )
        };

        self.block_on(request)
    }

    fn block<Block, Hash: Clone>(
        &self,
        hash: Option<Hash>,
    ) -> Result<Option<SignedBlock<Block>>, jsonrpsee::core::ClientError>
    where
        Block: BlockT + DeserializeOwned,
        Hash: 'static + Send + Sync + sp_runtime::Serialize + DeserializeOwned,
    {
        let request = &|| {
            substrate_rpc_client::ChainApi::<
				BlockNumber,
				Hash,
				Block::Header,
				SignedBlock<Block>,
			>::block(&self.http_client, hash.clone())
        };

        self.block_on(request)
    }

    fn block_hash<Block: BlockT + DeserializeOwned>(
        &self,
        block_number: Option<<Block::Header as HeaderT>::Number>,
    ) -> Result<Option<Block::Hash>, jsonrpsee::core::ClientError> {
        let request = &|| {
            substrate_rpc_client::ChainApi::<
                <Block::Header as HeaderT>::Number,
                Block::Hash,
                Block::Header,
                SignedBlock<Block>,
            >::block_hash(
                &self.http_client,
                block_number.map(|n| ListOrValue::Value(NumberOrHex::Hex(n.into()))),
            )
        };

        self.block_on(request).map(|ok| match ok {
            ListOrValue::List(v) => v.get(0).map_or(None, |some| *some),
            ListOrValue::Value(v) => v,
        })
    }

    fn header<Block: BlockT + DeserializeOwned>(
        &self,
        hash: Option<Block::Hash>,
    ) -> Result<Option<Block::Header>, jsonrpsee::core::ClientError> {
        let request = &|| {
            substrate_rpc_client::ChainApi::<
                BlockNumber,
                Block::Hash,
                Block::Header,
                SignedBlock<Block>,
            >::header(&self.http_client, hash)
        };

        self.block_on(request)
    }

    fn storage<
        Hash: 'static + Clone + Sync + Send + DeserializeOwned + sp_runtime::Serialize + core::fmt::Debug,
    >(
        &self,
        key: StorageKey,
        at: Option<Hash>,
    ) -> Result<Option<StorageData>, jsonrpsee::core::ClientError> {
        let request = &|| {
            substrate_rpc_client::StateApi::<Hash>::storage(
                &self.http_client,
                key.clone(),
                at.clone(),
            )
        };

        self.block_on(request)
    }

    fn storage_hash<
        Hash: 'static + Clone + Sync + Send + DeserializeOwned + sp_runtime::Serialize + core::fmt::Debug,
    >(
        &self,
        key: StorageKey,
        at: Option<Hash>,
    ) -> Result<Option<Hash>, jsonrpsee::core::ClientError> {
        let request = &|| {
            substrate_rpc_client::StateApi::<Hash>::storage_hash(
                &self.http_client,
                key.clone(),
                at.clone(),
            )
        };

        self.block_on(request)
    }

    fn storage_keys_paged<
        Hash: 'static + Clone + Sync + Send + DeserializeOwned + sp_runtime::Serialize,
    >(
        &self,
        key: Option<StorageKey>,
        count: u32,
        start_key: Option<StorageKey>,
        at: Option<Hash>,
    ) -> Result<Vec<sp_state_machine::StorageKey>, ClientError> {
        let request = &|| {
            substrate_rpc_client::StateApi::<Hash>::storage_keys_paged(
                &self.http_client,
                key.clone(),
                count.clone(),
                start_key.clone(),
                at.clone(),
            )
        };
        let result = self.block_on(request);

        match result {
            Ok(result) => Ok(result.iter().map(|item| item.0.clone()).collect()),
            Err(err) => Err(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonrpsee::{
        RpcModule,
        http_client::HttpClientBuilder,
        server::{ServerBuilder, ServerHandle},
        types::error::ErrorObjectOwned,
    };
    use polkadot_sdk::sp_runtime::{
        OpaqueExtrinsic,
        generic::{Block as GenericBlock, Header as GenericHeader},
        traits::BlakeTwo256,
    };
    use serde_json::json;
    use std::{net::SocketAddr, sync::atomic::AtomicUsize, time::Instant};

    // ---------------------------
    // Unit tests for `block_on`.
    // ---------------------------

    fn rpc_for_tests(delay_ms: u32, retries: u32) -> RPC {
        let client =
            HttpClientBuilder::default().build("http://127.0.0.1:8080").expect("build http client");
        RPC::new(client, delay_ms, retries)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn block_on_success_no_retry() {
        let rpc = rpc_for_tests(0, 1);
        let attempts = AtomicUsize::new(0);

        let res: Result<i32, &'static str> = rpc.block_on(&|| {
            attempts.fetch_add(1, Ordering::SeqCst);
            async { Ok(42) }
        });

        assert_eq!(res.unwrap(), 42, "should propagate Ok value");
        assert_eq!(attempts.load(Ordering::SeqCst), 1, "should run exactly one attempt");
    }

    /// Verifies that `block_on` retries on error and eventually returns Ok.
    #[tokio::test(flavor = "multi_thread")]
    async fn block_on_retries_then_succeeds() {
        // Fail the first 2 attempts, succeed on the 3rd.
        let rpc = rpc_for_tests(/* delay_ms */ 0, /* retries */ 5);
        let attempts = AtomicUsize::new(0);

        let res: Result<&'static str, &'static str> = rpc.block_on(&|| {
            let n = attempts.fetch_add(1, Ordering::SeqCst);
            async move { if n < 2 { Err("transient error") } else { Ok("ok-now") } }
        });

        assert_eq!(res.unwrap(), "ok-now");
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            3,
            "should have retried exactly until the first success"
        );
    }

    /// Verifies that `block_on` exhausts retries and returns the last error.
    #[tokio::test(flavor = "multi_thread")]
    async fn block_on_exhausts_retries_and_fails() {
        let retries = 3u32;
        let rpc = rpc_for_tests(0, retries);
        let attempts = AtomicUsize::new(0);

        let err = rpc
            .block_on(&|| {
                attempts.fetch_add(1, Ordering::SeqCst);
                async { Err::<(), &str>("always failing") }
            })
            .unwrap_err();

        assert_eq!(err, "always failing");
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            retries as usize,
            "should attempt exactly `max_retries_per_request` times"
        );
    }

    /// Verifies that the initial fixed delay (to avoid 429s) is honored before the first attempt.
    #[tokio::test(flavor = "multi_thread")]
    async fn block_on_respects_initial_delay() {
        let delay_ms = 60u32;
        let rpc = rpc_for_tests(delay_ms, /* retries */ 1);
        let attempts = AtomicUsize::new(0);

        let start = Instant::now();
        let _ = rpc.block_on(&|| {
            attempts.fetch_add(1, Ordering::SeqCst);
            async { Ok::<(), ()>(()) }
        });
        let elapsed = start.elapsed();

        assert_eq!(attempts.load(Ordering::SeqCst), 1, "should still run exactly one attempt");
        assert!(
            elapsed >= Duration::from_millis(delay_ms as u64),
            "elapsed {:?} should be >= configured initial delay {:?}ms",
            elapsed,
            delay_ms
        );
    }

    // ---------------------------------------------------------
    // Integration-style tests with an embedded JSON-RPC server.
    // ---------------------------------------------------------

    async fn start_mock_server_ok() -> (SocketAddr, ServerHandle) {
        let mut module = RpcModule::new(());

        module
            .register_method("system_chain", |_, _, _| {
                Ok::<String, ErrorObjectOwned>("Local Testnet".to_string())
            })
            .expect("register system_chain");

        module
            .register_method("system_properties", |_, _, _| {
                Ok::<serde_json::Value, ErrorObjectOwned>(json!({
                    "tokenSymbol": "UNIT",
                    "tokenDecimals": 12,
                }))
            })
            .expect("register system_properties");

        module
            .register_method("chain_getBlockHash", |_, _, _| {
                Ok::<Option<String>, ErrorObjectOwned>(Some(
                    "0x0000000000000000000000000000000000000000000000000000000000000001".into(),
                ))
            })
            .expect("register chain_getBlockHash");

        module
            .register_method("state_getStorage", |_, _, _| {
                Ok::<Option<String>, ErrorObjectOwned>(Some("0xdeadbeef".into()))
            })
            .expect("register state_getStorage");

        module
            .register_method("state_getStorageHash", |_, _, _| {
                Ok::<Option<String>, ErrorObjectOwned>(Some(
                    "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
                ))
            })
            .expect("register state_getStorageHash");

        module
            .register_method("state_getKeysPaged", |_, _, _| {
                Ok::<Vec<String>, ErrorObjectOwned>(vec!["0x1122".into(), "0xaabbcc".into()])
            })
            .expect("register state_getKeysPaged");

        let server = ServerBuilder::default().build("127.0.0.1:0").await.unwrap();
        let addr = server.local_addr().unwrap();
        let handle = server.start(module);
        (addr, handle)
    }

    // Return RPC plus the ServerHandle so tests keep the server alive.
    async fn rpc_against_mock() -> (RPC, ServerHandle) {
        let (addr, handle) = start_mock_server_ok().await;
        let url = format!("http://{}", addr);
        let client = HttpClientBuilder::default().build(&url).unwrap();
        (RPC::new(client, 0, 1), handle)
    }

    type BlockType = GenericBlock<GenericHeader<u32, BlakeTwo256>, OpaqueExtrinsic>;

    #[tokio::test(flavor = "multi_thread")]
    async fn integration_system_endpoints_ok() {
        // Keep `_server` in scope so the server isn't dropped early.
        let (rpc, _server) = rpc_against_mock().await;

        let chain = <super::RPC as super::RPCClient<BlockType>>::system_chain(&rpc)
            .expect("system_chain ok");
        assert_eq!(chain, "Local Testnet");

        let props = <super::RPC as super::RPCClient<BlockType>>::system_properties(&rpc)
            .expect("system_properties ok");
        assert_eq!(props.get("tokenSymbol").and_then(|v| v.as_str()), Some("UNIT"));
        assert_eq!(props.get("tokenDecimals").and_then(|v| v.as_u64()), Some(12));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn integration_chain_and_state_endpoints_ok() {
        let (rpc, _server) = rpc_against_mock().await;

        let hash = <super::RPC as super::RPCClient<BlockType>>::block_hash(&rpc, None)
            .expect("ok")
            .expect("some hash");
        assert_eq!(hash, polkadot_sdk::sp_core::H256::from_low_u64_be(1));

        let val = <super::RPC as super::RPCClient<BlockType>>::storage(
            &rpc,
            StorageKey(vec![0x00, 0xde, 0xad]),
            None,
        )
        .expect("ok")
        .expect("some data");
        assert_eq!(val.0, vec![0xde, 0xad, 0xbe, 0xef]);

        let h = <super::RPC as super::RPCClient<BlockType>>::storage_hash(
            &rpc,
            StorageKey(vec![0x00, 0xde, 0xad]),
            None,
        )
        .expect("ok")
        .expect("some hash");
        assert_eq!(h, polkadot_sdk::sp_core::H256::from_slice(&[0xaa; 32]));

        let keys: Vec<sp_state_machine::StorageKey> =
            <super::RPC as super::RPCClient<BlockType>>::storage_keys_paged(
                &rpc, None, 10, None, None,
            )
            .expect("ok");
        let expected: Vec<sp_state_machine::StorageKey> =
            vec![vec![0x11, 0x22], vec![0xaa, 0xbb, 0xcc]];
        assert_eq!(keys, expected);
    }
}
