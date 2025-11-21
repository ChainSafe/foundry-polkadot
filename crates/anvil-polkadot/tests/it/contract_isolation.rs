use std::time::Duration;

use crate::{
    abi::SimpleStorage,
    utils::{TestNode, call_get_value, get_contract_code, unwrap_response},
};
use alloy_primitives::{Address, U256};
use alloy_rpc_types::{TransactionInput, TransactionRequest};
use alloy_sol_types::SolCall;
use anvil_core::eth::EthRequest;
use anvil_polkadot::{
    api_server::revive_conversions::ReviveAddress,
    config::{AnvilNodeConfig, SubstrateNodeConfig},
};
use polkadot_sdk::pallet_revive::evm::Account;

/// Tests that multiple contract instances maintain independent state
#[tokio::test(flavor = "multi_thread")]
async fn test_multiple_contract_instances_independent_storage() {
    let anvil_node_config = AnvilNodeConfig::test_config();
    let substrate_node_config = SubstrateNodeConfig::new(&anvil_node_config);
    let mut node = TestNode::new(anvil_node_config.clone(), substrate_node_config).await.unwrap();

    let alith = Account::from(subxt_signer::eth::dev::alith());
    let alith_address = ReviveAddress::new(alith.address());
    let contract_code = get_contract_code("SimpleStorage");

    // Deploy 3 instances of SimpleStorage contract (nonces 0, 1, 2)
    let contract1_tx = node.deploy_contract(&contract_code.init, alith.address()).await;
    unwrap_response::<()>(node.eth_rpc(EthRequest::Mine(None, None)).await.unwrap()).unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    let contract2_tx = node.deploy_contract(&contract_code.init, alith.address()).await;
    unwrap_response::<()>(node.eth_rpc(EthRequest::Mine(None, None)).await.unwrap()).unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    let contract3_tx = node.deploy_contract(&contract_code.init, alith.address()).await;
    unwrap_response::<()>(node.eth_rpc(EthRequest::Mine(None, None)).await.unwrap()).unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    let receipt1 = node.get_transaction_receipt(contract1_tx).await;
    let contract1_address = receipt1.contract_address.unwrap();
    let receipt2 = node.get_transaction_receipt(contract2_tx).await;
    let contract2_address = receipt2.contract_address.unwrap();
    let receipt3 = node.get_transaction_receipt(contract3_tx).await;
    let contract3_address = receipt3.contract_address.unwrap();

    // Verify all contracts are deployed successfully
    assert_eq!(receipt1.status, Some(polkadot_sdk::pallet_revive::U256::from(1)));
    assert_eq!(receipt2.status, Some(polkadot_sdk::pallet_revive::U256::from(1)));
    assert_eq!(receipt3.status, Some(polkadot_sdk::pallet_revive::U256::from(1)));

    // Set different values for each contract (Contract 1: 100, Contract 2: 200, Contract 3: 300)
    let set_value1 = SimpleStorage::setValueCall::new((U256::from(100),)).abi_encode();
    let call_tx1 = TransactionRequest::default()
        .from(Address::from(alith_address))
        .to(Address::from(ReviveAddress::new(contract1_address)))
        .input(TransactionInput::both(set_value1.into()));
    node.send_transaction(call_tx1).await.unwrap();
    unwrap_response::<()>(node.eth_rpc(EthRequest::Mine(None, None)).await.unwrap()).unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    let set_value2 = SimpleStorage::setValueCall::new((U256::from(200),)).abi_encode();
    let call_tx2 = TransactionRequest::default()
        .from(Address::from(alith_address))
        .to(Address::from(ReviveAddress::new(contract2_address)))
        .input(TransactionInput::both(set_value2.into()));
    node.send_transaction(call_tx2).await.unwrap();
    unwrap_response::<()>(node.eth_rpc(EthRequest::Mine(None, None)).await.unwrap()).unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    let set_value3 = SimpleStorage::setValueCall::new((U256::from(300),)).abi_encode();
    let call_tx3 = TransactionRequest::default()
        .from(Address::from(alith_address))
        .to(Address::from(ReviveAddress::new(contract3_address)))
        .input(TransactionInput::both(set_value3.into()));
    node.send_transaction(call_tx3).await.unwrap();
    unwrap_response::<()>(node.eth_rpc(EthRequest::Mine(None, None)).await.unwrap()).unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify each contract maintains its own independent storage by calling getValue()
    let value1 = call_get_value(&mut node, contract1_address, Address::from(alith_address)).await;
    let value2 = call_get_value(&mut node, contract2_address, Address::from(alith_address)).await;
    let value3 = call_get_value(&mut node, contract3_address, Address::from(alith_address)).await;

    assert_eq!(value1, U256::from(100), "Contract 1 should have value 100");
    assert_eq!(value2, U256::from(200), "Contract 2 should have value 200");
    assert_eq!(value3, U256::from(300), "Contract 3 should have value 300");

    // Update contract 2's value to 999 and verify others are unaffected
    let update_value2 = SimpleStorage::setValueCall::new((U256::from(999),)).abi_encode();
    let update_tx2 = TransactionRequest::default()
        .from(Address::from(alith_address))
        .to(Address::from(ReviveAddress::new(contract2_address)))
        .input(TransactionInput::both(update_value2.into()));
    node.send_transaction(update_tx2).await.unwrap();
    unwrap_response::<()>(node.eth_rpc(EthRequest::Mine(None, None)).await.unwrap()).unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify only contract 2 changed
    let value1_after = call_get_value(&mut node, contract1_address, Address::from(alith_address)).await;
    let value2_after = call_get_value(&mut node, contract2_address, Address::from(alith_address)).await;
    let value3_after = call_get_value(&mut node, contract3_address, Address::from(alith_address)).await;

    assert_eq!(value1_after, U256::from(100), "Contract 1 value should remain 100");
    assert_eq!(value2_after, U256::from(999), "Contract 2 value should be updated to 999");
    assert_eq!(value3_after, U256::from(300), "Contract 3 value should remain 300");
}
