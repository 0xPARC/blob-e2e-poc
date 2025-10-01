use std::{path::Path, process::Command, time::Instant};

use anyhow::{Result, anyhow};
use pod2_onchain::{prove_pod, store_files};
use tracing::info;

const BIN_PATH: &str = "../pod2-onchain";
const PATH: &str = "../tmp";

/// computes the one extra recursive proof from the given MainPod's proof in
/// order to shrink it, together with using the bn254's poseidon variant in the
/// configuration of the plonky2 prover, in order to make it compatible with the
/// Groth16 circuit
pub fn prove(pod: pod2::frontend::MainPod) -> Result<Vec<u8>> {
    let start = Instant::now();
    // generate new plonky2 proof (groth16's friendly kind) from POD's proof
    let (verifier_data, common_circuit_data, proof_with_pis) = prove_pod(pod)?;
    info!(
        "[TIME] plonky2 proof (groth16-friendly) took: {:?}",
        start.elapsed()
    );

    // store the files
    store_files(
        &Path::new(PATH).join("podproof"),
        verifier_data.verifier_only,
        common_circuit_data,
        proof_with_pis,
    )?;

    // check that the trusted setup & r1cs files exist
    let pk_path = Path::new(PATH).join("grothartifacts/proving.key");
    let vk_path = Path::new(PATH).join("grothartifacts/verifying.key");
    let r1cs_path = Path::new(PATH).join("grothartifacts/r1cs");
    if !pk_path.exists() || !vk_path.exists() || !r1cs_path.exists() {
        return Err(anyhow!(
            "not found: pk, vk, r1cs. Path:\n  pk: {:?}\n  vk: {:?},\n  r1cs: {:?}",
            pk_path,
            vk_path,
            r1cs_path
        ));
    }

    // assuming that the trusted setup & r1cs have been already generated,
    // generate the Groth16 proof
    gen_groth16_proof()?;

    // read proof from file and return it
    let proof_path = Path::new(PATH).join("grothartifacts/proof.proof");
    if !proof_path.exists() {
        return Err(anyhow!("groth16 proof not found. Path: {:?}", proof_path));
    }
    let proof_bytes = std::fs::read(proof_path)?;

    Ok(proof_bytes)
}

#[allow(dead_code)]
fn gen_groth16_ts() -> Result<()> {
    groth16_cli(vec![
        "-t".to_string(),
        "-i".to_string(),
        format!("{}/podproof", PATH),
        "-o".to_string(),
        format!("{}/grothartifacts", PATH),
    ])
}

fn gen_groth16_proof() -> Result<()> {
    groth16_cli(vec![
        "-p".to_string(),
        "-i".to_string(),
        format!("{}/podproof", PATH),
        "-o".to_string(),
        format!("{}/grothartifacts", PATH),
    ])
}

#[allow(dead_code)]
fn gen_groth16_verify() -> Result<()> {
    groth16_cli(vec![
        "-v".to_string(),
        "-o".to_string(),
        format!("{}/grothartifacts", PATH),
    ])
}

fn groth16_cli(args: Vec<String>) -> Result<()> {
    println!("calling pod2-onchain with args: {args:?}");

    let bin_path = Path::new(BIN_PATH);
    dbg!(bin_path.exists());
    if !bin_path.exists() {
        return Err(anyhow!("binary not found at path {:?}", bin_path));
    }
    dbg!(&bin_path);

    let output = Command::new(BIN_PATH)
        .args(args)
        .output()
        .expect("failed to execute process");
    println!("status: {}", output.status);
    println!("stdout: {}", String::from_utf8_lossy(&output.stdout));
    println!("stderr: {}", String::from_utf8_lossy(&output.stderr));
    if let Some(code) = output.status.code() {
        match code {
            0 => {}
            _ => return Err(anyhow!("Exited with code: {}", code)),
        }
    } else {
        println!("Process terminated by signal");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use pod2::{
        backends::plonky2::{basetypes::DEFAULT_VD_SET, mainpod::Prover},
        dict,
        frontend::MainPodBuilder,
        middleware::{Params, Value, containers::Set},
    };

    use super::*;

    // returns a MainPod
    fn compute_pod_proof() -> Result<pod2::frontend::MainPod> {
        let params = Params::default();
        let vd_set = &*DEFAULT_VD_SET;
        let predicates = app::build_predicates(&params);

        let state = Set::new(
            params.max_depth_mt_containers,
            std::collections::HashSet::new(),
        )
        .expect("Should be able to construct empty set.");

        let mut builder = MainPodBuilder::new(&params, &vd_set);
        let mut helper = app::Helper::new(&mut builder, &predicates);

        let data = Value::from(33);
        dbg!("DATA", &data);
        let op = dict!(app::DEPTH, {"name" => "ins", "data" => data.clone()}).unwrap();

        let (_new_state, st_update) = helper.st_update(state.clone(), &[op]);
        builder.reveal(&st_update);

        let mut expected_new_state = state.clone();
        expected_new_state
            .insert(&data)
            .expect("Set should be able to accommodate a new entry.");

        let prover = Prover {};
        let pod = builder.prove(&prover)?;
        println!("# pod\n:{}", pod);
        pod.pod.verify().unwrap();

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
        let (verifier_data, common_circuit_data, proof_with_pis) = prove_pod(pod)?;
        info!(
            "[TIME] plonky2 proof (groth16-friendly) took: {:?}",
            start.elapsed()
        );

        // store the files
        store_files(
            &Path::new(PATH).join("podproof"),
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

        // compute its plonky2 & groth16 proof
        let _ = prove(pod)?;

        Ok(())
    }
}
