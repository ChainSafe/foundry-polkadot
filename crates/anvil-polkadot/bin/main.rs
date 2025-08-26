//! The `anvil-polkadot` CLI: a fast local ethereum-compatible development node, based on
//! polkadot-sdk.
use std::{net::SocketAddr, time::Instant};

fn main() {
    let start_time = Instant::now();

    if let Err(err) = anvil_polkadot::run(start_time) {
        let _ = foundry_common::sh_err!("{err:?}");
        std::process::exit(1);
    }
}
