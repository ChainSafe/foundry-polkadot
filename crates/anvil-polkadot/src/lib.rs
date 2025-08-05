//! Anvil is a fast local Ethereum development node.

#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]

use crate::{
    logging::{LoggingManager, NodeLogLayer},
    substrate_node::service::Service,
};
use eyre::Result;
use server::try_spawn_ipc;
use std::net::SocketAddr;

mod substrate_node;

mod config;
pub use config::{AccountGenerator, AnvilNodeConfig, CHAIN_ID, DEFAULT_GAS_LIMIT, VERSION_MESSAGE};

/// support for polling filters
pub mod filter;
/// commandline output
pub mod logging;
/// types for subscriptions
pub mod pubsub;
/// axum RPC server implementations
pub mod server;

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
pub async fn spawn_main(anvil_config: AnvilNodeConfig, service: &Service) -> Result<()> {
    let logger = if anvil_config.enable_tracing { init_tracing() } else { Default::default() };
    logger.set_enabled(!anvil_config.silent);

    // We need a service that forwards requests from the ethereum json rpc to the substrate node.
    // For later: Can we reuse the revive rpc logic?

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

    // let dump_state = self.dump_state_path();
    //     let dump_interval =
    //         self.state_interval.map(Duration::from_secs).unwrap_or(DEFAULT_DUMP_INTERVAL);
    //     let preserve_historical_states = self.preserve_historical_states;
    //     let mut state_dumper =
    //         PeriodicStateDumper::new(dump_state, dump_interval, preserve_historical_states);

    // TODO: SPAWN ALL TASKS.

    let mut addresses = Vec::with_capacity(anvil_config.host.len());

    for addr in &anvil_config.host {
        let sock_addr = SocketAddr::new(*addr, anvil_config.port);

        // Create a TCP listener.
        let tcp_listener = tokio::net::TcpListener::bind(sock_addr).await?;
        addresses.push(tcp_listener.local_addr()?);

        // Spawn the server future on a new task.
        let srv = server::serve_on(tcp_listener, anvil_config.server_config.clone());
        let spawn_handle = service.task_manager.spawn_handle();
        spawn_handle.spawn(
            "anvil",
            "anvil-tcp",
            async move { srv.await.expect("TCP server failure") },
        );
    }

    anvil_config
        .get_ipc_path()
        .map(|path| try_spawn_ipc(&service.task_manager, path))
        .transpose()?;

    // handle.print()?;

    Ok(())
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
