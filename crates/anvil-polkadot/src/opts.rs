use crate::{cmd::NodeArgs, substrate_node::chain_spec};
use clap::{Parser, Subcommand};
use foundry_cli::opts::GlobalArgs;
use foundry_common::version::{LONG_VERSION, SHORT_VERSION};
use polkadot_sdk::{sc_cli::SubstrateCli, sc_service};

/// A fast local Ethereum development node.
#[derive(Parser)]
#[command(name = "anvil", version = SHORT_VERSION, long_version = LONG_VERSION, next_display_order = None)]
pub struct Anvil {
    /// Include the global arguments.
    #[command(flatten)]
    pub global: GlobalArgs,

    #[command(flatten)]
    pub node: NodeArgs,

    #[command(subcommand)]
    pub cmd: Option<AnvilSubcommand>,
}

#[derive(Subcommand)]
pub enum AnvilSubcommand {
    /// Generate shell completions script.
    #[command(visible_alias = "com")]
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },

    /// Generate Fig autocompletion spec.
    #[command(visible_alias = "fig")]
    GenerateFigSpec,
}

impl SubstrateCli for Anvil {
    fn impl_name() -> String {
        "Anvil Polkadot".into()
    }

    fn impl_version() -> String {
        "V1".into()
    }

    fn description() -> String {
        "Description".into()
    }

    fn author() -> String {
        "Authors".into()
    }

    fn support_url() -> String {
        "URL".into()
    }

    fn copyright_start_year() -> i32 {
        2025
    }

    fn executable_name() -> String {
        "anvil-polkadot".into()
    }

    fn load_spec(&self, id: &str) -> std::result::Result<Box<dyn sc_service::ChainSpec>, String> {
        Ok(match id {
            "dev" | "" => Box::new(chain_spec::development_chain_spec()?),
            path => {
                Box::new(chain_spec::ChainSpec::from_json_file(std::path::PathBuf::from(path))?)
            }
        })
    }
}
