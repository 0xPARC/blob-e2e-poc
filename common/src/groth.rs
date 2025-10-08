use std::{path::Path, time::Instant};

use anyhow::{Result, anyhow};
use tracing::info;

const INPUT_PATH: &str = "../tmp/plonky2-proof";
const OUTPUT_PATH: &str = "../tmp/groth-artifacts";

/// initializes the groth16 prover memory, loading the artifacts. This method
/// must be called before the `prove` method.
pub fn init() {
    pod2_onchain::init(INPUT_PATH, OUTPUT_PATH);
}

/// computes the one extra recursive proof from the given MainPod's proof in
/// order to shrink it, together with using the bn254's poseidon variant in the
/// configuration of the plonky2 prover, in order to make it compatible with the
/// Groth16 circuit.
/// Returns the Groth16 proof, and the Public Inputs, both in their byte-array
/// representation.
pub fn prove(pod: pod2::frontend::MainPod) -> Result<(Vec<u8>, Vec<u8>)> {
    let start = Instant::now();
    // generate new plonky2 proof (groth16's friendly kind) from POD's proof
    let (_, _, proof_with_pis) = pod2_onchain::prove_pod(pod)?;
    info!(
        "[TIME] plonky2 proof (groth16-friendly) took: {:?}",
        start.elapsed()
    );

    // check that the trusted setup & r1cs files exist
    let pk_path = Path::new(&OUTPUT_PATH).join("proving.key");
    let vk_path = Path::new(&OUTPUT_PATH).join("verifying.key");
    let r1cs_path = Path::new(&OUTPUT_PATH).join("r1cs");
    if !pk_path.exists() || !vk_path.exists() || !r1cs_path.exists() {
        return Err(anyhow!(
            "not found: pk, vk, r1cs. Path:\n  pk: {:?}\n  vk: {:?},\n  r1cs: {:?}",
            pk_path,
            vk_path,
            r1cs_path
        ));
    }

    // assuming that the trusted setup & r1cs are in place, generate the Groth16
    // proof
    let (g16_proof, g16_pub_inp) = pod2_onchain::groth16_prove(proof_with_pis)?;

    Ok((g16_proof, g16_pub_inp))
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use pod2::{
        backends::plonky2::{basetypes::DEFAULT_VD_SET, mainpod::Prover},
        frontend::{MainPodBuilder, Operation},
        middleware::{Params, containers::Set},
    };

    use super::*;

    // returns a MainPod, example adapted from pod2/examples/main_pod_points.rs
    fn compute_pod_proof() -> Result<pod2::frontend::MainPod> {
        let params = Params {
            max_input_pods: 0,
            ..Default::default()
        };

        let mut builder = MainPodBuilder::new(&params, &DEFAULT_VD_SET);
        let set_entries = ["a", "2", "3"].into_iter().map(|n| n.into()).collect();
        let set = Set::new(10, set_entries)?;

        builder.pub_op(Operation::set_contains(set, "2"))?;

        let prover = Prover {};
        let pod = builder.prove(&prover).unwrap();
        Ok(pod)
    }

    // The following test is ignored by default since it requires the trusted
    // setup and takes too long to run. To run it:
    //   cargo test --release -p common gen_sample_pod_proof -- --nocapture --ignored
    // This test is used to generate a sample of MainPod proof, which is used to generate the
    // Groth16 trusted setup.
    #[ignore]
    #[test]
    fn gen_sample_pod_proof() -> Result<()> {
        // obtain the pod to be proven
        let start = Instant::now();
        let pod = compute_pod_proof()?;
        println!(
            "[TIME] generate pod & compute pod proof took: {:?}",
            start.elapsed()
        );

        // generate new plonky2 proof (groth16's friendly kind) from POD's proof
        let (verifier_data, common_circuit_data, proof_with_pis) = pod2_onchain::prove_pod(pod)?;
        info!(
            "[TIME] plonky2 proof (groth16-friendly) took: {:?}",
            start.elapsed()
        );

        // store the files
        pod2_onchain::pod::store_files(
            &Path::new(INPUT_PATH),
            verifier_data.verifier_only,
            common_circuit_data,
            proof_with_pis,
        )?;

        Ok(())
    }

    #[ignore]
    #[test]
    fn test_prove_method() -> Result<()> {
        // obtain the pod to be proven
        let start = Instant::now();
        let pod = compute_pod_proof()?;
        println!(
            "[TIME] generate pod & compute pod proof took: {:?}",
            start.elapsed()
        );

        // initialize groth16 memory
        init();

        // compute its plonky2 & groth16 proof
        let (g16_proof, g16_pub_inp) = prove(pod)?;
        let v = pod2_onchain::groth16_verify(g16_proof, g16_pub_inp)?;
        dbg!(&v);

        Ok(())
    }
}
