use crate::{
    api_server::{
        error::{Error, Result, ToRpcResponseResult},
        ApiRequest,
    },
    logging::LoggingManager,
    macros::node_info,
    substrate_node::{
        mining_engine::MiningEngine,
        service::{
            storage::{AccountInfo, AccountType},
            BackendWithOverlay, Client, Service,
        },
    },
};
use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_rpc_types::anvil::MineOptions;
use anvil_core::eth::{EthRequest, Params};
use anvil_rpc::response::ResponseResult;
use codec::Encode;
use futures::{channel::mpsc, StreamExt};
use polkadot_sdk::{
    pallet_revive::ReviveApi,
    pallet_revive_eth_rpc::subxt_client::{
        runtime_types::{
            bounded_collections::bounded_vec::BoundedVec, pallet_revive::vm::CodeInfo,
        },
        src_chain::runtime_types::pallet_revive::storage::ContractInfo,
    },
    parachains_common::{AccountId, Hash},
    sc_client_api::HeaderBackend,
    sc_service::RpcHandlers,
    sp_api::ProvideRuntimeApi,
    sp_core::{Hasher, H160, H256},
    sp_io,
    sp_runtime::traits::BlakeTwo256,
};
use std::{sync::Arc, time::Duration};
use substrate_runtime::Balance;

pub struct ApiServer {
    req_receiver: mpsc::Receiver<ApiRequest>,
    backend: BackendWithOverlay,
    logging_manager: LoggingManager,
    rpc: RpcHandlers,
    client: Arc<Client>,
    mining_engine: Arc<MiningEngine>,
}

impl ApiServer {
    pub fn new(
        substrate_service: &Service,
        req_receiver: mpsc::Receiver<ApiRequest>,
        logging_manager: LoggingManager,
    ) -> Self {
        Self {
            req_receiver,
            logging_manager,
            backend: BackendWithOverlay::new(
                substrate_service.backend.clone(),
                substrate_service.storage_overrides.clone(),
            ),
            rpc: substrate_service.rpc_handlers.clone(),
            client: substrate_service.client.clone(),
            mining_engine: substrate_service.mining_engine.clone(),
        }
    }

    pub async fn run(mut self) {
        while let Some(msg) = self.req_receiver.next().await {
            let resp = self.execute(msg.req).await;

            msg.resp_sender.send(resp).expect("Dropped receiver");
        }
    }

    pub async fn execute(&mut self, req: EthRequest) -> ResponseResult {
        let res = match req.clone() {
            EthRequest::Mine(blocks, interval) => self.mine(blocks, interval).await.to_rpc_result(),
            EthRequest::SetIntervalMining(interval) => {
                self.set_interval_mining(interval).to_rpc_result()
            }
            EthRequest::GetIntervalMining(()) => self.get_interval_mining().to_rpc_result(),
            EthRequest::GetAutoMine(()) => self.get_auto_mine().to_rpc_result(),
            EthRequest::SetAutomine(enabled) => self.set_auto_mine(enabled).to_rpc_result(),
            EthRequest::EvmMine(mine) => self.evm_mine(mine).await.to_rpc_result(),
            //------- TimeMachine---------
            EthRequest::EvmSetBlockTimeStampInterval(time) => {
                self.set_block_timestamp_interval(time).to_rpc_result()
            }
            EthRequest::EvmRemoveBlockTimeStampInterval(()) => {
                self.remove_block_timestamp_interval().to_rpc_result()
            }
            EthRequest::EvmSetNextBlockTimeStamp(time) => {
                self.set_next_block_timestamp(time).to_rpc_result()
            }
            EthRequest::EvmIncreaseTime(time) => self.increase_time(time).to_rpc_result(),
            EthRequest::EvmSetTime(timestamp) => self.set_time(timestamp).to_rpc_result(),
            EthRequest::SetLogging(enabled) => self.set_logging(enabled).to_rpc_result(),
            EthRequest::SetBalance(address, value) => {
                self.set_balance(address, value).to_rpc_result()
            }
            EthRequest::SetNonce(address, value) => self.set_nonce(address, value).to_rpc_result(),
            EthRequest::SetCode(address, bytes) => self.set_code(address, bytes).to_rpc_result(),
            EthRequest::SetStorageAt(address, key, value) => {
                self.set_storage_at(address, key, value).to_rpc_result()
            }
            EthRequest::SetChainId(chain_id) => self.set_chain_id(chain_id).to_rpc_result(),
            _ => Err::<(), _>(Error::RpcUnimplemented).to_rpc_result(),
        };

        if let ResponseResult::Error(err) = &res {
            node_info!("\nRPC request failed:");
            node_info!("    Request: {:?}", req);
            node_info!("    Error: {}\n", err);
        }

        res
    }

    async fn mine(&self, blocks: Option<U256>, interval: Option<U256>) -> Result<()> {
        node_info!("anvil_mine");

        if blocks.is_some_and(|b| u64::try_from(b).is_err()) {
            return Err(Error::InvalidParams("The number of blocks is too large".to_string()))
        }
        if interval.is_some_and(|i| u64::try_from(i).is_err()) {
            return Err(Error::InvalidParams("The interval between blocks is too large".to_string()))
        }
        self.mining_engine
            .mine(blocks.map(|b| b.to()), interval.map(|i| Duration::from_secs(i.to())))
            .await
            .map_err(Error::Mining)
    }

    fn set_interval_mining(&self, interval: u64) -> Result<()> {
        node_info!("evm_setIntervalMining");

        self.mining_engine.set_interval_mining(Duration::from_secs(interval));
        Ok(())
    }

    fn get_interval_mining(&self) -> Result<Option<u64>> {
        node_info!("anvil_getIntervalMining");

        Ok(self.mining_engine.get_interval_mining())
    }

    fn get_auto_mine(&self) -> Result<bool> {
        node_info!("anvil_getAutomine");

        Ok(self.mining_engine.is_automine())
    }

    fn set_auto_mine(&self, enabled: bool) -> Result<()> {
        node_info!("evm_setAutomine");

        self.mining_engine.set_auto_mine(enabled);
        Ok(())
    }

    async fn evm_mine(&self, mine: Option<Params<Option<MineOptions>>>) -> Result<String> {
        node_info!("evm_mine");

        self.mining_engine.evm_mine(mine.and_then(|p| p.params)).await?;
        Ok("0x0".to_string())
    }

    fn set_block_timestamp_interval(&self, time: u64) -> Result<()> {
        node_info!("anvil_setBlockTimestampInterval");

        self.mining_engine.set_block_timestamp_interval(Duration::from_secs(time));
        Ok(())
    }

    fn remove_block_timestamp_interval(&self) -> Result<bool> {
        node_info!("anvil_removeBlockTimestampInterval");

        Ok(self.mining_engine.remove_block_timestamp_interval())
    }

    fn set_next_block_timestamp(&self, time: U256) -> Result<()> {
        node_info!("anvil_setBlockTimestampInterval");

        if time >= U256::from(u64::MAX) {
            return Err(Error::InvalidParams("The timestamp is too big".to_string()))
        }
        let time = time.to::<u64>();
        self.mining_engine
            .set_next_block_timestamp(Duration::from_secs(time))
            .map_err(Error::Mining)
    }

    fn increase_time(&self, time: U256) -> Result<i64> {
        node_info!("evm_increaseTime");

        Ok(self.mining_engine.increase_time(Duration::from_secs(time.try_into().unwrap_or(0))))
    }

    fn set_time(&self, timestamp: U256) -> Result<u64> {
        node_info!("evm_setTime");

        if timestamp >= U256::from(u64::MAX) {
            return Err(Error::InvalidParams("The timestamp is too big".to_string()))
        }
        let time = timestamp.to::<u64>();
        Ok(self.mining_engine.set_time(Duration::from_secs(time)))
    }

    fn set_logging(&self, enabled: bool) -> Result<()> {
        node_info!("anvil_setLoggingEnabled");

        self.logging_manager.set_enabled(enabled);
        Ok(())
    }

    fn set_balance(&self, address: Address, value: U256) -> Result<()> {
        node_info!("anvil_setBalance");

        let latest_block = self.backend.blockchain().info().best_hash;

        let account_id = self.get_account_id(latest_block, address);
        let mut balance_data =
            self.backend.read_balance(latest_block, account_id.clone())?.unwrap_or_default();
        let total_issuance = self.backend.read_total_issuance(latest_block)?;
        let (new_balance, dust) = self.construct_balance_with_dust(latest_block, value);

        let diff = new_balance as i128 - (balance_data.total() as i128);

        if diff < 0 {
            let diff = diff.abs() as Balance;

            balance_data.free = balance_data.free.saturating_sub(diff);
            self.backend.inject_balance(latest_block, account_id, balance_data);
            self.backend.inject_total_issuance(latest_block, total_issuance.saturating_sub(diff));
        } else if diff > 0 {
            let diff = diff.abs() as Balance;

            balance_data.free = balance_data.free.saturating_add(diff);
            self.backend.inject_balance(latest_block, account_id, balance_data);
            self.backend.inject_total_issuance(latest_block, total_issuance.saturating_add(diff));
        }

        let mut account_info = self
            .backend
            .read_account_info(latest_block, address)?
            .unwrap_or_else(|| AccountInfo { account_type: AccountType::EOA, dust: 0 });

        if account_info.dust != dust {
            account_info.dust = dust;

            self.backend.inject_account_info(latest_block, address, account_info);
        }

        Ok(())
    }

    fn set_nonce(&self, address: Address, value: U256) -> Result<()> {
        node_info!("anvil_setNonce");

        let latest_block = self.backend.blockchain().info().best_hash;

        let account_id = self.get_account_id(latest_block, address);

        self.backend.inject_nonce(
            latest_block,
            account_id,
            value.try_into().map_err(|_| Error::NonceOverflow)?,
        );

        Ok(())
    }

    fn set_storage_at(&self, address: Address, key: U256, value: B256) -> Result<()> {
        let latest_block = self.backend.blockchain().info().best_hash;

        let Some(AccountInfo { account_type: AccountType::Contract(contract_info), .. }) =
            self.backend.read_account_info(latest_block, address)?
        else {
            return Ok(())
        };
        let trie_id = contract_info.trie_id.0;

        self.backend.inject_child_storage(
            latest_block,
            trie_id,
            key.to_be_bytes_vec(),
            value.to_vec(),
        );

        Ok(())
    }

    fn set_code(&self, address: Address, bytes: Bytes) -> Result<()> {
        node_info!("anvil_setCode");

        let latest_block = self.backend.blockchain().info().best_hash;

        let account_id = self.get_account_id(latest_block, address);

        let code_hash = H256(sp_io::hashing::keccak_256(&bytes));

        let account_info = match self.backend.read_account_info(latest_block, address)? {
            None => {
                let contract_info = new_contract_info(&address, code_hash);

                AccountInfo { account_type: AccountType::Contract(contract_info), dust: 0 }
            }
            Some(AccountInfo { account_type: AccountType::Contract(mut contract_info), dust }) => {
                if contract_info.code_hash != code_hash {
                    // Remove the pristine code and code info for the old hash.
                    self.backend.inject_pristine_code(latest_block, contract_info.code_hash, None);
                    self.backend.inject_code_info(latest_block, contract_info.code_hash, None);
                }

                contract_info.code_hash = code_hash;

                AccountInfo { account_type: AccountType::Contract(contract_info), dust }
            }
            Some(AccountInfo { account_type: AccountType::EOA, dust }) => {
                let contract_info = new_contract_info(&address, code_hash);

                AccountInfo { account_type: AccountType::Contract(contract_info), dust }
            }
        };

        self.backend.inject_account_info(latest_block, address, account_info);

        let code_info = CodeInfo {
            owner: <[u8; 32]>::from(account_id).into(),
            deposit: 0,
            refcount: 0,
            code_len: bytes.len() as u32,
            behaviour_version: 0,
        };

        self.backend.inject_pristine_code(latest_block, code_hash, Some(bytes));
        self.backend.inject_code_info(latest_block, code_hash, Some(code_info));

        Ok(())
    }

    fn set_chain_id(&self, chain_id: u64) -> Result<()> {
        node_info!("anvil_setChainId");

        let latest_block = self.backend.blockchain().info().best_hash;
        self.backend.inject_chain_id(latest_block, chain_id);

        Ok(())
    }

    // ----- Helpers

    fn get_account_id(&self, block: Hash, address: Address) -> AccountId {
        self.client.runtime_api().account_id(block, address).unwrap()
    }

    fn construct_balance_with_dust(&self, block: Hash, value: U256) -> (Balance, u32) {
        self.client.runtime_api().new_balance_with_dust(block, value).unwrap()
    }
}

fn new_contract_info(address: &Address, code_hash: H256) -> ContractInfo {
    let address = H160::from_slice(address.as_slice());

    let trie_id = {
        let buf = ("bcontract_trie_v1", address, 0).using_encoded(BlakeTwo256::hash);
        buf.as_ref().to_vec()
    };

    ContractInfo {
        trie_id: BoundedVec::<u8>(trie_id),
        code_hash,
        storage_bytes: 0,
        storage_items: 0,
        storage_byte_deposit: 0,
        storage_item_deposit: 0,
        storage_base_deposit: 0,
        immutable_data_len: 0,
    }
}
