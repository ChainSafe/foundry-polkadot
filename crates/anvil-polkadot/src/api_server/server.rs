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
use anvil_core::eth::EthRequest;
use anvil_rpc::{error::RpcError, response::ResponseResult};
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
            EthRequest::SetBalance(address, value) => {
                self.set_balance(address, value).to_rpc_result()
            }
            EthRequest::SetNonce(address, value) => self.set_nonce(address, value).to_rpc_result(),
            EthRequest::SetCode(address, bytes) => self.set_code(address, bytes).to_rpc_result(),
            EthRequest::SetStorageAt(address, key, value) => {
                self.set_storage_at(address, key, value).to_rpc_result()
            }
            EthRequest::Mine(blocks, interval) => {
                if blocks.is_some_and(|b| u64::try_from(b).is_err()) {
                    return ResponseResult::Error(RpcError::invalid_params(
                        "The number of blocks is too large",
                    ));
                }
                if interval.is_some_and(|i| u64::try_from(i).is_err()) {
                    return ResponseResult::Error(RpcError::invalid_params(
                        "The interval between blocks is too large",
                    ));
                }
                self.mining_engine
                    .mine(blocks.map(|b| b.to()), interval.map(|i| Duration::from_secs(i.to())))
                    .await
                    .to_rpc_result()
            }
            EthRequest::SetIntervalMining(interval) => self
                .mining_engine
                .set_interval_mining(Duration::from_secs(interval))
                .to_rpc_result(),
            EthRequest::GetIntervalMining(()) => {
                self.mining_engine.get_interval_mining().to_rpc_result()
            }
            EthRequest::GetAutoMine(()) => self.mining_engine.get_auto_mine().to_rpc_result(),
            EthRequest::SetAutomine(enabled) => {
                self.mining_engine.set_auto_mine(enabled).to_rpc_result()
            }
            EthRequest::EvmMine(mine) => {
                self.mining_engine.evm_mine(mine.and_then(|p| p.params)).await.to_rpc_result()
            }
            EthRequest::EvmMineDetailed(_mine) => ResponseResult::Error(RpcError::internal_error()),
            //------- TimeMachine---------
            EthRequest::EvmSetBlockTimeStampInterval(time) => self
                .mining_engine
                .set_block_timestamp_interval(Duration::from_secs(time))
                .to_rpc_result(),
            EthRequest::EvmRemoveBlockTimeStampInterval(()) => {
                self.mining_engine.remove_block_timestamp_interval().to_rpc_result()
            }
            EthRequest::EvmSetNextBlockTimeStamp(time) => {
                if time >= U256::from(u64::MAX) {
                    return ResponseResult::Error(RpcError::invalid_params(
                        "The timestamp is too big",
                    ))
                }
                let time = time.to::<u64>();
                self.mining_engine
                    .set_next_block_timestamp(Duration::from_secs(time))
                    .to_rpc_result()
            }
            EthRequest::EvmIncreaseTime(time) => self
                .mining_engine
                .increase_time(Duration::from_secs(time.try_into().unwrap_or(0)))
                .to_rpc_result(),
            EthRequest::EvmSetTime(timestamp) => {
                if timestamp >= U256::from(u64::MAX) {
                    return ResponseResult::Error(RpcError::invalid_params(
                        "The timestamp is too big",
                    ))
                }
                // Make sure here we are not traveling back in time.
                let time = timestamp.to::<u64>();
                self.mining_engine.set_time(Duration::from_secs(time)).to_rpc_result()
            }
            EthRequest::SetLogging(enabled) => {
                node_info!("anvil_setLoggingEnabled");
                self.logging_manager.set_enabled(enabled);
                ResponseResult::Success(serde_json::Value::Bool(true))
            }
            EthRequest::SetChainId(chain_id) => self.set_chain_id(chain_id),
            EthRequest::SetLogging(enabled) => self.set_logging(enabled),
            _ => Err(Error::RpcUnimplemented),
        };

        if let ResponseResult::Error(err) = &res {
            node_info!("\nRPC request failed:");
            node_info!("    Request: {:?}", res);
            node_info!("    Error: {}\n", err);
        }

        res
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

    fn set_logging(&self, enabled: bool) -> Result<()> {
        node_info!("anvil_setLoggingEnabled");
        self.logging_manager.set_enabled(enabled);
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
