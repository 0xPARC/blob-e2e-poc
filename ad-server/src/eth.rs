use alloy::{
    consensus::{SidecarBuilder, SimpleCoder},
    eips::{eip1559::Eip1559Estimation, eip4844::DATA_GAS_PER_BLOB},
    network::{TransactionBuilder, TransactionBuilder4844},
    primitives::{Address, TxHash},
    providers::{Provider, ProviderBuilder},
    rpc::types::{TransactionReceipt, TransactionRequest},
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

    let fees = provider.estimate_eip1559_fees().await?;
    let blob_base_fee = provider.get_blob_base_fee().await?;

    // for a new tx, increase gas price by 10% to reduce the chances of the
    // nodes rejecting it (in practice increase it by 11% to ensure it passes
    // the miner filter)
    let (receipt, tx_hash) = send_tx(
        cfg,
        provider,
        receiver,
        sidecar,
        fees,
        blob_base_fee,
        111 / 100,
    )
    .await?;

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

use async_recursion::async_recursion;
#[async_recursion]
async fn send_tx<'async_recursion>(
    cfg: Config,
    provider: impl alloy::providers::Provider + 'async_recursion + 'static,
    receiver: Address,
    sidecar: alloy::eips::eip4844::BlobTransactionSidecar,
    fees: Eip1559Estimation,
    blob_base_fee: u128,
    increase_ratio: u128,
) -> Result<(TransactionReceipt, TxHash)> {
    let tx = TransactionRequest::default()
        .with_max_fee_per_gas(fees.max_fee_per_gas * increase_ratio)
        .with_max_priority_fee_per_gas(fees.max_priority_fee_per_gas * increase_ratio)
        .with_max_fee_per_blob_gas(blob_base_fee * increase_ratio)
        .with_to(receiver)
        .with_blob_sidecar(sidecar.clone());

    dbg!(&tx.max_fee_per_gas.unwrap());
    dbg!(&tx.max_priority_fee_per_gas.unwrap());
    dbg!(&tx.max_fee_per_blob_gas.unwrap());

    let send_tx_result = provider.send_transaction(tx).await;
    let pending_tx_result = match send_tx_result {
        Ok(pending_tx_result) => pending_tx_result,
        Err(e) => {
            if e.to_string().contains("Too Many Requests") {
                // NOTE: this assumes we're using infura for the rpc_url
                panic!("error: {}", e);
            }

            println!("tx err: {}", e);
            println!("sending tx again with 2x gas price");
            return send_tx(
                cfg,
                provider,
                receiver,
                sidecar,
                fees,
                blob_base_fee,
                increase_ratio * 2,
            )
            .await;
        }
    };
    println!("watching pending tx, timeout of {}", cfg.tx_watch_timeout);
    let pending_tx_result = pending_tx_result
        .with_timeout(Some(std::time::Duration::from_secs(cfg.tx_watch_timeout)))
        .watch()
        .await;

    dbg!("sent");
    let tx_hash = match pending_tx_result {
        Ok(pending_tx) => pending_tx,
        Err(e) => {
            if e.to_string().contains("Too Many Requests") {
                panic!("error: {}", e);
            }

            println!("tx err: {}", e);
            println!("sending tx again with 2x gas price");
            return send_tx(
                cfg,
                provider,
                receiver,
                sidecar,
                fees,
                blob_base_fee,
                increase_ratio * 2,
            )
            .await;
        }
    };
    println!("Pending transaction... tx hash: {}", tx_hash);

    let receipt = match provider.get_transaction_receipt(tx_hash).await? {
        Some(receipt) => receipt,
        None => {
            println!("get_transaction_receipt failed, resending tx");
            return send_tx(
                cfg,
                provider,
                receiver,
                sidecar,
                fees,
                blob_base_fee,
                increase_ratio * 2,
            )
            .await;
        }
    };

    Ok((receipt, tx_hash))
}

#[cfg(test)]
mod tests {
    use tracing::info;

    use super::*;

    // this test is mostly to check the send_payload method isolated from the
    // rest of the AD server logic
    #[tokio::test]
    async fn test_tx() -> anyhow::Result<()> {
        common::load_dotenv()?;
        let cfg = Config::from_env()?;
        info!(?cfg, "Loaded config");

        let tx_hash = send_payload(cfg, b"test".to_vec()).await?;
        dbg!(tx_hash);

        Ok(())
    }
}
