use std::time::Duration;

use crate::utils::{TestNode, unwrap_response};
use alloy_primitives::{Address, U256};
use alloy_rpc_types::TransactionRequest;
use anvil_core::eth::EthRequest;
use anvil_polkadot::{
    api_server::revive_conversions::ReviveAddress,
    config::{AnvilNodeConfig, SubstrateNodeConfig},
};
use polkadot_sdk::pallet_revive::evm::Account;

/// Tests that forking preserves state from the source chain and allows local modifications
#[tokio::test(flavor = "multi_thread")]
async fn test_fork_preserves_state_and_allows_modifications() {
    // Step 1: Create the "source" node
    let source_config = AnvilNodeConfig::test_config();
    let source_substrate_config = SubstrateNodeConfig::new(&source_config);
    let mut source_node =
        TestNode::new(source_config.clone(), source_substrate_config).await.unwrap();

    let source_substrate_rpc_port = source_node.substrate_rpc_port();

    tokio::time::sleep(Duration::from_millis(1000)).await;

    let alith = Account::from(subxt_signer::eth::dev::alith());
    let baltathar = Account::from(subxt_signer::eth::dev::baltathar());
    let alith_address = ReviveAddress::new(alith.address());
    let baltathar_address = ReviveAddress::new(baltathar.address());

    // Step 2: Perform a transaction on the source node
    let initial_alith_balance = source_node.get_balance(alith.address(), None).await;
    let initial_baltathar_balance = source_node.get_balance(baltathar.address(), None).await;

    let transfer_amount = U256::from_str_radix("5000000000000000000", 10).unwrap(); // 5 ether
    let transaction = TransactionRequest::default()
        .value(transfer_amount)
        .from(Address::from(alith_address))
        .to(Address::from(baltathar_address));

    source_node.send_transaction(transaction, None).await.unwrap();
    unwrap_response::<()>(source_node.eth_rpc(EthRequest::Mine(None, None)).await.unwrap())
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify the transfer happened on source node
    let source_alith_balance = source_node.get_balance(alith.address(), None).await;
    let source_baltathar_balance = source_node.get_balance(baltathar.address(), None).await;

    assert!(
        source_alith_balance < initial_alith_balance, // This is to not check the gas fee
        "Alith balance should decrease on source node"
    );
    assert_eq!(
        source_baltathar_balance,
        initial_baltathar_balance + transfer_amount,
        "Baltathar should receive 5 ether on source node"
    );

    // Step 3: Create a forked node pointing to the source node's Substrate RPC
    let source_rpc_url = format!("http://127.0.0.1:{}", source_substrate_rpc_port);

    let fork_config =
        AnvilNodeConfig::test_config().with_port(0).with_eth_rpc_url(Some(source_rpc_url));

    let fork_substrate_config = SubstrateNodeConfig::new(&fork_config);
    let mut fork_node = TestNode::new(fork_config.clone(), fork_substrate_config).await.unwrap();

    // Step 4: Verify the forked node has the same balances as the source node at fork point
    let fork_alith_balance = fork_node.get_balance(alith.address(), None).await;
    let fork_baltathar_balance = fork_node.get_balance(baltathar.address(), None).await;

    assert_eq!(
        fork_alith_balance, source_alith_balance,
        "Forked node should have same Alith balance as source"
    );
    assert_eq!(
        fork_baltathar_balance, source_baltathar_balance,
        "Forked node should have same Baltathar balance as source"
    );

    // Step 5: Perform a transaction on the forked node (send 1 ether)
    let fork_transfer_amount = U256::from_str_radix("1000000000000000000", 10).unwrap(); // 1 ether
    let fork_transaction = TransactionRequest::default()
        .value(fork_transfer_amount)
        .from(Address::from(alith_address))
        .to(Address::from(baltathar_address));

    fork_node.send_transaction(fork_transaction, None).await.unwrap();
    unwrap_response::<()>(fork_node.eth_rpc(EthRequest::Mine(None, None)).await.unwrap()).unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Step 6: Verify the transaction affected only the forked node
    let fork_alith_balance_after = fork_node.get_balance(alith.address(), None).await;
    let fork_baltathar_balance_after = fork_node.get_balance(baltathar.address(), None).await;

    assert!(
        fork_alith_balance_after < fork_alith_balance,
        "Alith balance should decrease on forked node"
    );
    assert_eq!(
        fork_baltathar_balance_after,
        fork_baltathar_balance + fork_transfer_amount,
        "Baltathar should receive 1 ether on forked node"
    );

    // Step 7: Verify the source node was NOT affected by the forked node's transaction
    let source_alith_balance_final = source_node.get_balance(alith.address(), None).await;
    let source_baltathar_balance_final = source_node.get_balance(baltathar.address(), None).await;

    assert_eq!(
        source_alith_balance_final, source_alith_balance,
        "Source node Alith balance should remain unchanged"
    );
    assert_eq!(
        source_baltathar_balance_final, source_baltathar_balance,
        "Source node Baltathar balance should remain unchanged"
    );
}

/// Tests that forking creates a new chain starting from the latest finalized block of the source chain
#[tokio::test(flavor = "multi_thread")]
async fn test_fork_from_latest_finalized_block() {
    const BLOCKS_TO_MINE: u32 = 5;
    // Step 1: Create the "source" node
    let source_config = AnvilNodeConfig::test_config();
    let source_substrate_config = SubstrateNodeConfig::new(&source_config);
    let mut source_node =
        TestNode::new(source_config.clone(), source_substrate_config).await.unwrap();

    let source_substrate_rpc_port = source_node.substrate_rpc_port();

    tokio::time::sleep(Duration::from_millis(1000)).await;

    let alith = Account::from(subxt_signer::eth::dev::alith());
    let baltathar = Account::from(subxt_signer::eth::dev::baltathar());
    let alith_address = ReviveAddress::new(alith.address());
    let baltathar_address = ReviveAddress::new(baltathar.address());

    // Step 2: Mine several blocks on the source node to create history
    let transfer_amount = U256::from_str_radix("1000000000000000000", 10).unwrap(); // 1 ether

    // Mine 3 blocks with transfers
    for _ in 0..BLOCKS_TO_MINE {
        let transaction = TransactionRequest::default()
            .value(transfer_amount)
            .from(Address::from(alith_address))
            .to(Address::from(baltathar_address));
        source_node.send_transaction(transaction, None).await.unwrap();
        unwrap_response::<()>(source_node.eth_rpc(EthRequest::Mine(None, None)).await.unwrap())
            .unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // Get the current block number from source (this will be the fork point)
    let source_best_block = source_node.best_block_number().await;
    let source_best_block_hash =
        source_node.eth_block_hash_by_number(source_best_block).await.unwrap();

    // Verify the source node has mined the correct number of blocks
    assert_eq!(source_best_block, BLOCKS_TO_MINE, "Source node should have mined 5 blocks");

    // Step 3: Create a forked node from the latest finalized block of the source chain
    let source_rpc_url = format!("http://127.0.0.1:{}", source_substrate_rpc_port);

    let fork_config = AnvilNodeConfig::test_config().with_eth_rpc_url(Some(source_rpc_url));

    let fork_substrate_config = SubstrateNodeConfig::new(&fork_config);
    let mut fork_node = TestNode::new(fork_config.clone(), fork_substrate_config).await.unwrap();

    // Wait for fork node to be ready
    tokio::time::sleep(Duration::from_millis(1000)).await;

    // Step 4: Verify the forked node starts from the same block as source best block
    let fork_initial_block_number = fork_node.best_block_number().await;
    assert_eq!(
        fork_initial_block_number, source_best_block,
        "Forked node should start from source's latest finalized block"
    );

    // Step 5: Verify the genesis block hash of the fork matches the source
    let fork_genesis_hash =
        fork_node.eth_block_hash_by_number(fork_initial_block_number).await.unwrap();
    assert_eq!(
        fork_genesis_hash, source_best_block_hash,
        "Forked node's initial block hash should match source block hash"
    );

    // Step 6: Mine a new block on the forked node
    let fork_transaction = TransactionRequest::default()
        .value(transfer_amount)
        .from(Address::from(alith_address))
        .to(Address::from(baltathar_address));
    fork_node.send_transaction(fork_transaction, None).await.unwrap();
    unwrap_response::<()>(fork_node.eth_rpc(EthRequest::Mine(None, None)).await.unwrap()).unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Step 7: Verify the forked node advanced by 1 block
    let fork_current_block = fork_node.best_block_number().await;
    assert_eq!(
        fork_current_block,
        fork_initial_block_number + 1,
        "Forked node should have advanced by 1 block after mining"
    );

    // Step 8: Verify that the source node is still at the same block (unchanged by fork)
    let source_current_block = source_node.best_block_number().await;
    assert_eq!(
        source_current_block, source_best_block,
        "Source node should remain at the same block"
    );

    // Step 9: Verify the block hashes diverged after the fork point
    let fork_new_block_hash = fork_node.eth_block_hash_by_number(fork_current_block).await.unwrap();

    // Mine one more block on source to create a divergence
    let source_transaction = TransactionRequest::default()
        .value(transfer_amount)
        .from(Address::from(alith_address))
        .to(Address::from(baltathar_address));
    source_node.send_transaction(source_transaction, None).await.unwrap();
    unwrap_response::<()>(source_node.eth_rpc(EthRequest::Mine(None, None)).await.unwrap())
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    let source_new_block = source_node.best_block_number().await;
    let source_new_block_hash =
        source_node.eth_block_hash_by_number(source_new_block).await.unwrap();

    // Both chains should be at the same height now
    assert_eq!(source_new_block, fork_current_block, "Both chains should be at the same height");

    // But the hashes should be different (chains diverged)
    assert_ne!(
        fork_new_block_hash, source_new_block_hash,
        "Block hashes should be different (chains diverged after fork point)"
    );
}
