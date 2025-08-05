use crate::{
    init_tracing,
    opts::{Anvil, AnvilSubcommand},
    spawn_main, substrate_node,
};
use clap::{CommandFactory, Parser};
use eyre::Result;
use foundry_cli::{handler, utils};
use polkadot_sdk::{
    sc_cli::{self, SubstrateCli},
    sc_network::Litep2pNetworkBackend,
    sc_service::TaskManager,
};

/// Run the `anvil` command line interface.
pub fn run() -> Result<()> {
    setup()?;

    let args = Anvil::parse();
    args.global.init()?;

    run_command(args)
}

/// Setup the exception handler and other utilities.
pub fn setup() -> Result<()> {
    utils::install_crypto_provider();
    handler::install();
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
                    "anvil",
                    &mut std::io::stdout(),
                );
            }
            AnvilSubcommand::GenerateFigSpec => clap_complete::generate(
                clap_complete_fig::Fig,
                &mut Anvil::command(),
                "anvil",
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

    Ok(runner.run_node_until_exit(|config| async move {
        // Spawn the substrate node.
        let substrate_service =
            substrate_node::service::new::<Litep2pNetworkBackend>(&anvil_config, config)
                .map_err(sc_cli::Error::Service)?;

        // Spawn the other tasks.
        spawn_main(anvil_config, &substrate_service)
            .await
            .map_err(|err| sc_cli::Error::Application(err.into()))?;

        Ok::<TaskManager, sc_cli::Error>(substrate_service.task_manager)
    })?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_cli() {
        Anvil::command().debug_assert();
    }

    #[test]
    fn can_parse_help() {
        let _: Anvil = Anvil::parse_from(["anvil", "--help"]);
    }

    #[test]
    fn can_parse_short_version() {
        let _: Anvil = Anvil::parse_from(["anvil", "-V"]);
    }

    #[test]
    fn can_parse_long_version() {
        let _: Anvil = Anvil::parse_from(["anvil", "--version"]);
    }

    #[test]
    fn can_parse_completions() {
        let args: Anvil = Anvil::parse_from(["anvil", "completions", "bash"]);
        assert!(matches!(
            args.cmd,
            Some(AnvilSubcommand::Completions { shell: clap_complete::Shell::Bash })
        ));
    }
}
