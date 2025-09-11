use std::{
    fs,
    io::{Read, Write},
    ops::Deref,
    time::Instant,
};

use anyhow::Result;
use itertools::Itertools;
use plonky2::{
    field::types::Field,
    iop::witness::{PartialWitness, WitnessWrite},
    plonk::{
        circuit_builder::CircuitBuilder,
        circuit_data::{
            CircuitConfig, CircuitData, CommonCircuitData, VerifierCircuitData,
            VerifierOnlyCircuitData,
        },
        config::GenericConfig,
        proof::ProofWithPublicInputs,
    },
};
use pod2::{
    backends::plonky2::{
        basetypes::{C, D, DEFAULT_VD_SET, F, Proof},
        mainpod::Prover,
    },
    frontend::{MainPodBuilder, Operation},
    middleware::{Params, ToFields, containers::Set},
};

// returns a MainPod, example adapted from pod2/examples/main_pod_points.rs
pub fn compute_pod_proof() -> Result<pod2::frontend::MainPod> {
    let params = Params::default();

    let mut builder = MainPodBuilder::new(&params, &DEFAULT_VD_SET);
    let set_entries = [1, 2, 3].into_iter().map(|n| n.into()).collect();
    let set = Set::new(10, set_entries)?;

    builder.pub_op(Operation::set_contains(set, 1))?;

    let prover = Prover {};
    let pod = builder.prove(&prover).unwrap();
    Ok(pod)
}

/// returns a compressed proof for the given MainPod
pub fn compress_pod(pod: pod2::frontend::MainPod) -> Result<Vec<u8>> {
    // generate new plonky2 proof from POD's proof. This is 1 extra recursion in
    // order to shrink the proof size, together with removing extra custom gates
    let start = Instant::now();
    let (verifier_data, common_circuit_data, proof_with_pis) = prove_pod(pod)?;
    println!("[TIME] plonky2 (wrapper) proof took: {:?}", start.elapsed());

    // get the compressed proof, which we will send inside a blob
    let compressed_proof = proof_with_pis.compress(
        &verifier_data.verifier_only.circuit_digest,
        &common_circuit_data.common,
    )?;
    let compressed_proof_pis_bytes = compressed_proof.to_bytes();
    // store it in a file just in case we want to check it later
    let mut file = fs::File::create("proof_with_public_inputs.bin")?;
    file.write_all(&compressed_proof_pis_bytes)?;
    dbg!(&compressed_proof_pis_bytes.len());

    println!(
        "size of proof_with_pis: {}",
        compressed_proof_pis_bytes.len()
    );
    Ok(compressed_proof_pis_bytes)
}

/// proves the given MainPod, while shrinking it's proof so that it fits in a blob
fn prove_pod(
    pod: pod2::frontend::MainPod,
) -> Result<(
    VerifierCircuitData<F, C, D>,
    CircuitData<F, C, D>,
    ProofWithPublicInputs<F, C, D>,
)> {
    let pod_verifier_data: VerifierOnlyCircuitData<C, D> = pod.pod.verifier_data();

    let rec_main_pod_verifier_circuit_data =
        &*pod2::backends::plonky2::mainpod::cache_get_rec_main_pod_verifier_circuit_data(
            &pod.pod.params(),
        );
    let pod_common_circuit_data: CommonCircuitData<F, D> =
        rec_main_pod_verifier_circuit_data.deref().common.clone();

    let pod_proof: Proof = pod.pod.proof();
    let public_inputs = pod
        .statements_hash()
        .to_fields(&pod.params)
        .iter()
        .chain(pod.pod.vd_set().root().0.iter())
        .cloned()
        .collect_vec();
    let pod_proof_with_pis = pod2::middleware::ProofWithPublicInputs {
        proof: pod_proof.clone(),
        public_inputs,
    };

    // generate new plonky2 proof from POD's proof
    let start = Instant::now();
    let (verifier_data, common_circuit_data, proof_with_pis) = shrink_proof(
        pod_verifier_data,
        pod_common_circuit_data,
        pod_proof_with_pis,
    )?;
    println!("[TIME] encapsulation proof took: {:?}", start.elapsed());

    // sanity check: verify proof
    verifier_data.verify(proof_with_pis.clone())?;

    Ok((verifier_data, common_circuit_data, proof_with_pis))
}

/// performs 1 level recursion (plonky2) to get rid of extra custom gates and zk
pub(crate) fn shrink_proof(
    verifier_only_data: VerifierOnlyCircuitData<C, D>,
    common_circuit_data: CommonCircuitData<F, D>,
    proof_with_public_inputs: ProofWithPublicInputs<F, C, D>,
) -> Result<(
    VerifierCircuitData<F, C, D>,
    CircuitData<F, C, D>,
    ProofWithPublicInputs<F, C, D>,
)> {
    let config = CircuitConfig::standard_recursion_config();
    let mut builder: CircuitBuilder<<C as GenericConfig<D>>::F, D> = CircuitBuilder::new(config);

    // create circuit logic
    let proof_with_pis_target = builder.add_virtual_proof_with_pis(&common_circuit_data);
    let verifier_circuit_target = builder.constant_verifier_data(&verifier_only_data);
    builder.verify_proof::<C>(
        &proof_with_pis_target,
        &verifier_circuit_target,
        &common_circuit_data,
    );

    builder.register_public_inputs(&proof_with_pis_target.public_inputs);

    let circuit_data = builder.build::<C>();

    // set targets
    let mut pw = PartialWitness::new();
    pw.set_verifier_data_target(&verifier_circuit_target, &verifier_only_data)?;
    pw.set_proof_with_pis_target(&proof_with_pis_target, &proof_with_public_inputs)?;

    let vd = circuit_data.verifier_data();
    let proof = circuit_data.prove(pw)?;

    Ok((vd, circuit_data, proof))
}

/// simple circuit that computes few iterations of the Fibonacci sequence
// NOTE: this method is temporal for initial tests before the app is integrated.
pub fn simple_circuit() -> Result<(
    VerifierCircuitData<F, C, D>,
    CommonCircuitData<F, D>,
    ProofWithPublicInputs<F, C, D>,
)> {
    let config = CircuitConfig::standard_recursion_config();
    let mut builder = CircuitBuilder::<F, D>::new(config);

    // the arithmetic circuit.
    let initial_a = builder.add_virtual_target();
    let initial_b = builder.add_virtual_target();
    let mut prev_target = initial_a;
    let mut cur_target = initial_b;
    for _ in 0..99 {
        let temp = builder.add(prev_target, cur_target);
        prev_target = cur_target;
        cur_target = temp;
    }

    // public inputs are the two initial values (provided below) and the
    // result (which is generated).
    builder.register_public_input(initial_a);
    builder.register_public_input(initial_b);
    builder.register_public_input(cur_target);

    // provide initial values.
    let mut pw = PartialWitness::new();
    pw.set_target(initial_a, F::ZERO)?;
    pw.set_target(initial_b, F::ONE)?;

    let data = builder.build::<C>();

    let proof = data.prove(pw)?;
    let vd = data.verifier_data();
    let cd = vd.common.clone();

    Ok((vd, cd, proof))
}
