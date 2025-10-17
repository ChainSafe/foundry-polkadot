use polkadot_sdk::{
	sc_client_api::{self, execution_extensions::ExecutionExtensions},
	sc_executor::{self, RuntimeVersion, RuntimeVersionOf},
	sp_api::ProofRecorder,
	sp_core::{self, traits::{CallContext, Externalities}},
	sp_runtime::traits::{Block as BlockT, HashingFor},
	sp_state_machine::{OverlayedChanges, StorageProof},
	sp_blockchain,
	sp_externalities,
};

use std::cell::RefCell;
use std::marker::PhantomData;

/// Call executor that executes methods locally, querying all required
/// data from local backend.
#[derive(Clone)]
pub struct LazyLoadingCallExecutor<Block, Executor> {
	inner: Executor,
	_phantom_data: PhantomData<Block>,
}

impl<Block: BlockT, Executor> LazyLoadingCallExecutor<Block, Executor>
where
	Executor: sc_client_api::CallExecutor<Block> + Clone + 'static,
{
	/// Creates new instance of local call executor.
	pub fn new(executor: Executor) -> sp_blockchain::Result<Self> {
		Ok(LazyLoadingCallExecutor {
			inner: executor,
			_phantom_data: Default::default(),
		})
	}
}

impl<Block, Executor> sc_client_api::CallExecutor<Block>
	for LazyLoadingCallExecutor<Block, Executor>
where
	Executor: sc_client_api::CallExecutor<Block>,
	Block: BlockT,
{
	type Error = Executor::Error;

	type Backend = Executor::Backend;

	fn execution_extensions(&self) -> &ExecutionExtensions<Block> {
		&self.inner.execution_extensions()
	}

	fn call(
		&self,
		at_hash: Block::Hash,
		method: &str,
		call_data: &[u8],
		context: CallContext,
	) -> sp_blockchain::Result<Vec<u8>> {
		self.inner.call(at_hash, method, call_data, context)
	}

	fn contextual_call(
		&self,
		at_hash: Block::Hash,
		method: &str,
		call_data: &[u8],
		changes: &RefCell<OverlayedChanges<HashingFor<Block>>>,
		// not used in lazy loading
		_recorder: &Option<ProofRecorder<Block>>,
		call_context: CallContext,
		extensions: &RefCell<sp_externalities::Extensions>,
	) -> Result<Vec<u8>, sp_blockchain::Error> {
		self.inner.contextual_call(
			at_hash,
			method,
			call_data,
			changes,
			&None,
			call_context,
			extensions,
		)
	}

	fn runtime_version(&self, at_hash: Block::Hash) -> sp_blockchain::Result<RuntimeVersion> {
		sc_client_api::CallExecutor::runtime_version(&self.inner, at_hash)
	}

	fn prove_execution(
		&self,
		at_hash: Block::Hash,
		method: &str,
		call_data: &[u8],
	) -> sp_blockchain::Result<(Vec<u8>, StorageProof)> {
		self.inner.prove_execution(at_hash, method, call_data)
	}
}

impl<Block, Executor> RuntimeVersionOf for LazyLoadingCallExecutor<Block, Executor>
where
	Executor: RuntimeVersionOf,
	Block: BlockT,
{
	fn runtime_version(
		&self,
		ext: &mut dyn Externalities,
		runtime_code: &sp_core::traits::RuntimeCode<'_>,
	) -> Result<RuntimeVersion, sc_executor::error::Error> {
		self.inner.runtime_version(ext, runtime_code)
	}
}
