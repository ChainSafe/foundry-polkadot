use polkadot_sdk::{
	substrate_rpc_client,
	sp_state_machine,
	sp_runtime,
	sp_storage::{StorageData, StorageKey},
};
use std::{
	time::Duration, 
	sync::{
		Arc, 
		atomic::{AtomicU64, Ordering}
	}
};
use serde::de::DeserializeOwned;
use jsonrpsee::{http_client::HttpClient, core::ClientError};
use tokio_retry::strategy::FixedInterval;
use tokio_retry::Retry;

trait RPCClient  {
	fn storage<
	Hash: 'static + Clone + Sync + Send + DeserializeOwned + sp_runtime::Serialize + core::fmt::Debug
	>(
		&self,
		key: StorageKey,
		at: Option<Hash>,
	) -> Result<Option<StorageData>, jsonrpsee::core::ClientError>;

	fn storage_keys_paged<
	Hash: 'static + Clone + Sync + Send + DeserializeOwned + sp_runtime::Serialize + core::fmt::Debug
	>(
		&self,
		key: Option<StorageKey>,
		count: u32,
		start_key: Option<StorageKey>,
		at: Option<Hash>,
	) -> Result<Vec<sp_state_machine::StorageKey>, ClientError>;
}

#[derive(Debug, Clone)]
pub struct LazyLoadingRPC {
	http_client: HttpClient,
	delay_between_requests_ms: u32,
	max_retries_per_request: u32,
	counter: Arc<AtomicU64>,
}

impl LazyLoadingRPC {
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

impl RPCClient for LazyLoadingRPC {
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
    use jsonrpsee::http_client::HttpClientBuilder;
    use std::sync::atomic::{AtomicUsize};
    use std::time::{Instant};

    fn rpc_for_tests(delay_ms: u32, retries: u32) -> RPC {
        let client = HttpClientBuilder::default()
            .build("http://127.0.0.1:8080")
            .expect("build http client");
        LazyLoadingRPC::new(client, delay_ms, retries)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn block_on_success_no_retry() {
        let rpc = rpc_for_tests( 0,  1);
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
        // fail first 2 attempts, succeed on 3rd.
        let rpc = rpc_for_tests(/*delay_ms*/ 0, /*retries*/ 5);
        let attempts = AtomicUsize::new(0);

        let res: Result<&'static str, &'static str> = rpc.block_on(&|| {
            let n = attempts.fetch_add(1, Ordering::SeqCst);
            async move {
                if n < 2 {
                    Err("transient error")
                } else {
                    Ok("ok-now")
                }
            }
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
        let rpc = rpc_for_tests( 0, retries);
        let attempts = AtomicUsize::new(0);

        let err = rpc
            .block_on(&|| {
                attempts.fetch_add(1, Ordering::SeqCst);
                async { Err::<(), &str>("always failing") }
            })
            .unwrap_err();

        // Assert
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
        let rpc = rpc_for_tests(delay_ms, /*retries*/ 1);
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
}
