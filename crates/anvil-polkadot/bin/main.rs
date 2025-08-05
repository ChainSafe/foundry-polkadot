//! The `anvil` CLI: a fast local Ethereum development node, akin to Hardhat Network, Tenderly.

use anvil_polkadot::args::run;

fn main() {
    if let Err(err) = run() {
        let _ = foundry_common::sh_err!("{err:?}");
        std::process::exit(1);
    }
}
