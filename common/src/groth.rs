use std::{path::Path, process::Command, time::Instant};

use anyhow::Result;
use pod2_onchain::{prove_pod, store_files};
use tracing::info;

/// computes the one extra recursive proof from the given MainPod's proof in
/// order to shrink it, together with using the bn254's poseidon variant in the
/// configuration of the plonky2 prover, in order to make it compatible with the
/// Groth16 circuit
pub fn prove(pod: pod2::frontend::MainPod) -> Result<()> {
    let start = Instant::now();
    let (verifier_data, common_circuit_data, proof_with_pis) = prove_pod(pod)?;
    info!(
        "[TIME] plonky2 proof (groth16-friendly) took: {:?}",
        start.elapsed()
    );
    debug_assert_eq!(proof_with_pis.public_inputs.len(), 14); // poseidon + vdset_root + sha256 + gamma

    // store the files
    store_files(
        Path::new("testdata/pod"),
        verifier_data.verifier_only,
        common_circuit_data,
        proof_with_pis,
    )?;

    // TODO call groth16 verifier (assuming that the trusted setup & r1cs have
    // been already generated)

    Ok(())
}

fn gen_groth16_proof() -> Result<()> {
    // TODO get proof from a different input each time
    let status = Command::new("pod2-onchain -p i tmp/podproof -o tmp/grothartifacts")
        .status()
        .expect("failed to execute process");
    if let Some(code) = status.code() {
        match code {
            0 => println!("Exited with os.Exit(0)"),
            1 => println!("Exited with os.Exit(1)"),
            _ => println!("Exited with code: {}", code),
        }
    } else {
        println!("Process terminated by signal");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

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
        let set_entries = [1, 2, 3].into_iter().map(|n| n.into()).collect();
        let set = Set::new(10, set_entries)?;

        builder.pub_op(Operation::set_contains(set, 1))?;

        let prover = Prover {};
        let pod = builder.prove(&prover).unwrap();
        Ok(pod)
    }

    #[test]
    fn gen_sample_pod_proof() -> Result<()> {
        // step 1) obtain the pod to be proven
        let start = Instant::now();
        let pod = compute_pod_proof()?;
        println!(
            "[TIME] generate pod & compute pod proof took: {:?}",
            start.elapsed()
        );

        // step 2) generate new plonky2 proof from POD's proof
        let start = Instant::now();
        let (verifier_data, common_circuit_data, proof_with_pis) = prove_pod(pod)?;
        println!(
            "[TIME] plonky2 proof (groth16-friendly) took: {:?}",
            start.elapsed()
        );
        assert_eq!(proof_with_pis.public_inputs.len(), 14); // poseidon + vdset_root + sha256 + gamma

        // step 3) store the files
        store_files(
            Path::new("../tmp/podproof"),
            verifier_data.verifier_only,
            common_circuit_data,
            proof_with_pis,
        )?;

        Ok(())
    }
}
