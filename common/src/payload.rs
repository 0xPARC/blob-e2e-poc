use std::io::{Read, Write};

use anyhow::{Result, anyhow};
use plonky2::{
    field::types::{Field, Field64, PrimeField64},
    plonk::proof::CompressedProof,
    util::serialization::Buffer,
};
use pod2::middleware::{
    C, CommonCircuitData, CustomPredicateBatch, CustomPredicateRef, D, F, Hash, RawValue,
};

pub fn write_elems<const N: usize>(bytes: &mut Vec<u8>, elems: &[F; N]) {
    for elem in elems {
        bytes
            .write_all(&elem.to_canonical_u64().to_le_bytes())
            .expect("vec write");
    }
}

pub fn read_elems<const N: usize>(bytes: &mut impl Read) -> Result<[F; N]> {
    let mut elems = [F::ZERO; N];
    let mut elem_bytes = [0; 8];
    #[allow(clippy::needless_range_loop)]
    for i in 0..N {
        bytes.read_exact(&mut elem_bytes)?;
        let n = u64::from_le_bytes(elem_bytes);
        if n >= F::ORDER {
            return Err(anyhow!("{} >= F::ORDER", n));
        }
        elems[i] = F::from_canonical_u64(n);
    }
    Ok(elems)
}

pub fn write_custom_predicate_ref(bytes: &mut Vec<u8>, cpr: &CustomPredicateRef) {
    write_elems(bytes, &cpr.batch.id().0);
    bytes
        .write_all(&(cpr.index as u8).to_le_bytes())
        .expect("vec write");
}

pub fn read_custom_predicate_ref(bytes: &mut impl Read) -> Result<CustomPredicateRef> {
    let custom_pred_batch_id = Hash(read_elems(bytes)?);
    let custom_pred_index = {
        let mut buffer = [0; 1];
        bytes.read_exact(&mut buffer)?;
        u8::from_le_bytes(buffer) as usize
    };
    Ok(CustomPredicateRef {
        batch: CustomPredicateBatch::new_opaque("unknown".to_string(), custom_pred_batch_id),
        index: custom_pred_index,
    })
}

#[derive(Debug, Eq, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum Payload {
    Init(PayloadInit),
    Update(PayloadUpdate),
}

const PAYLOAD_MAGIC: u16 = 0xad00;
const PAYLOAD_TYPE_INIT: u8 = 1;
const PAYLOAD_TYPE_UPDATE: u8 = 2;

impl Payload {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buffer = Vec::new();
        buffer
            .write_all(&PAYLOAD_MAGIC.to_le_bytes())
            .expect("vec write");
        match self {
            Self::Init(payload) => {
                buffer
                    .write_all(&PAYLOAD_TYPE_INIT.to_le_bytes())
                    .expect("vec write");
                payload.write_bytes(&mut buffer);
            }
            Self::Update(payload) => {
                buffer
                    .write_all(&PAYLOAD_TYPE_UPDATE.to_le_bytes())
                    .expect("vec write");
                payload.write_bytes(&mut buffer);
            }
        }
        buffer
    }

    pub fn from_bytes(bytes: &[u8], common_data: &CommonCircuitData) -> Result<Self> {
        let mut bytes = bytes;
        let magic = {
            let mut buffer = [0; 2];
            bytes.read_exact(&mut buffer)?;
            u16::from_le_bytes(buffer)
        };
        if magic != PAYLOAD_MAGIC {
            return Err(anyhow!("Invalid payload magic: {:04x}", magic));
        }
        let type_ = {
            let mut buffer = [0; 1];
            bytes.read_exact(&mut buffer)?;
            u8::from_le_bytes(buffer)
        };
        Ok(match type_ {
            PAYLOAD_TYPE_INIT => Payload::Init(PayloadInit::from_bytes(bytes)?),
            PAYLOAD_TYPE_UPDATE => Payload::Update(PayloadUpdate::from_bytes(bytes, common_data)?),
            t => return Err(anyhow!("Invalid payload type: {}", t)),
        })
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct PayloadInit {
    pub id: Hash,
    pub custom_predicate_ref: CustomPredicateRef,
    pub vds_root: Hash,
}

impl PayloadInit {
    pub fn write_bytes(&self, buffer: &mut Vec<u8>) {
        write_elems(buffer, &self.id.0);
        write_custom_predicate_ref(buffer, &self.custom_predicate_ref);
        write_elems(buffer, &self.vds_root.0);
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let mut bytes = bytes;
        let id = Hash(read_elems(&mut bytes)?);
        let custom_predicate_ref = read_custom_predicate_ref(&mut bytes)?;
        let vds_root = Hash(read_elems(&mut bytes)?);
        Ok(Self {
            id,
            custom_predicate_ref,
            vds_root,
        })
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct PayloadUpdate {
    pub id: Hash,
    pub shrunk_main_pod_proof: CompressedProof<F, C, D>,
    pub new_state: RawValue,
}

impl PayloadUpdate {
    pub fn write_bytes(&self, buffer: &mut Vec<u8>) {
        write_elems(buffer, &self.id.0);
        plonky2::util::serialization::Write::write_compressed_proof(
            buffer,
            &self.shrunk_main_pod_proof,
        )
        .expect("vec write");
        write_elems(buffer, &self.new_state.0);
    }

    pub fn from_bytes(bytes: &[u8], common_data: &CommonCircuitData) -> Result<Self> {
        let mut bytes = bytes;
        let id = Hash(read_elems(&mut bytes)?);
        let shrunk_main_pod_proof = {
            let mut buffer = Buffer::new(bytes);
            let proof =
                plonky2::util::serialization::Read::read_compressed_proof(&mut buffer, common_data)
                    .map_err(|e| anyhow!("read_compressed_proof: {}", e))?;
            let len = buffer.pos();
            bytes = &bytes[len..];
            proof
        };
        let new_state = RawValue(read_elems(&mut bytes)?);
        Ok(Self {
            id,
            shrunk_main_pod_proof,
            new_state,
        })
    }
}

#[cfg(test)]
mod tests {
    use plonky2::plonk::proof::CompressedProofWithPublicInputs;
    use pod2::{
        backends::plonky2::{
            basetypes::DEFAULT_VD_SET,
            mainpod::{Prover, calculate_statements_hash},
        },
        dict,
        frontend::MainPodBuilder,
        middleware::{Params, Statement, Value},
    };

    use super::*;
    use crate::circuits::{ShrunkMainPodSetup, shrink_compress_pod};

    #[test]
    fn test_payload_roundtrip() {
        let params = Params::default();
        println!("SrhunkMainPod setup");
        let shrunk_main_pod_build = ShrunkMainPodSetup::new(&params).build().unwrap();
        let common_data = &shrunk_main_pod_build.circuit_data.common;
        let predicates = app::build_predicates(&params);
        let id = Hash([F(1), F(2), F(3), F(4)]);
        let custom_predicate_ref = CustomPredicateRef {
            batch: CustomPredicateBatch::new_opaque(
                "unknown".to_string(),
                predicates.update_pred.batch.id(),
            ),
            index: predicates.update_pred.index,
        };
        let vd_set = &*DEFAULT_VD_SET;
        let vds_root = vd_set.root();
        let payload_init = Payload::Init(PayloadInit {
            id,
            custom_predicate_ref: custom_predicate_ref.clone(),
            vds_root,
        });

        println!("PayloadInit roundtrip");
        let payload_init_bytes = payload_init.to_bytes();
        let payload_init_decoded = Payload::from_bytes(&payload_init_bytes, common_data).unwrap();
        assert_eq!(payload_init, payload_init_decoded);

        let mut builder = MainPodBuilder::new(&params, vd_set);
        let predicates = app::build_predicates(&params);
        let mut helper = app::Helper::new(&mut builder, &predicates);

        let state = 0;
        let count = 6;
        let op = dict!(32, {"name" => "inc", "n" => count}).unwrap();
        let (new_state, st_update) = helper.st_update(state, &[op]);
        println!("st: {:?}", st_update);
        builder.reveal(&st_update);

        println!("MainPod prove");
        let prover = Prover {};
        let pod = builder.prove(&prover).unwrap();
        pod.pod.verify().unwrap();

        println!("MainPod shrink & compress");
        let shrunk_main_pod_proof = shrink_compress_pod(&shrunk_main_pod_build, pod).unwrap();

        let payload_update = Payload::Update(PayloadUpdate {
            id,
            shrunk_main_pod_proof: shrunk_main_pod_proof.clone(),
            new_state: RawValue::from(new_state),
        });

        println!("PayloadUpdate roundtrip");
        let payload_update_bytes = payload_update.to_bytes();
        let payload_update_decoded =
            Payload::from_bytes(&payload_update_bytes, common_data).unwrap();
        assert_eq!(payload_update, payload_update_decoded);

        // Veirfy the proof

        println!("Verify shrunk mainPod");
        let st = Statement::Custom(
            custom_predicate_ref,
            vec![Value::from(new_state), Value::from(state)],
        );
        println!("st: {:?}", st);
        let sts_hash = calculate_statements_hash(&[st.into()], &params);
        let public_inputs = [sts_hash.0, vds_root.0].concat();
        let proof_with_pis = CompressedProofWithPublicInputs {
            proof: shrunk_main_pod_proof,
            public_inputs,
        };
        dbg!(&proof_with_pis.public_inputs);
        let proof = proof_with_pis
            .decompress(
                &shrunk_main_pod_build
                    .circuit_data
                    .verifier_only
                    .circuit_digest,
                &shrunk_main_pod_build.circuit_data.common,
            )
            .unwrap();
        shrunk_main_pod_build.circuit_data.verify(proof).unwrap();
    }
}
