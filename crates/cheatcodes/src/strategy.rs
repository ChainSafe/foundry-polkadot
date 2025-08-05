use std::{any::Any, fmt::Debug, sync::Arc};

use alloy_primitives::{Address, Bytes, TxKind, B256, U256};
use alloy_rpc_types::{TransactionInput, TransactionRequest};
use revm::{
    interpreter::{CallInputs, Interpreter},
    primitives::SignedAuthorization,
};

use crate::{
    inspector::{check_if_fixed_gas_limit, CommonCreateInput, Ecx, InnerEcx},
    script::Broadcast,
    BroadcastableTransaction, BroadcastableTransactions, CheatcodesExecutor, CheatsConfig,
    CheatsCtxt, DynCheatcode, Result,
};

/// Context for [CheatcodeInspectorStrategy].
pub trait CheatcodeInspectorStrategyContext: Debug + Send + Sync + Any {
    /// Clone the strategy context.
    fn new_cloned(&self) -> Box<dyn CheatcodeInspectorStrategyContext>;
    /// Alias as immutable reference of [Any].
    fn as_any_ref(&self) -> &dyn Any;
    /// Alias as mutable reference of [Any].
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

impl Clone for Box<dyn CheatcodeInspectorStrategyContext> {
    fn clone(&self) -> Self {
        self.new_cloned()
    }
}

/// Default strategy context object.
impl CheatcodeInspectorStrategyContext for () {
    fn new_cloned(&self) -> Box<dyn CheatcodeInspectorStrategyContext> {
        Box::new(())
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn as_any_ref(&self) -> &dyn Any {
        self
    }
}

/// PVM-specific strategy context.
#[derive(Debug, Default, Clone)]
pub struct PvmCheatcodeInspectorStrategyContext {
    /// Whether we're using PVM instead of EVM
    pub using_pvm: bool,

    /// PVM-specific environment data
    pub pvm_env: Option<PvmEnvironment>,

    /// Any PVM-specific state that needs to be tracked
    pub pvm_state: PvmState,

    /// Track if we need to switch to PVM mode
    pub pvm_startup_migration: PvmStartupMigration,

    /// Addresses that should be executed in EVM mode even when in PVM mode
    pub skip_pvm_addresses: std::collections::HashSet<Address>,

    /// Whether to record the next create address for skip_pvm_addresses
    pub record_next_create_address: bool,
}

/// PVM startup migration status
#[derive(Debug, Default, Clone)]
pub enum PvmStartupMigration {
    #[default]
    Defer,
    Allowed,
    Done,
}

impl PvmStartupMigration {
    pub fn allow(&mut self) {
        *self = Self::Allowed;
    }

    pub fn done(&mut self) {
        *self = Self::Done;
    }

    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allowed)
    }
}

/// PVM environment configuration
#[derive(Debug, Default, Clone)]
pub struct PvmEnvironment {
    /// Memory configuration for PVM
    pub memory_config: PvmMemoryConfig,

    /// Debug information flag
    pub debug_information: bool,
}

/// PVM memory configuration
#[derive(Debug, Default, Clone)]
pub struct PvmMemoryConfig {
    /// Heap size for PVM
    pub heap_size: u32,

    /// Stack size for PVM
    pub stack_size: u32,
}

/// PVM-specific state tracking
#[derive(Debug, Default, Clone)]
pub struct PvmState {
    /// Any PVM-specific state that needs to be tracked
    pub custom_state: std::collections::HashMap<String, String>,

    /// PVM-specific storage mappings
    pub pvm_storage: std::collections::HashMap<Address, std::collections::HashMap<U256, U256>>,

    /// PVM-specific account info
    pub pvm_accounts: std::collections::HashMap<Address, PvmAccountInfo>,
}

/// PVM account information
#[derive(Debug, Default, Clone)]
pub struct PvmAccountInfo {
    pub balance: U256,
    pub nonce: u64,
    pub code_hash: B256,
    pub code: Option<Bytes>,
}

impl PvmCheatcodeInspectorStrategyContext {
    pub fn new(pvm_env: Option<PvmEnvironment>) -> Self {
        Self {
            using_pvm: false, // Start in EVM mode by default
            pvm_env,
            pvm_state: PvmState::default(),
            pvm_startup_migration: PvmStartupMigration::Defer,
            skip_pvm_addresses: std::collections::HashSet::new(),
            record_next_create_address: false,
        }
    }

    /// Switch to PVM mode
    pub fn switch_to_pvm(&mut self) {
        if !self.using_pvm {
            tracing::info!("switching to PVM mode");
            self.using_pvm = true;
        }
    }

    /// Switch to EVM mode
    pub fn switch_to_evm(&mut self) {
        if self.using_pvm {
            tracing::info!("switching to EVM mode");
            self.using_pvm = false;
        }
    }

    /// Check if an address should be executed in EVM mode
    pub fn should_skip_pvm(&self, address: Address) -> bool {
        self.skip_pvm_addresses.contains(&address)
    }

    /// Add an address to skip PVM execution
    pub fn add_skip_pvm_address(&mut self, address: Address) {
        self.skip_pvm_addresses.insert(address);
    }
}

impl CheatcodeInspectorStrategyContext for PvmCheatcodeInspectorStrategyContext {
    fn new_cloned(&self) -> Box<dyn CheatcodeInspectorStrategyContext> {
        Box::new(self.clone())
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn as_any_ref(&self) -> &dyn Any {
        self
    }
}

/// Stateless strategy runner for [CheatcodeInspectorStrategy].
pub trait CheatcodeInspectorStrategyRunner: Debug + Send + Sync {
    /// Apply cheatcodes.
    fn apply_full(
        &self,
        cheatcode: &dyn DynCheatcode,
        ccx: &mut CheatsCtxt,
        executor: &mut dyn CheatcodesExecutor,
    ) -> Result {
        cheatcode.dyn_apply(ccx, executor)
    }

    /// Called when the main test or script contract is deployed.
    fn base_contract_deployed(&self, _ctx: &mut dyn CheatcodeInspectorStrategyContext) {}

    /// Record broadcastable transaction during CREATE.
    fn record_broadcastable_create_transactions(
        &self,
        _ctx: &mut dyn CheatcodeInspectorStrategyContext,
        config: Arc<CheatsConfig>,
        input: &dyn CommonCreateInput,
        ecx_inner: InnerEcx,
        broadcast: &Broadcast,
        broadcastable_transactions: &mut BroadcastableTransactions,
    );

    /// Record broadcastable transaction during CALL.
    #[allow(clippy::too_many_arguments)]
    fn record_broadcastable_call_transactions(
        &self,
        _ctx: &mut dyn CheatcodeInspectorStrategyContext,
        config: Arc<CheatsConfig>,
        input: &CallInputs,
        ecx_inner: InnerEcx,
        broadcast: &Broadcast,
        broadcastable_transactions: &mut BroadcastableTransactions,
        active_delegation: &mut Option<SignedAuthorization>,
    );

    /// Hook for pre initialize_interp.
    fn pre_initialize_interp(
        &self,
        _ctx: &mut dyn CheatcodeInspectorStrategyContext,
        _interpreter: &mut Interpreter,
        _ecx: Ecx,
    ) {
    }

    /// Hook for post initialize_interp.
    fn post_initialize_interp(
        &self,
        _ctx: &mut dyn CheatcodeInspectorStrategyContext,
        _interpreter: &mut Interpreter,
        _ecx: Ecx,
    ) {
    }

    /// Hook for pre step_end.
    ///
    /// Used to override opcode behaviors. Returns true if handled.
    fn pre_step_end(
        &self,
        _ctx: &mut dyn CheatcodeInspectorStrategyContext,
        _interpreter: &mut Interpreter,
        _ecx: Ecx,
    ) -> bool {
        false
    }
}

/// PVM-specific strategy extensions
pub trait CheatcodeInspectorStrategyExt: CheatcodeInspectorStrategyRunner {
    /// Switch to PVM mode
    fn pvm_switch_to_pvm(&self, _ctx: &mut dyn CheatcodeInspectorStrategyContext, _ecx: Ecx) {}

    /// Switch to EVM mode  
    fn pvm_switch_to_evm(&self, _ctx: &mut dyn CheatcodeInspectorStrategyContext, _ecx: Ecx) {}

    /// Handle PVM-specific operations
    fn pvm_handle_operation(
        &self,
        _ctx: &mut dyn CheatcodeInspectorStrategyContext,
        _ecx: Ecx,
    ) -> bool {
        false
    }
}

/// Implements [CheatcodeInspectorStrategyRunner] for EVM.
#[derive(Debug, Default, Clone)]
pub struct EvmCheatcodeInspectorStrategyRunner;

impl CheatcodeInspectorStrategyRunner for EvmCheatcodeInspectorStrategyRunner {
    fn record_broadcastable_create_transactions(
        &self,
        _ctx: &mut dyn CheatcodeInspectorStrategyContext,
        _config: Arc<CheatsConfig>,
        input: &dyn CommonCreateInput,
        ecx_inner: InnerEcx,
        broadcast: &Broadcast,
        broadcastable_transactions: &mut BroadcastableTransactions,
    ) {
        let is_fixed_gas_limit = check_if_fixed_gas_limit(ecx_inner, input.gas_limit());

        let account = &ecx_inner.journaled_state.state()[&broadcast.new_origin];
        broadcastable_transactions.push_back(BroadcastableTransaction {
            rpc: ecx_inner.db.active_fork_url(),
            transaction: TransactionRequest {
                from: Some(broadcast.new_origin),
                to: None,
                value: Some(input.value()),
                input: TransactionInput::new(input.init_code()),
                nonce: Some(account.info.nonce),
                gas: if is_fixed_gas_limit { Some(input.gas_limit()) } else { None },
                ..Default::default()
            }
            .into(),
        });
    }

    fn record_broadcastable_call_transactions(
        &self,
        _ctx: &mut dyn CheatcodeInspectorStrategyContext,
        _config: Arc<CheatsConfig>,
        call: &CallInputs,
        ecx_inner: InnerEcx,
        broadcast: &Broadcast,
        broadcastable_transactions: &mut BroadcastableTransactions,
        active_delegation: &mut Option<SignedAuthorization>,
    ) {
        let is_fixed_gas_limit = check_if_fixed_gas_limit(ecx_inner, call.gas_limit);

        let account = ecx_inner.journaled_state.state().get_mut(&broadcast.new_origin).unwrap();

        let mut tx_req = TransactionRequest {
            from: Some(broadcast.new_origin),
            to: Some(TxKind::from(Some(call.target_address))),
            value: call.transfer_value(),
            input: TransactionInput::new(call.input.clone()),
            nonce: Some(account.info.nonce),
            chain_id: Some(ecx_inner.env.cfg.chain_id),
            gas: if is_fixed_gas_limit { Some(call.gas_limit) } else { None },
            ..Default::default()
        };

        if let Some(auth_list) = active_delegation.take() {
            tx_req.authorization_list = Some(vec![auth_list]);
        } else {
            tx_req.authorization_list = None;
        }

        broadcastable_transactions.push_back(BroadcastableTransaction {
            rpc: ecx_inner.db.active_fork_url(),
            transaction: tx_req.into(),
        });
        debug!(target: "cheatcodes", tx=?broadcastable_transactions.back().unwrap(), "broadcastable call");
    }
}

/// Implements [CheatcodeInspectorStrategyRunner] for PVM.
#[derive(Debug, Default, Clone)]
pub struct PvmCheatcodeInspectorStrategyRunner;

impl CheatcodeInspectorStrategyRunner for PvmCheatcodeInspectorStrategyRunner {
    fn base_contract_deployed(&self, ctx: &mut dyn CheatcodeInspectorStrategyContext) {
        let ctx = get_pvm_context(ctx);
        debug!("allowing startup PVM migration");
        ctx.pvm_startup_migration.allow();
    }

    fn record_broadcastable_create_transactions(
        &self,
        ctx: &mut dyn CheatcodeInspectorStrategyContext,
        config: Arc<CheatsConfig>,
        input: &dyn CommonCreateInput,
        ecx_inner: InnerEcx,
        broadcast: &Broadcast,
        broadcastable_transactions: &mut BroadcastableTransactions,
    ) {
        let ctx_pvm = get_pvm_context(ctx);

        if !ctx_pvm.using_pvm {
            // Fall back to EVM implementation
            return EvmCheatcodeInspectorStrategyRunner.record_broadcastable_create_transactions(
                ctx,
                config,
                input,
                ecx_inner,
                broadcast,
                broadcastable_transactions,
            );
        }

        // PVM-specific CREATE transaction recording
        // For now, use EVM implementation but with PVM-specific metadata
        let is_fixed_gas_limit = check_if_fixed_gas_limit(ecx_inner, input.gas_limit());

        let account = &ecx_inner.journaled_state.state()[&broadcast.new_origin];
        let tx = TransactionRequest {
            from: Some(broadcast.new_origin),
            to: None,
            value: Some(input.value()),
            input: TransactionInput::new(input.init_code()),
            nonce: Some(account.info.nonce),
            gas: if is_fixed_gas_limit { Some(input.gas_limit()) } else { None },
            ..Default::default()
        };

        // Add PVM-specific metadata
        // TODO: Add PVM-specific transaction fields when available

        broadcastable_transactions.push_back(BroadcastableTransaction {
            rpc: ecx_inner.db.active_fork_url(),
            transaction: tx.into(),
        });

        debug!(target: "cheatcodes", "PVM broadcastable create transaction recorded");
    }

    fn record_broadcastable_call_transactions(
        &self,
        ctx: &mut dyn CheatcodeInspectorStrategyContext,
        config: Arc<CheatsConfig>,
        call: &CallInputs,
        ecx_inner: InnerEcx,
        broadcast: &Broadcast,
        broadcastable_transactions: &mut BroadcastableTransactions,
        active_delegation: &mut Option<SignedAuthorization>,
    ) {
        let ctx_pvm = get_pvm_context(ctx);

        if !ctx_pvm.using_pvm {
            // Fall back to EVM implementation
            return EvmCheatcodeInspectorStrategyRunner.record_broadcastable_call_transactions(
                ctx,
                config,
                call,
                ecx_inner,
                broadcast,
                broadcastable_transactions,
                active_delegation,
            );
        }

        // PVM-specific CALL transaction recording
        let is_fixed_gas_limit = check_if_fixed_gas_limit(ecx_inner, call.gas_limit);

        let account = ecx_inner.journaled_state.state().get_mut(&broadcast.new_origin).unwrap();

        let mut tx_req = TransactionRequest {
            from: Some(broadcast.new_origin),
            to: Some(TxKind::from(Some(call.target_address))),
            value: call.transfer_value(),
            input: TransactionInput::new(call.input.clone()),
            nonce: Some(account.info.nonce),
            chain_id: Some(ecx_inner.env.cfg.chain_id),
            gas: if is_fixed_gas_limit { Some(call.gas_limit) } else { None },
            ..Default::default()
        };

        if let Some(auth_list) = active_delegation.take() {
            tx_req.authorization_list = Some(vec![auth_list]);
        } else {
            tx_req.authorization_list = None;
        }

        // Add PVM-specific metadata
        // TODO: Add PVM-specific transaction fields when available

        broadcastable_transactions.push_back(BroadcastableTransaction {
            rpc: ecx_inner.db.active_fork_url(),
            transaction: tx_req.into(),
        });

        debug!(target: "cheatcodes", "PVM broadcastable call transaction recorded");
    }

    fn post_initialize_interp(
        &self,
        ctx: &mut dyn CheatcodeInspectorStrategyContext,
        _interpreter: &mut Interpreter,
        ecx: Ecx,
    ) {
        let ctx = get_pvm_context(ctx);

        if ctx.pvm_startup_migration.is_allowed() && !ctx.using_pvm {
            self.select_pvm(ctx, ecx);
            ctx.pvm_startup_migration.done();
            debug!("startup PVM migration completed");
        }
    }

    /// Returns true if handled.
    fn pre_step_end(
        &self,
        ctx: &mut dyn CheatcodeInspectorStrategyContext,
        _interpreter: &mut Interpreter,
        _ecx: Ecx,
    ) -> bool {
        let ctx = get_pvm_context(ctx);

        if !ctx.using_pvm {
            return false;
        }

        // PVM-specific opcode handling would go here
        // For now, just return false to let EVM handle it
        false
    }
}

impl PvmCheatcodeInspectorStrategyRunner {
    /// Select PVM mode for execution
    pub fn select_pvm(&self, ctx: &mut PvmCheatcodeInspectorStrategyContext, _ecx: Ecx) {
        if !ctx.using_pvm {
            tracing::info!("switching to PVM mode");
            ctx.using_pvm = true;

            // TODO: Implement PVM-specific state migration
            // This would involve:
            // - Converting EVM state to PVM state
            // - Setting up PVM-specific storage mappings
            // - Initializing PVM account information
        }
    }

    /// Select EVM mode for execution
    pub fn select_evm(&self, ctx: &mut PvmCheatcodeInspectorStrategyContext, _ecx: Ecx) {
        if ctx.using_pvm {
            tracing::info!("switching to EVM mode");
            ctx.using_pvm = false;

            // TODO: Implement EVM state migration
            // This would involve:
            // - Converting PVM state back to EVM state
            // - Restoring EVM storage mappings
            // - Updating EVM account information
        }
    }
}

impl CheatcodeInspectorStrategyExt for PvmCheatcodeInspectorStrategyRunner {
    fn pvm_switch_to_pvm(&self, ctx: &mut dyn CheatcodeInspectorStrategyContext, ecx: Ecx) {
        let ctx_pvm = get_pvm_context(ctx);
        self.select_pvm(ctx_pvm, ecx);
    }

    fn pvm_switch_to_evm(&self, ctx: &mut dyn CheatcodeInspectorStrategyContext, ecx: Ecx) {
        let ctx_pvm = get_pvm_context(ctx);
        self.select_evm(ctx_pvm, ecx);
    }

    fn pvm_handle_operation(
        &self,
        ctx: &mut dyn CheatcodeInspectorStrategyContext,
        _ecx: Ecx,
    ) -> bool {
        let ctx_pvm = get_pvm_context(ctx);

        if !ctx_pvm.using_pvm {
            return false;
        }

        // TODO: Implement PVM-specific operation handling
        // This could include:
        // - PVM-specific opcode overrides
        // - PVM-specific cheatcode handling
        // - PVM-specific state management

        false
    }
}

/// Defines the strategy for [super::Cheatcodes].
#[derive(Debug)]
pub struct CheatcodeInspectorStrategy {
    /// Strategy runner.
    pub runner: &'static dyn CheatcodeInspectorStrategyRunner,
    /// Strategy context.
    pub context: Box<dyn CheatcodeInspectorStrategyContext>,
}

impl CheatcodeInspectorStrategy {
    /// Creates a new EVM strategy for the [super::Cheatcodes].
    pub fn new_evm() -> Self {
        Self { runner: &EvmCheatcodeInspectorStrategyRunner, context: Box::new(()) }
    }

    /// Creates a new PVM strategy for the [super::Cheatcodes].
    pub fn new_pvm(pvm_env: Option<PvmEnvironment>) -> Self {
        Self {
            runner: &PvmCheatcodeInspectorStrategyRunner,
            context: Box::new(PvmCheatcodeInspectorStrategyContext::new(pvm_env)),
        }
    }
}

impl Clone for CheatcodeInspectorStrategy {
    fn clone(&self) -> Self {
        Self { runner: self.runner, context: self.context.new_cloned() }
    }
}

/// Helper function to get PVM context
fn get_pvm_context(
    ctx: &mut dyn CheatcodeInspectorStrategyContext,
) -> &mut PvmCheatcodeInspectorStrategyContext {
    ctx.as_any_mut().downcast_mut().expect("expected PvmCheatcodeInspectorStrategyContext")
}

// Legacy type aliases for backward compatibility
pub type CheatcodesStrategy = CheatcodeInspectorStrategy;
