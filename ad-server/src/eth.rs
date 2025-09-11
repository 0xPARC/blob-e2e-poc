use alloy::{
    consensus::{SidecarBuilder, SimpleCoder},
    eips::eip4844::DATA_GAS_PER_BLOB,
    network::{TransactionBuilder, TransactionBuilder4844},
    primitives::{Address, TxHash},
    providers::{Provider, ProviderBuilder},
    rpc::types::TransactionRequest,
    signers::local::PrivateKeySigner,
};
use anyhow::Result;

use crate::Config;

pub async fn send_pod_proof(cfg: Config, compressed_proof_bytes: Vec<u8>) -> Result<TxHash> {
    if cfg.priv_key.is_empty() {
        // test mode, return a mock tx_hash
        return Ok(TxHash::from([0u8; 32]));
    }
    // PART 2: send the pod2 proof into a tx blob
    let signer: PrivateKeySigner = cfg.priv_key.parse()?;
    let provider = ProviderBuilder::new()
        .wallet(signer.clone())
        .connect(&cfg.rpc_url)
        .await?;

    let latest_block = provider.get_block_number().await?;
    println!("Latest block number: {latest_block}");

    let sender = signer.address();
    let receiver = Address::from([0x42; 20]);
    dbg!(&sender);
    dbg!(&receiver);

    let sidecar: SidecarBuilder<SimpleCoder> = SidecarBuilder::from_slice(&compressed_proof_bytes);
    let sidecar = sidecar.build()?;

    let tx = TransactionRequest::default()
        .with_to(receiver)
        .with_blob_sidecar(sidecar);

    let pending_tx = provider.send_transaction(tx).await?;

    let tx_hash = *pending_tx.tx_hash();
    println!("Pending transaction... tx hash: {}", tx_hash);

    let receipt = pending_tx.get_receipt().await?;

    println!(
        "Transaction included in block {}",
        receipt.block_number.expect("Failed to get block number")
    );

    assert_eq!(receipt.from, sender);
    assert_eq!(receipt.to, Some(receiver));
    assert_eq!(
        receipt
            .blob_gas_used
            .expect("Expected to be EIP-4844 transaction"),
        DATA_GAS_PER_BLOB
    );

    Ok(tx_hash)
}
