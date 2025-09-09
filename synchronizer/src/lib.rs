use std::error::Error;

use alloy::{
    rpc::types::beacon::sidecar::{BeaconBlobBundle, BlobData},
    transports::http::reqwest,
};

type E = Box<dyn Error>;

pub(crate) async fn get_blobs(beacon_url: &str, block_id: u64) -> Result<Vec<BlobData>, E> {
    let req_url = format!("{}/eth/v1/beacon/blob_sidecars/{}", beacon_url, block_id);
    let resp = reqwest::get(req_url).await?.text().await?;
    let blob_bundle: BeaconBlobBundle = serde_json::from_str(&resp)?;
    Ok(blob_bundle.data)
}

#[cfg(test)]
mod tests {
    use crate::{E, get_blobs};

    #[tokio::test]
    async fn test_get_blobs() -> Result<(), E> {
        let beacon_url = "https://ethereum-beacon-api.publicnode.com";
        let block_id = 11111111;
        let blobs = get_blobs(beacon_url, block_id).await?;
        println!("{:?}", blobs);
        Ok(())
    }
}
