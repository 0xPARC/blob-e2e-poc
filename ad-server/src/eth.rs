use alloy::{
    consensus::{SidecarBuilder, SimpleCoder},
    eips::eip4844::DATA_GAS_PER_BLOB,
    network::{TransactionBuilder, TransactionBuilder4844},
    primitives::{Address, TxHash},
    providers::{Provider, ProviderBuilder},
    rpc::types::TransactionRequest,
    signers::local::PrivateKeySigner,
};
use anyhow::{Result, anyhow};

use crate::Config;

pub async fn send_payload(cfg: Config, b: Vec<u8>) -> Result<TxHash> {
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

    let sidecar: SidecarBuilder<SimpleCoder> = SidecarBuilder::from_slice(&b);
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

    if receipt.from != sender {
        return Err(anyhow!(
            "receipt.from: {} != sender: {}",
            receipt.from,
            sender
        ));
    }
    let receipt_to = receipt.to.ok_or(anyhow!("expected receipt.to"))?;
    if receipt_to != receiver {
        return Err(anyhow!(
            "receipt.to: {} != receiver: {}",
            receipt_to,
            receiver
        ));
    }
    let blob_gas_used = receipt
        .blob_gas_used
        .ok_or(anyhow!("expected EIP-4844 tx"))?;
    if blob_gas_used != DATA_GAS_PER_BLOB {
        return Err(anyhow!(
            "blob_gas_used: {} != DATA_GAS_PER_BLOB: {}",
            blob_gas_used,
            DATA_GAS_PER_BLOB
        ));
    }

    Ok(tx_hash)
}
