use crate::{
    config::{AnvilNodeConfig, SubstrateNodeConfig, DEFAULT_MNEMONIC},
    AccountGenerator, CHAIN_ID,
};
use alloy_genesis::Genesis;
use alloy_primitives::{utils::Unit, B256, U256};
use alloy_signer_local::coins_bip39::{English, Mnemonic};
use anvil_server::ServerConfig;
use clap::Parser;
use foundry_common::shell;
use foundry_config::Chain;
use futures::FutureExt;
use rand::{rngs::StdRng, SeedableRng};
use serde::{Deserialize, Serialize};
use std::{
    future::Future,
    net::IpAddr,
    path::{Path, PathBuf},
    pin::Pin,
    str::FromStr,
    task::{Context, Poll},
    time::Duration,
};
use tokio::time::{Instant, Interval};

/// Modes that determine the transaction ordering of the mempool
///
/// This type controls the transaction order via the priority metric of a transaction
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TransactionOrder {
    /// Keep the pool transaction transactions sorted in the order they arrive.
    ///
    /// This will essentially assign every transaction the exact priority so the order is
    /// determined by their internal id
    Fifo,
    /// This means that it prioritizes transactions based on the fees paid to the miner.
    #[default]
    Fees,
}

impl FromStr for TransactionOrder {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.to_lowercase();
        let order = match s.as_str() {
            "fees" => Self::Fees,
            "fifo" => Self::Fifo,
            _ => return Err(format!("Unknown TransactionOrder: `{s}`")),
        };
        Ok(order)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SerializableState {}

impl SerializableState {
    /// This is used as the clap `value_parser` implementation
    #[allow(dead_code)]
    pub(crate) fn parse(path: &str) -> Result<Self, String> {
        Ok(Self {})
    }
}

#[derive(Clone, Debug, Parser)]
pub struct NodeArgs {
    /// Port number to listen on.
    #[arg(long, short, default_value = "8545", value_name = "NUM")]
    pub port: u16,

    /// Number of dev accounts to generate and configure.
    #[arg(long, short, default_value = "10", value_name = "NUM")]
    pub accounts: u64,

    /// The balance of every dev account in Ether.
    #[arg(long, default_value = "10000", value_name = "NUM")]
    pub balance: u64,

    /// The timestamp of the genesis block.
    #[arg(long, value_name = "NUM")]
    pub timestamp: Option<u64>,

    /// The number of the genesis block.
    #[arg(long, value_name = "NUM")]
    pub number: Option<u64>,

    /// BIP39 mnemonic phrase used for generating accounts.
    /// Cannot be used if `mnemonic_random` or `mnemonic_seed` are used.
    #[arg(long, short, conflicts_with_all = &["mnemonic_seed", "mnemonic_random"])]
    pub mnemonic: Option<String>,

    /// Automatically generates a BIP39 mnemonic phrase, and derives accounts from it.
    /// Cannot be used with other `mnemonic` options.
    /// You can specify the number of words you want in the mnemonic.
    /// [default: 12]
    #[arg(long, conflicts_with_all = &["mnemonic", "mnemonic_seed"], default_missing_value = "12", num_args(0..=1))]
    pub mnemonic_random: Option<usize>,

    /// Generates a BIP39 mnemonic phrase from a given seed
    /// Cannot be used with other `mnemonic` options.
    ///
    /// CAREFUL: This is NOT SAFE and should only be used for testing.
    /// Never use the private keys generated in production.
    #[arg(long = "mnemonic-seed-unsafe", conflicts_with_all = &["mnemonic", "mnemonic_random"])]
    pub mnemonic_seed: Option<u64>,

    /// Sets the derivation path of the child key to be derived.
    ///
    /// [default: m/44'/60'/0'/0/]
    #[arg(long)]
    pub derivation_path: Option<String>,

    /// Block time in seconds for interval mining.
    #[arg(short, long, visible_alias = "blockTime", value_name = "SECONDS", value_parser = duration_from_secs_f64)]
    pub block_time: Option<Duration>,

    /// Slots in an epoch
    #[arg(long, value_name = "SLOTS_IN_AN_EPOCH", default_value_t = 32)]
    pub slots_in_an_epoch: u64,

    /// Writes output of `anvil` as json to user-specified file.
    #[arg(long, value_name = "FILE", value_hint = clap::ValueHint::FilePath)]
    pub config_out: Option<PathBuf>,

    /// Disable auto and interval mining, and mine on demand instead.
    #[arg(long, visible_alias = "no-mine", conflicts_with = "block_time")]
    pub no_mining: bool,

    #[arg(long, visible_alias = "mixed-mining", requires = "block_time")]
    pub mixed_mining: bool,

    /// The hosts the server will listen on.
    #[arg(
        long,
        value_name = "IP_ADDR",
        env = "ANVIL_IP_ADDR",
        default_value = "127.0.0.1",
        help_heading = "Server options",
        value_delimiter = ','
    )]
    pub host: Vec<IpAddr>,

    /// How transactions are sorted in the mempool.
    /// TODO: see how this can be used with our transaction pool.
    #[arg(long, default_value = "fees")]
    pub order: TransactionOrder,

    /// Initialize the genesis block with the given `genesis.json` file.
    /// TODO: can we even use this? besides the accounts storage.
    #[arg(long, value_name = "PATH", value_parser= read_genesis_file)]
    pub init: Option<Genesis>,

    /// This is an alias for both --load-state and --dump-state.
    ///
    /// It initializes the chain with the state and block environment stored at the file, if it
    /// exists, and dumps the chain's state on exit.
    #[arg(
        long,
        value_name = "PATH",
        value_parser = StateFile::parse,
        conflicts_with_all = &[
            "init",
            "dump_state",
            "load_state"
        ]
    )]
    /// TODO: is this going to be the substrate runtime state?
    pub state: Option<StateFile>,

    /// Interval in seconds at which the state and block environment is to be dumped to disk.
    ///
    /// See --state and --dump-state
    #[arg(short, long, value_name = "SECONDS")]
    pub state_interval: Option<u64>,

    /// Dump the state and block environment of chain on exit to the given file.
    ///
    /// If the value is a directory, the state will be written to `<VALUE>/state.json`.
    #[arg(long, value_name = "PATH", conflicts_with = "init")]
    pub dump_state: Option<PathBuf>,

    /// Preserve historical state snapshots when dumping the state.
    ///
    /// This will save the in-memory states of the chain at particular block hashes.
    ///
    /// These historical states will be loaded into the memory when `--load-state` / `--state`, and
    /// aids in RPC calls beyond the block at which state was dumped.
    #[arg(long, conflicts_with = "init", default_value = "false")]
    pub preserve_historical_states: bool,

    /// Initialize the chain from a previously saved state snapshot.
    #[arg(
        long,
        value_name = "PATH",
        value_parser = SerializableState::parse,
        conflicts_with = "init"
    )]
    /// TODO: this needs to be serializable substrate state
    pub load_state: Option<SerializableState>,

    #[arg(long, help = IPC_HELP, value_name = "PATH", visible_alias = "ipcpath")]
    pub ipc: Option<Option<String>>,

    /// Don't keep full chain history.
    /// If a number argument is specified, at most this number of states is kept in memory.
    ///
    /// If enabled, no state will be persisted on disk, so `max_persisted_states` will be 0.
    #[arg(long)]
    /// TODO: route this to the state pruning on substrate. See if it even makes sense with
    /// max_persisted_states.
    pub prune_history: Option<Option<usize>>,

    /// Max number of states to persist on disk.
    ///
    /// Note that `prune_history` will overwrite `max_persisted_states` to 0.
    #[arg(long, conflicts_with = "prune_history")]
    pub max_persisted_states: Option<usize>,

    /// Number of blocks with transactions to keep in memory.
    #[arg(long)]
    /// TODO: Most likely useless
    pub transaction_block_keeper: Option<usize>,

    #[command(flatten)]
    /// TODO: we'll most likely need to heavily trim this.
    pub evm: AnvilEvmArgs,

    #[command(flatten)]
    pub server_config: ServerConfig,

    /// Path to the cache directory where states are stored.    
    #[arg(long, value_name = "PATH")]
    pub cache_path: Option<PathBuf>,
}

#[cfg(windows)]
const IPC_HELP: &str =
    "Launch an ipc server at the given path or default path = `\\.\\pipe\\anvil.ipc`";

/// The default IPC endpoint
#[cfg(not(windows))]
const IPC_HELP: &str = "Launch an ipc server at the given path or default path = `/tmp/anvil.ipc`";

/// Default interval for periodically dumping the state.
const DEFAULT_DUMP_INTERVAL: Duration = Duration::from_secs(60);

impl NodeArgs {
    pub fn into_node_config(self) -> eyre::Result<(AnvilNodeConfig, SubstrateNodeConfig)> {
        let genesis_balance = Unit::ETHER.wei().saturating_mul(U256::from(self.balance));

        let anvil_config = AnvilNodeConfig::default()
            .with_gas_limit(self.evm.gas_limit)
            .disable_block_gas_limit(self.evm.disable_block_gas_limit)
            .with_gas_price(self.evm.gas_price)
            .with_blocktime(self.block_time)
            .with_no_mining(self.no_mining)
            .with_mixed_mining(self.mixed_mining, self.block_time)
            .with_account_generator(self.account_generator())?
            .with_genesis_balance(genesis_balance)
            .with_genesis_timestamp(self.timestamp)
            .with_genesis_block_number(self.number)
            .with_port(self.port)
            .with_base_fee(self.evm.block_base_fee_per_gas)
            .disable_min_priority_fee(self.evm.disable_min_priority_fee)
            .with_server_config(self.server_config)
            .with_host(self.host)
            .set_silent(shell::is_quiet())
            .set_config_out(self.config_out)
            .with_chain_id(self.evm.chain_id)
            .with_transaction_order(self.order)
            .with_genesis(self.init)
            .with_steps_tracing(self.evm.steps_tracing)
            .with_print_logs(!self.evm.disable_console_log)
            .with_print_traces(self.evm.print_traces)
            .with_auto_impersonate(self.evm.auto_impersonate)
            .with_ipc(self.ipc)
            .with_code_size_limit(self.evm.code_size_limit)
            .disable_code_size_limit(self.evm.disable_code_size_limit)
            .set_pruned_history(self.prune_history)
            .with_init_state(self.load_state.or_else(|| self.state.and_then(|s| s.state)))
            .with_transaction_block_keeper(self.transaction_block_keeper)
            .with_max_persisted_states(self.max_persisted_states)
            .with_disable_default_create2_deployer(self.evm.disable_default_create2_deployer)
            .with_slots_in_an_epoch(self.slots_in_an_epoch)
            .with_memory_limit(self.evm.memory_limit)
            .with_cache_path(self.cache_path);

        let substrate_node_config = SubstrateNodeConfig::new(&anvil_config);

        Ok((anvil_config, substrate_node_config))
    }

    fn account_generator(&self) -> AccountGenerator {
        let mut gen = AccountGenerator::new(self.accounts as usize)
            .phrase(DEFAULT_MNEMONIC)
            .chain_id(self.evm.chain_id.unwrap_or(CHAIN_ID.into()));
        if let Some(ref mnemonic) = self.mnemonic {
            gen = gen.phrase(mnemonic);
        } else if let Some(count) = self.mnemonic_random {
            let mut rng = rand::thread_rng();
            let mnemonic = match Mnemonic::<English>::new_with_count(&mut rng, count) {
                Ok(mnemonic) => mnemonic.to_phrase(),
                Err(_) => DEFAULT_MNEMONIC.to_string(),
            };
            gen = gen.phrase(mnemonic);
        } else if let Some(seed) = self.mnemonic_seed {
            let mut seed = StdRng::seed_from_u64(seed);
            let mnemonic = Mnemonic::<English>::new(&mut seed).to_phrase();
            gen = gen.phrase(mnemonic);
        }
        if let Some(ref derivation) = self.derivation_path {
            gen = gen.derivation_path(derivation);
        }
        gen
    }

    /// Returns the location where to dump the state to.
    fn dump_state_path(&self) -> Option<PathBuf> {
        self.dump_state.as_ref().or_else(|| self.state.as_ref().map(|s| &s.path)).cloned()
    }
}

/// Anvil's EVM related arguments.
#[derive(Clone, Debug, Parser)]
#[command(next_help_heading = "EVM options")]
pub struct AnvilEvmArgs {
    /// The block gas limit.
    #[arg(long, alias = "block-gas-limit", help_heading = "Environment config")]
    pub gas_limit: Option<u128>,

    /// Disable the `call.gas_limit <= block.gas_limit` constraint.
    #[arg(
        long,
        value_name = "DISABLE_GAS_LIMIT",
        help_heading = "Environment config",
        alias = "disable-gas-limit",
        conflicts_with = "gas_limit"
    )]
    pub disable_block_gas_limit: bool,

    /// EIP-170: Contract code size limit in bytes. Useful to increase this because of tests. To
    /// disable entirely, use `--disable-code-size-limit`. By default, it is 0x6000 (~25kb).
    #[arg(long, value_name = "CODE_SIZE", help_heading = "Environment config")]
    pub code_size_limit: Option<usize>,

    /// Disable EIP-170: Contract code size limit.
    #[arg(
        long,
        value_name = "DISABLE_CODE_SIZE_LIMIT",
        conflicts_with = "code_size_limit",
        help_heading = "Environment config"
    )]
    pub disable_code_size_limit: bool,

    /// The gas price.
    #[arg(long, help_heading = "Environment config")]
    pub gas_price: Option<u128>,

    /// The base fee in a block.
    #[arg(
        long,
        visible_alias = "base-fee",
        value_name = "FEE",
        help_heading = "Environment config"
    )]
    pub block_base_fee_per_gas: Option<u64>,

    /// Disable the enforcement of a minimum suggested priority fee.
    #[arg(long, visible_alias = "no-priority-fee", help_heading = "Environment config")]
    pub disable_min_priority_fee: bool,

    /// The chain ID.
    #[arg(long, alias = "chain", help_heading = "Environment config")]
    pub chain_id: Option<Chain>,

    /// Enable steps tracing used for debug calls returning geth-style traces
    #[arg(long, visible_alias = "tracing")]
    pub steps_tracing: bool,

    /// Disable printing of `console.log` invocations to stdout.
    #[arg(long, visible_alias = "no-console-log")]
    pub disable_console_log: bool,

    /// Enable printing of traces for executed transactions and `eth_call` to stdout.
    #[arg(long, visible_alias = "enable-trace-printing")]
    pub print_traces: bool,

    /// Enables automatic impersonation on startup. This allows any transaction sender to be
    /// simulated as different accounts, which is useful for testing contract behavior.
    #[arg(long, visible_alias = "auto-unlock")]
    pub auto_impersonate: bool,

    /// Disable the default create2 deployer
    #[arg(long, visible_alias = "no-create2")]
    pub disable_default_create2_deployer: bool,

    /// The memory limit per EVM execution in bytes.
    #[arg(long)]
    pub memory_limit: Option<u64>,
}

/// Helper type to periodically dump the state of the chain to disk
struct PeriodicStateDumper {
    in_progress_dump: Option<Pin<Box<dyn Future<Output = ()> + Send + Sync + 'static>>>,
    dump_state: Option<PathBuf>,
    preserve_historical_states: bool,
    interval: Interval,
}

impl PeriodicStateDumper {
    fn new(
        dump_state: Option<PathBuf>,
        interval: Duration,
        preserve_historical_states: bool,
    ) -> Self {
        let dump_state = dump_state.map(|mut dump_state| {
            if dump_state.is_dir() {
                dump_state = dump_state.join("state.json");
            }
            dump_state
        });

        // periodically flush the state
        let interval = tokio::time::interval_at(Instant::now() + interval, interval);
        Self { in_progress_dump: None, dump_state, preserve_historical_states, interval }
    }

    async fn dump(&self) {
        if let Some(state) = self.dump_state.clone() {
            Self::dump_state(state, self.preserve_historical_states).await
        }
    }

    /// Infallible state dump
    async fn dump_state(dump_state: PathBuf, preserve_historical_states: bool) {
        // TODO
        trace!(path=?dump_state, "Dumping state. NOOP for now");
    }
}

// An endless future that periodically dumps the state to disk if configured.
impl Future for PeriodicStateDumper {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        if this.dump_state.is_none() {
            return Poll::Pending
        }

        loop {
            if let Some(mut flush) = this.in_progress_dump.take() {
                match flush.poll_unpin(cx) {
                    Poll::Ready(_) => {
                        this.interval.reset();
                    }
                    Poll::Pending => {
                        this.in_progress_dump = Some(flush);
                        return Poll::Pending
                    }
                }
            }

            if this.interval.poll_tick(cx).is_ready() {
                let path = this.dump_state.clone().expect("exists; see above");
                this.in_progress_dump =
                    Some(Box::pin(Self::dump_state(path, this.preserve_historical_states)));
            } else {
                break
            }
        }

        Poll::Pending
    }
}

/// Represents the --state flag and where to load from, or dump the state to
#[derive(Clone, Debug)]
pub struct StateFile {
    pub path: PathBuf,
    pub state: Option<SerializableState>,
}

impl StateFile {
    /// This is used as the clap `value_parser` implementation to parse from file but only if it
    /// exists
    fn parse(path: &str) -> Result<Self, String> {
        Self::parse_path(path)
    }

    /// Parse from file but only if it exists
    pub fn parse_path(path: impl AsRef<Path>) -> Result<Self, String> {
        let mut path = path.as_ref().to_path_buf();
        if path.is_dir() {
            path = path.join("state.json");
        }
        let mut state = Self { path, state: None };
        if !state.path.exists() {
            return Ok(state)
        }

        // TODO: load from file
        state.state = None;

        Ok(state)
    }
}

/// Clap's value parser for genesis. Loads a genesis.json file.
fn read_genesis_file(path: &str) -> Result<Genesis, String> {
    foundry_common::fs::read_json_file(path.as_ref()).map_err(|err| err.to_string())
}

fn duration_from_secs_f64(s: &str) -> Result<Duration, String> {
    let s = s.parse::<f64>().map_err(|e| e.to_string())?;
    if s == 0.0 {
        return Err("Duration must be greater than 0".to_string());
    }
    Duration::try_from_secs_f64(s).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{env, net::Ipv4Addr};

    #[test]
    fn can_parse_prune_config() {
        let args: NodeArgs = NodeArgs::parse_from(["anvil", "--prune-history"]);
        assert!(args.prune_history.is_some());

        let args: NodeArgs = NodeArgs::parse_from(["anvil", "--prune-history", "100"]);
        assert_eq!(args.prune_history, Some(Some(100)));
    }

    #[test]
    fn can_parse_max_persisted_states_config() {
        let args: NodeArgs = NodeArgs::parse_from(["anvil", "--max-persisted-states", "500"]);
        assert_eq!(args.max_persisted_states, (Some(500)));
    }

    #[test]
    fn can_parse_disable_block_gas_limit() {
        let args: NodeArgs = NodeArgs::parse_from(["anvil", "--disable-block-gas-limit"]);
        assert!(args.evm.disable_block_gas_limit);

        let args =
            NodeArgs::try_parse_from(["anvil", "--disable-block-gas-limit", "--gas-limit", "100"]);
        assert!(args.is_err());
    }

    #[test]
    fn can_parse_disable_code_size_limit() {
        let args: NodeArgs = NodeArgs::parse_from(["anvil", "--disable-code-size-limit"]);
        assert!(args.evm.disable_code_size_limit);

        let args = NodeArgs::try_parse_from([
            "anvil",
            "--disable-code-size-limit",
            "--code-size-limit",
            "100",
        ]);
        // can't be used together
        assert!(args.is_err());
    }

    #[test]
    fn can_parse_host() {
        let args = NodeArgs::parse_from(["anvil"]);
        assert_eq!(args.host, vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]);

        let args = NodeArgs::parse_from([
            "anvil", "--host", "::1", "--host", "1.1.1.1", "--host", "2.2.2.2",
        ]);
        assert_eq!(
            args.host,
            ["::1", "1.1.1.1", "2.2.2.2"].map(|ip| ip.parse::<IpAddr>().unwrap()).to_vec()
        );

        let args = NodeArgs::parse_from(["anvil", "--host", "::1,1.1.1.1,2.2.2.2"]);
        assert_eq!(
            args.host,
            ["::1", "1.1.1.1", "2.2.2.2"].map(|ip| ip.parse::<IpAddr>().unwrap()).to_vec()
        );

        env::set_var("ANVIL_IP_ADDR", "1.1.1.1");
        let args = NodeArgs::parse_from(["anvil"]);
        assert_eq!(args.host, vec!["1.1.1.1".parse::<IpAddr>().unwrap()]);

        env::set_var("ANVIL_IP_ADDR", "::1,1.1.1.1,2.2.2.2");
        let args = NodeArgs::parse_from(["anvil"]);
        assert_eq!(
            args.host,
            ["::1", "1.1.1.1", "2.2.2.2"].map(|ip| ip.parse::<IpAddr>().unwrap()).to_vec()
        );
    }
}
