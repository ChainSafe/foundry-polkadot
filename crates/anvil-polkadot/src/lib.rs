//! Anvil is a fast local Ethereum development node.

#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]

use crate::{
    config::AnvilNodeConfig,
    logging::{LoggingManager, NodeLogLayer},
    substrate_node::service::Service,
};
use eyre::Result;
use opts::{Anvil, AnvilSubcommand};
use polkadot_sdk::{
    sc_cli::{self, SubstrateCli},
    sc_network::Litep2pNetworkBackend,
    sc_service::{self, TaskManager},
};
use server::try_spawn_ipc;
use std::net::SocketAddr;

mod substrate_node;

mod config;

/// commandline output
pub mod logging;
/// types for subscriptions
pub mod pubsub;
/// axum RPC server implementations
pub mod server;

mod api_server;

/// contains cli command
#[cfg(feature = "cmd")]
pub mod cmd;

#[cfg(feature = "cmd")]
pub mod opts;

#[macro_use]
extern crate foundry_common;

#[macro_use]
extern crate tracing;

use clap::{CommandFactory, Parser};
use foundry_cli::utils;

/// Run the `anvil` command line interface.
#[cfg(feature = "cmd")]
pub fn run() -> Result<()> {
    setup()?;

    let args = Anvil::parse();
    args.global.init()?;

    run_command(args)
}

/// Setup the panic handler and other utilities.
pub fn setup() -> Result<()> {
    utils::install_crypto_provider();
    foundry_cli::handler::install();
    utils::load_dotenv();
    utils::enable_paint();

    Ok(())
}

/// Run the subcommand.
pub fn run_command(args: Anvil) -> Result<()> {
    if let Some(cmd) = &args.cmd {
        match cmd {
            AnvilSubcommand::Completions { shell } => {
                clap_complete::generate(
                    *shell,
                    &mut Anvil::command(),
                    "anvil-polkadot",
                    &mut std::io::stdout(),
                );
            }
            AnvilSubcommand::GenerateFigSpec => clap_complete::generate(
                clap_complete_fig::Fig,
                &mut Anvil::command(),
                "anvil-polkadot",
                &mut std::io::stdout(),
            ),
        }
        return Ok(())
    }

    let _ = fdlimit::raise_fd_limit();

    let (anvil_config, substrate_config) = args.node.clone().into_node_config()?;
    let logger = if anvil_config.enable_tracing { init_tracing() } else { Default::default() };
    logger.set_enabled(!anvil_config.silent);

    let runner = args.create_runner(&substrate_config)?;

    Ok(runner.run_node_until_exit(|config| async move { spawn(anvil_config, config).await })?)
}

pub async fn spawn(
    anvil_config: AnvilNodeConfig,
    substrate_config: sc_service::Configuration,
) -> Result<TaskManager, sc_cli::Error> {
    // Spawn the substrate node.
    let substrate_service =
        substrate_node::service::new::<Litep2pNetworkBackend>(&anvil_config, substrate_config)
            .map_err(sc_cli::Error::Service)?;

    // Spawn the other tasks.
    spawn_anvil_tasks(anvil_config, &substrate_service)
        .await
        .map_err(|err| sc_cli::Error::Application(err.into()))?;

    Ok(substrate_service.task_manager)
}

pub async fn spawn_anvil_tasks(anvil_config: AnvilNodeConfig, service: &Service) -> Result<()> {
    let logger = if anvil_config.enable_tracing { init_tracing() } else { Default::default() };
    logger.set_enabled(!anvil_config.silent);
    let mut addresses = Vec::with_capacity(anvil_config.host.len());

    // Spawn the api server.
    let api_handle = api_server::spawn(&service);

    // Spawn the network servers.
    for addr in &anvil_config.host {
        let sock_addr = SocketAddr::new(*addr, anvil_config.port);

        // Create a TCP listener.
        let tcp_listener = tokio::net::TcpListener::bind(sock_addr).await?;
        addresses.push(tcp_listener.local_addr()?);

        // Spawn the server future on a new task.
        let srv =
            server::serve_on(tcp_listener, anvil_config.server_config.clone(), api_handle.clone());
        let spawn_handle = service.task_manager.spawn_handle();
        spawn_handle.spawn(
            "anvil",
            "anvil-tcp",
            async move { srv.await.expect("TCP server failure") },
        );
    }

    // If configured, spawn the IPC server.
    anvil_config
        .get_ipc_path()
        .map(|path| try_spawn_ipc(&service.task_manager, path, api_handle))
        .transpose()?;

    // TODO: there was a print here.

    Ok(())
}

#[doc(hidden)]
// TODO: this tracing intialisation conflicts with the one in substrate.
fn init_tracing() -> LoggingManager {
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
