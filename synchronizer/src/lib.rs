pub mod clients;

use std::error::Error;

use alloy::{
    eips::eip4844::FIELD_ELEMENT_BYTES_USIZE,
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

/// Extracts bytes from a blob in the 'simple' encoding.
pub(crate) fn bytes_from_simple_blob(blob_bytes: &[u8]) -> Result<Vec<u8>, E> {
    // Blob = [0x00] ++ 8_BYTE_LEN ++ [0x00,...,0x00] ++ X.
    let data_len = u64::from_be_bytes(std::array::from_fn(|i| blob_bytes[1 + i])) as usize;

    // Sanity check: Blob must be able to accommodate the specified data length.
    let max_data_len =
        (blob_bytes.len() / FIELD_ELEMENT_BYTES_USIZE - 1) * (FIELD_ELEMENT_BYTES_USIZE - 1);
    if data_len > max_data_len {
        return Err(format!(
            "Given blob of length {} cannot accommodate {} bytes.",
            blob_bytes.len(),
            data_len
        )
        .into());
    }

    Ok(blob_bytes
        .chunks(FIELD_ELEMENT_BYTES_USIZE)
        .skip(1)
        .flat_map(|chunk| chunk[1..].to_vec())
        .take(data_len)
        .collect())
}

#[cfg(test)]
mod tests {
    use plonky2::plonk::proof::CompressedProofWithPublicInputs;
    use pod2::{
        backends::plonky2::mainpod::Prover,
        frontend::{MainPodBuilder, Operation},
        middleware::{DEFAULT_VD_SET, Params},
    };
    use pod2_onchain::poseidon_bn128::config::PoseidonBN128GoldilocksConfig;

    use crate::{E, bytes_from_simple_blob, get_blobs};

    #[tokio::test]
    async fn test_get_blobs() -> Result<(), E> {
        let beacon_url = "https://ethereum-beacon-api.publicnode.com";
        let block_id = 11111111;
        let blobs = get_blobs(beacon_url, block_id).await?;
        println!("{:?}", blobs);
        Ok(())
    }

    // Culled from https://github.com/arnaucube/pod2-blob-example/blob/13ca6ba9fe06b1295330c2f50107b6cd8a3251ce/src/main.rs#L30
    pub fn compute_pod_proof() -> Result<pod2::frontend::MainPod, E> {
        let params = Params {
            max_input_pods: 0,
            ..Default::default()
        };

        let mut builder = MainPodBuilder::new(&params, &DEFAULT_VD_SET);
        let set_entries = [1, 2, 3].into_iter().map(|n| n.into()).collect();
        let set = pod2::middleware::containers::Set::new(10, set_entries)?;

        builder.pub_op(Operation::set_contains(set, 1))?;

        let prover = Prover {};
        let pod = builder.prove(&prover)?;
        Ok(pod)
    }
    #[test]
    fn test_arnau_proof_blob() -> Result<(), E> {
        // Arnau's blob string. Taken from https://sepolia.etherscan.io/tx/0xce74df829b8e7622f0b077e9f8a4caf002f975740ef6f155f02679f0719f4a33#blobs
        let arnau_blob_str = &std::fs::read_to_string("./arnau_blob_str")?;

        // Extract bytes from blob
        let half_len = arnau_blob_str.len() / 2;
        let blob: Vec<u8> = (0..half_len)
            .map(|i| u8::from_str_radix(&arnau_blob_str[2 * i..2 * i + 2], 16))
            .collect::<Result<_, _>>()?;
        let proof_bytes = bytes_from_simple_blob(&blob)?;

        // Deserialise/decompress proof
        let pod = compute_pod_proof()?;
        let (verifier_data, common_circuit_data, _) = pod2_onchain::prove_pod(pod)?;
        let proof =
            CompressedProofWithPublicInputs::<_, PoseidonBN128GoldilocksConfig, 2>::from_bytes(
                proof_bytes,
                &common_circuit_data.common,
            )?
            .decompress(
                &verifier_data.verifier_only.circuit_digest,
                &common_circuit_data.common,
            )?;

        // Verify proof
        common_circuit_data.verify(proof).map_err(|e| e.into())
    }
}
