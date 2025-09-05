use super::ApiRequest;
use crate::{
    logging::LoggingManager,
    macros::node_info,
    substrate_node::service::{Backend, BackendWithOverlay, Client, Service, StorageOverrides},
};
use alloy_primitives::U256;
use anvil_core::eth::EthRequest;
use anvil_rpc::{error::RpcError, response::ResponseResult};
use codec::Decode;
use futures::{channel::mpsc, StreamExt};
use parking_lot::Mutex;
use polkadot_sdk::{
    pallet_revive::ReviveApi,
    parachains_common::{AccountId, Hash},
    sc_client_api::{Backend as _, HeaderBackend},
    sc_service::RpcHandlers,
    sp_api::ProvideRuntimeApi,
};
use serde_json::json;
use std::sync::Arc;
use substrate_runtime::Address;

pub struct ApiServer {
    req_receiver: mpsc::Receiver<ApiRequest>,
    backend: BackendWithOverlay,
    logging_manager: LoggingManager,
    rpc: RpcHandlers,
    client: Arc<Client>,
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
        }
    }

    pub async fn run(mut self) {
        while let Some(msg) = self.req_receiver.next().await {
            let resp = self.execute(msg.req).await;

            msg.resp_sender.send(resp).expect("Dropped receiver");
        }
    }

    pub async fn execute(&mut self, req: EthRequest) -> ResponseResult {
        match req {
            EthRequest::SetBalance(address, value) => {
                let latest_block = self.backend.blockchain().info().best_hash;

                let account_id = self.get_account_id(latest_block, address);
                let mut balance_data =
                    self.backend.read_balance(latest_block, account_id).unwrap_or_default();
                let total_issuance = self.backend.read_total_issuance(latest_block);
                let (new_balance, dust) = self.construct_balance_with_dust(latest_block, value);

                let diff = new_balance as i128 - (balance_data.total() as i128);

                if diff < 0 {
                    let diff = diff.abs() as Balance;

                    balance_data.free.saturating_sub(diff);
                    self.backend.inject_balance(latest_block, account_id, balance_data);
                    self.backend
                        .inject_total_issuance(latest_block, total_issuance.saturating_sub(diff));
                } else if diff > 0 {
                    let diff = diff.abs() as Balance;

                    balance_data.free.saturating_add(diff);
                    self.backend.inject_balance(latest_block, account_id, balance_data);
                    self.backend
                        .inject_total_issuance(latest_block, total_issuance.saturating_add(diff));
                }

                let mut account_info = self.backend.read_account_info(latest_block, address);

                if account_info.dust != dust {
                    account_info.dust = dust;

                    self.backend.inject_account_info(latest_block, address, account_info);
                }

                ResponseResult::Success(serde_json::Value::Null)
            }
            EthRequest::SetNonce(address, value) => {
                let latest_block = self.backend.blockchain().info().best_hash;

                let account_id = self.get_account_id(latest_block, address);

                self.backend.inject_nonce(latest_block, account_id, value.try_into().unwrap());

                ResponseResult::Success(serde_json::Value::Null)
            }
            EthRequest::SetCode(address, bytes) => {
                let latest_block = self.backend.blockchain().info().best_hash;

                let account_id = self.get_account_id(latest_block, address);

                let code_hash = H256(sp_io::hashing::keccak_256(&bytes));
                let account_info = self.backend.read_account_info(latest_block, address).unwrap_or_else(|| {
                    let contract_info = ContractInfo::new(&address, 0u32.into(), code_hash);

                    AccountInfo { AccountType::Contract(info), dust: 0 }
                });

                // if account info type is not contract, return
                self.backend.inject_account_info(latest_block, address, account_info);

                Bytecode::new_raw_checked(Bytes::from(code.to_vec())).unwrap();

                let code_info = CodeInfo {
                    owner: account_id,
                    deposit: 0,
                    refcount: 0,
                    code_len: bytes.len(),
                    code_type: BytecodeType::Evm,
                    behaviour_version: Default::default(),
                };

                // inject_pristine_code(bytes);
                // inject_code_info(code_info);

                ResponseResult::Success(serde_json::Value::Null)
            }
            EthRequest::SetStorageAt(address, key, value) => {
                let latest_block = self.backend.blockchain().info().best_hash;

                let account_info = self.backend.read_account_info(hash, address).unwrap();
                // get child trie id.
                // Add the child trie to the storage overrides.
                ResponseResult::Success(serde_json::Value::Null)
            }
            EthRequest::SetChainId(chain_id) => {
                let latest_block = self.backend.blockchain().info().best_hash;

                self.backend.inject_chain_id(latest_block, chain_id);

                ResponseResult::Success(serde_json::Value::Null)
            }
            EthRequest::SetLogging(enabled) => {
                node_info!("anvil_setLoggingEnabled");
                self.logging_manager.set_enabled(enabled);
                ResponseResult::Success(serde_json::Value::Bool(true))
            }
            _ => ResponseResult::Error(RpcError::internal_error()),
        }
    }

    fn get_account_id(&self, block: Hash, address: Address) -> AccountId {
        self.client.runtime_api().account_id(block, address).unwrap()
    }

    fn construct_balance_with_dust(&self, block: Hash, value: U256) -> (Balance, u32) {
        self.client.runtime_api().new_balance_with_dust(block, value).unwrap()
    }
}
