use std::{path::Path, time::Instant};

use anyhow::{Result, anyhow};
use tracing::info;

const INPUT_PATH: &str = "../tmp/plonky2-proof";
const OUTPUT_PATH: &str = "../tmp/groth-artifacts";

/// initializes the groth16 prover memory, loading the artifacts. This method
/// must be called before the `prove` method.
pub fn init() -> Result<()> {
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

    pod2_onchain::init(INPUT_PATH, OUTPUT_PATH);
    Ok(())
}

pub fn load_vk() -> Result<()> {
    // check that the trusted setup & r1cs files exist
    let vk_path = Path::new(&OUTPUT_PATH).join("verifying.key");
    if !vk_path.exists() {
        return Err(anyhow!("not found: vk. Path: vk: {:?}", vk_path,));
    }

    pod2_onchain::load_vk(OUTPUT_PATH);
    Ok(())
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
        frontend::MainPodBuilder,
        middleware::{Params, containers::Dictionary},
    };

    use super::*;

    // returns a MainPod, similar to the one that will be created by the AD-Server
    fn compute_pod_proof() -> Result<pod2::frontend::MainPod> {
        let params = Params::default();
        let vd_set = &*DEFAULT_VD_SET;
        let batches = app::build_predicates(&params);

        let mut builder = MainPodBuilder::new(&params, vd_set);
        let mut helper = app::Helper::new(&mut builder, &batches);

        let initial_state = Dictionary::new(
            params.max_depth_mt_containers,
            std::collections::HashMap::new(),
        )
        .unwrap();
        let (state, _st_update) = helper.st_update(initial_state.clone(), app::Op::Init.into())?;

        let op = app::Op::Add {
            group: app::Group::Red,
            user: "user1".to_string(),
        };
        let op = Dictionary::from(op);

        let (_new_state, st_update) = helper.st_update(state.clone(), op)?;
        builder.reveal(&st_update);

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
            Path::new(INPUT_PATH),
            verifier_data.verifier_only,
            common_circuit_data,
            proof_with_pis,
        )?;

        Ok(())
    }

    #[ignore]
    #[test]
    fn gen_trusted_setup() -> Result<()> {
        // if plonky2 groth16-friendly proof does not exist yet, generate it
        if !Path::new(INPUT_PATH).is_dir() {
            println!("generating plonky2 groth16-friendly proof");
            pod2_onchain::pod::sample_plonky2_g16_friendly_proof(INPUT_PATH)?;
        } else {
            println!("plonky2 groth16-friendly proof already exists, skipping generation");
        }

        // if trusted setup does not exist yet, generate it
        if !Path::new(OUTPUT_PATH).is_dir() {
            println!("generating groth16's trusted setup");
            let result = pod2_onchain::trusted_setup(INPUT_PATH, OUTPUT_PATH);
            println!("trusted_setup result: {result}");
        } else {
            println!("trusted setup already exists, skipping generation");
        }

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
        init()?;

        // compute its plonky2 & groth16 proof
        let (g16_proof, g16_pub_inp) = prove(pod.clone())?;
        pod2_onchain::groth16_verify(g16_proof.clone(), g16_pub_inp)?;

        // test the public_inputs parsing flow
        let (_, _, proof_with_pis) = pod2_onchain::prove_pod(pod)?;
        let pub_inp = proof_with_pis.public_inputs;

        // encode it as big-endian bytes compatible with Gnark
        let pub_inp_bytes = pod2_onchain::encode_public_inputs_gnark(pub_inp);
        // call groth16_verify again but now using the encoded pub_inp_bytes
        pod2_onchain::groth16_verify(g16_proof, pub_inp_bytes)?;

        Ok(())
    }
}
