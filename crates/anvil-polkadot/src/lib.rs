//! Anvil is a fast local Ethereum development node.

#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]

use crate::{
    logging::{LoggingManager, NodeLogLayer},
    server::error::{NodeError, NodeResult},
    shutdown::Signal,
    tasks::TaskManager,
};
use alloy_primitives::{Address, U256};
use alloy_signer_local::PrivateKeySigner;
use eyre::Result;
use foundry_common::provider::{ProviderBuilder, RetryProvider};
use futures::{FutureExt, TryFutureExt};
use server::try_spawn_ipc;
use std::{
    future::Future,
    net::SocketAddr,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::{
    runtime::Handle,
    task::{JoinError, JoinHandle},
};

/// contains the background service that drives the node
mod service;

mod config;
pub use config::{AccountGenerator, NodeConfig, CHAIN_ID, DEFAULT_GAS_LIMIT, VERSION_MESSAGE};

/// support for polling filters
pub mod filter;
/// commandline output
pub mod logging;
/// types for subscriptions
pub mod pubsub;
/// axum RPC server implementations
pub mod server;
/// Futures for shutdown signal
mod shutdown;
/// additional task management
mod tasks;

/// contains cli command
#[cfg(feature = "cmd")]
pub mod cmd;

#[cfg(feature = "cmd")]
pub mod args;

#[cfg(feature = "cmd")]
pub mod opts;

#[macro_use]
extern crate foundry_common;

#[macro_use]
extern crate tracing;

/// Creates the node and runs the server.
///
/// Returns the [EthApi] that can be used to interact with the node and the [JoinHandle] of the
/// task.
///
/// # Panics
///
/// Panics if any error occurs. For a non-panicking version, use [`try_spawn`].
///
///
/// # Examples
///
/// ```no_run
/// # use anvil::NodeConfig;
/// # async fn spawn() -> eyre::Result<()> {
/// let config = NodeConfig::default();
/// let (api, handle) = anvil::spawn(config).await;
///
/// // use api
///
/// // wait forever
/// handle.await.unwrap().unwrap();
/// # Ok(())
/// # }
/// ```
// pub async fn spawn(config: NodeConfig) -> (EthApi, NodeHandle) {
//     try_spawn(config).await.expect("failed to spawn node")
// }

/// Creates the node and runs the server
///
/// Returns the [EthApi] that can be used to interact with the node and the [JoinHandle] of the
/// task.
///
/// # Examples
///
/// ```no_run
/// # use anvil::NodeConfig;
/// # async fn spawn() -> eyre::Result<()> {
/// let config = NodeConfig::default();
/// let (api, handle) = anvil::try_spawn(config).await?;
///
/// // use api
///
/// // wait forever
/// handle.await??;
/// # Ok(())
/// # }
/// ```
pub async fn try_spawn(mut config: NodeConfig) -> Result<NodeHandle> {
    let logger = if config.enable_tracing { init_tracing() } else { Default::default() };
    logger.set_enabled(!config.silent);

    // Launch a substrate node.
    let substrate_node = launch_in_process_substrate_node(&config);
    // We need a service that forwards requests from the ethereum json rpc to the substrate node.
    // For later: Can we reuse the revive rpc logic?

    let NodeConfig {
        signer_accounts,
        block_time,
        port,
        max_transactions,
        server_config,
        no_mining,
        transaction_order,
        genesis,
        mixed_mining,
        ..
    } = config.clone();

    // let dev_signer: Box<dyn EthSigner> = Box::new(DevSigner::new(signer_accounts));
    // let mut signers = vec![dev_signer];
    // if let Some(genesis) = genesis {
    //     let genesis_signers = genesis
    //         .alloc
    //         .values()
    //         .filter_map(|acc| acc.private_key)
    //         .flat_map(|k| PrivateKeySigner::from_bytes(&k))
    //         .collect::<Vec<_>>();
    //     if !genesis_signers.is_empty() {
    //         signers.push(Box::new(DevSigner::new(genesis_signers)));
    //     }
    // }

    let mut servers = Vec::with_capacity(config.host.len());
    let mut addresses = Vec::with_capacity(config.host.len());

    for addr in &config.host {
        let sock_addr = SocketAddr::new(*addr, port);

        // Create a TCP listener.
        let tcp_listener = tokio::net::TcpListener::bind(sock_addr).await?;
        addresses.push(tcp_listener.local_addr()?);

        // Spawn the server future on a new task.
        let srv = server::serve_on(tcp_listener, server_config.clone());
        servers.push(tokio::task::spawn(srv.map_err(Into::into)));
    }

    let tokio_handle = Handle::current();
    let (signal, on_shutdown) = shutdown::signal();
    let task_manager = TaskManager::new(tokio_handle, on_shutdown);

    let ipc_task = config.get_ipc_path().map(|path| try_spawn_ipc(path)).transpose()?;

    let handle =
        NodeHandle { config, servers, ipc_task, addresses, _signal: Some(signal), task_manager };

    handle.print()?;

    Ok(handle)
}

type IpcTask = JoinHandle<()>;

/// A handle to the spawned node and server tasks.
///
/// This future will resolve if either the node or server task resolve/fail.
pub struct NodeHandle {
    config: NodeConfig,
    /// The address of the running rpc server.
    addresses: Vec<SocketAddr>,
    /// Join handles (one per socket) for the Anvil server.
    pub servers: Vec<JoinHandle<Result<(), NodeError>>>,
    /// The future that joins the ipc server, if any.
    ipc_task: Option<IpcTask>,
    /// A signal that fires the shutdown, fired on drop.
    _signal: Option<Signal>,
    /// A task manager that can be used to spawn additional tasks.
    task_manager: TaskManager,
}

impl Drop for NodeHandle {
    fn drop(&mut self) {
        // Fire shutdown signal to make sure anvil instance is terminated.
        if let Some(signal) = self._signal.take() {
            let _ = signal.fire();
        }
    }
}

impl NodeHandle {
    /// The [NodeConfig] the node was launched with.
    pub fn config(&self) -> &NodeConfig {
        &self.config
    }

    /// Prints the launch info.
    pub(crate) fn print(&self) -> Result<()> {
        self.config.print()?;
        if !self.config.silent {
            if let Some(ipc_path) = self.ipc_path() {
                sh_println!("IPC path: {ipc_path}")?;
            }
            sh_println!(
                "Listening on {}",
                self.addresses
                    .iter()
                    .map(|addr| { addr.to_string() })
                    .collect::<Vec<String>>()
                    .join(", ")
            )?;
        }
        Ok(())
    }

    /// The address of the launched server.
    ///
    /// **N.B.** this may not necessarily be the same `host + port` as configured in the
    /// `NodeConfig`, if port was set to 0, then the OS auto picks an available port.
    pub fn socket_address(&self) -> &SocketAddr {
        &self.addresses[0]
    }

    /// Returns the http endpoint.
    pub fn http_endpoint(&self) -> String {
        format!("http://{}", self.socket_address())
    }

    /// Returns the websocket endpoint.
    pub fn ws_endpoint(&self) -> String {
        format!("ws://{}", self.socket_address())
    }

    /// Returns the path of the launched ipc server, if any.
    pub fn ipc_path(&self) -> Option<String> {
        self.config.get_ipc_path()
    }

    /// Constructs a [`RetryProvider`] for this handle's HTTP endpoint.
    pub fn http_provider(&self) -> RetryProvider {
        ProviderBuilder::new(&self.http_endpoint()).build().expect("failed to build HTTP provider")
    }

    /// Constructs a [`RetryProvider`] for this handle's WS endpoint.
    pub fn ws_provider(&self) -> RetryProvider {
        ProviderBuilder::new(&self.ws_endpoint()).build().expect("failed to build WS provider")
    }

    /// Constructs a [`RetryProvider`] for this handle's IPC endpoint, if any.
    pub fn ipc_provider(&self) -> Option<RetryProvider> {
        ProviderBuilder::new(&self.config.get_ipc_path()?).build().ok()
    }

    /// Signer accounts that can sign messages/transactions from the EVM node.
    pub fn dev_accounts(&self) -> impl Iterator<Item = Address> + '_ {
        self.config.signer_accounts.iter().map(|wallet| wallet.address())
    }

    /// Signer accounts that can sign messages/transactions from the EVM node.
    pub fn dev_wallets(&self) -> impl Iterator<Item = PrivateKeySigner> + '_ {
        self.config.signer_accounts.iter().cloned()
    }

    /// Accounts that will be initialised with `genesis_balance` in the genesis block.
    pub fn genesis_accounts(&self) -> impl Iterator<Item = Address> + '_ {
        self.config.genesis_accounts.iter().map(|w| w.address())
    }

    /// Native token balance of every genesis account in the genesis block.
    pub fn genesis_balance(&self) -> U256 {
        self.config.genesis_balance
    }

    /// Default gas price for all txs.
    pub fn gas_price(&self) -> u128 {
        self.config.get_gas_price()
    }

    /// Returns the shutdown signal.
    pub fn shutdown_signal(&self) -> &Option<Signal> {
        &self._signal
    }

    /// Returns mutable access to the shutdown signal.
    ///
    /// This can be used to extract the Signal.
    pub fn shutdown_signal_mut(&mut self) -> &mut Option<Signal> {
        &mut self._signal
    }

    /// Returns the task manager that can be used to spawn new tasks.
    ///
    /// ```
    /// use anvil::NodeHandle;
    /// # fn t(handle: NodeHandle) {
    /// let task_manager = handle.task_manager();
    /// let on_shutdown = task_manager.on_shutdown();
    ///
    /// task_manager.spawn(async move {
    ///     on_shutdown.await;
    ///     // do something
    /// });
    ///
    /// # }
    /// ```
    pub fn task_manager(&self) -> &TaskManager {
        &self.task_manager
    }
}

impl Future for NodeHandle {
    type Output = Result<NodeResult<()>, JoinError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let pin = self.get_mut();

        // poll the ipc task
        if let Some(mut ipc) = pin.ipc_task.take() {
            if let Poll::Ready(res) = ipc.poll_unpin(cx) {
                return Poll::Ready(res.map(|()| Ok(())));
            } else {
                pin.ipc_task = Some(ipc);
            }
        }

        // poll the axum server handles
        for server in &mut pin.servers {
            if let Poll::Ready(res) = server.poll_unpin(cx) {
                return Poll::Ready(res);
            }
        }

        Poll::Pending
    }
}

#[doc(hidden)]
pub fn init_tracing() -> LoggingManager {
    use tracing_subscriber::prelude::*;

    let manager = LoggingManager::default();
    // check whether `RUST_LOG` is explicitly set
    let _ = if std::env::var("RUST_LOG").is_ok() {
        tracing_subscriber::Registry::default()
            .with(tracing_subscriber::EnvFilter::from_default_env())
            .with(tracing_subscriber::fmt::layer())
            .try_init()
    } else {
        tracing_subscriber::Registry::default()
            .with(NodeLogLayer::new(manager.clone()))
            .with(
                tracing_subscriber::fmt::layer()
                    .without_time()
                    .with_target(false)
                    .with_level(false),
            )
            .try_init()
    };

    manager
}

fn launch_in_process_substrate_node(config: &NodeConfig) {}
