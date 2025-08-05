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

mod responder;

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

pub async fn spawn_main(anvil_config: AnvilNodeConfig, service: &Service) -> Result<()> {
    let logger = if anvil_config.enable_tracing { init_tracing() } else { Default::default() };
    logger.set_enabled(!anvil_config.silent);
    let mut addresses = Vec::with_capacity(anvil_config.host.len());

    let responder_handle = responder::spawn(&service);

    for addr in &anvil_config.host {
        let sock_addr = SocketAddr::new(*addr, anvil_config.port);

        // Create a TCP listener.
        let tcp_listener = tokio::net::TcpListener::bind(sock_addr).await?;
        addresses.push(tcp_listener.local_addr()?);

        // Spawn the server future on a new task.
        let srv = server::serve_on(
            tcp_listener,
            anvil_config.server_config.clone(),
            responder_handle.clone(),
        );
        let spawn_handle = service.task_manager.spawn_handle();
        spawn_handle.spawn(
            "anvil",
            "anvil-tcp",
            async move { srv.await.expect("TCP server failure") },
        );
    }

    anvil_config
        .get_ipc_path()
        .map(|path| try_spawn_ipc(&service.task_manager, path, responder_handle))
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
