use std::{collections::HashSet, sync::Arc};

use alloy::primitives::TxHash;
use anyhow::Result;
use app::{DEPTH, Helper};
use common::{
    ProofType, groth,
    payload::{Payload, PayloadInit, PayloadProof, PayloadUpdate},
    shrink::shrink_compress_pod,
};
use pod2::{
    backends::plonky2::mainpod::Prover,
    dict,
    frontend::MainPodBuilder,
    middleware::{Hash, RawValue, Value, containers},
};
use serde::{Deserialize, Serialize};
use tokio::{sync::mpsc::Receiver, task};
use uuid::Uuid;

use crate::{Context, db};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum State {
    Init(StateInit),
    Update(StateUpdate),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum StateInit {
    Pending,
    SendingBlobTx,
    Complete { id: i64, tx_hash: TxHash },
    Error(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum StateUpdate {
    Pending,
    ProvingMainPod,
    ShrinkingMainPod,
    SendingBlobTx,
    Complete { tx_hash: TxHash },
    Error(String),
}

#[derive(Debug)]
pub enum Request {
    Init { req_id: Uuid },
    Update { req_id: Uuid, id: i64, data: Value },
}

pub async fn handle_loop(ctx: Arc<Context>, mut queue_rx: Receiver<Request>) {
    loop {
        let res = match queue_rx.recv().await {
            Some(req) => handle_req(ctx.clone(), req).await,
            None => panic!("channel closed"),
        };
        if let Err(err) = res {
            panic!("Queue: {:?}", err);
        }
    }
}

pub async fn handle_req(ctx: Arc<Context>, req: Request) -> Result<()> {
    match req {
        Request::Init { req_id } => {
            if let Err(err) = handle_init(ctx.clone(), req_id).await {
                ctx.queue_state
                    .write()
                    .await
                    .insert(req_id, State::Init(StateInit::Error(err.to_string())));
            }
        }
        Request::Update { req_id, id, data } => {
            if let Err(err) = handle_update(ctx.clone(), req_id, id, data).await {
                ctx.queue_state
                    .write()
                    .await
                    .insert(req_id, State::Update(StateUpdate::Error(err.to_string())));
            }
        }
    }
    Ok(())
}

async fn handle_init(ctx: Arc<Context>, req_id: Uuid) -> Result<()> {
    let set_state = async |state| {
        ctx.queue_state
            .write()
            .await
            .insert(req_id, State::Init(state));
    };

    let latest_set = match sqlx::query_as::<_, db::Set>(
        "SELECT id, set_container FROM sets ORDER BY id DESC LIMIT 1",
    )
    .fetch_optional(&ctx.db_pool)
    .await
    {
        Ok(Some(set)) => set,
        Ok(None) => db::Set {
            id: 0,
            set_container: db::SetContainerSql(
                containers::Set::new(
                    ctx.pod_config.params.max_depth_mt_containers,
                    HashSet::new(),
                )
                .expect("Should be able to construct empty set."),
            ),
        },
        Err(e) => return Err(e.into()),
    };
    let new_id = latest_set.id + 1;

    // send the payload to ethereum
    let payload_bytes = Payload::Init(PayloadInit {
        id: Hash::from(RawValue::from(new_id)), // TODO hash
        custom_predicate_ref: ctx.pod_config.predicates.update_pred.clone(),
        vds_root: ctx.pod_config.vd_set.root(),
    })
    .to_bytes();

    set_state(StateInit::SendingBlobTx).await;
    let tx_hash = crate::eth::send_payload(&ctx.cfg, payload_bytes).await?;

    // update db
    let set = db::Set {
        id: new_id,
        set_container: db::SetContainerSql(
            containers::Set::new(
                ctx.pod_config.params.max_depth_mt_containers,
                HashSet::new(),
            )
            .expect("Should be able to construct empty set."),
        ),
    };
    db::insert_set(&ctx.db_pool, &set).await?;

    set_state(StateInit::Complete {
        id: set.id,
        tx_hash,
    })
    .await;
    Ok(())
}

async fn handle_update(ctx: Arc<Context>, req_id: Uuid, id: i64, data: Value) -> Result<()> {
    let set_state = async |state| {
        ctx.queue_state
            .write()
            .await
            .insert(req_id, State::Update(state));
    };
    // TODO: Data validation

    // get state from db
    let set = db::get_set(&ctx.db_pool, id).await?;

    // with the actual POD
    let state = set.set_container;

    let start = std::time::Instant::now();

    let mut builder = MainPodBuilder::new(&ctx.pod_config.params, &ctx.pod_config.vd_set);
    let mut helper = Helper::new(&mut builder, &ctx.pod_config.predicates);

    let op = dict!(DEPTH, {"name" => "ins", "data" => data.clone()}).unwrap();

    let (new_state, st_update) = helper.st_update(state.0.clone(), &[op]);
    builder.reveal(&st_update);

    // sanity check
    println!("set old state: {:?}", state.0);
    println!("set new state: {:?}", new_state);
    let mut expected_new_state = state.0.clone();
    expected_new_state
        .insert(&data)
        .expect("Set should be able to accommodate a new entry.");

    if new_state != expected_new_state {
        // if we're inside this if, means that the pod2 lib has done something
        // wrong, hence, trigger a panic so that we notice it
        panic!(
            "new_state: {:?} != old_state ++ [data]: {:?}",
            new_state, expected_new_state
        );
    }

    set_state(StateUpdate::ProvingMainPod).await;
    let prover = Prover {};
    let pod = task::spawn_blocking(move || builder.prove(&prover).unwrap()).await?;
    println!("# pod\n:{}", pod);
    pod.pod.verify().unwrap();

    set_state(StateUpdate::ShrinkingMainPod).await;
    let compressed_proof = match ctx.cfg.proof_type {
        ProofType::Plonky2 => {
            let ctx = ctx.clone();
            let compressed_proof = task::spawn_blocking(move || {
                shrink_compress_pod(&ctx.shrunk_main_pod_build, pod).unwrap()
            })
            .await?;
            PayloadProof::Plonky2(Box::new(compressed_proof))
        }
        ProofType::Groth16 => {
            let compressed_proof = task::spawn_blocking(move || groth::prove(pod).unwrap()).await?;
            PayloadProof::Groth16(compressed_proof)
        }
    };
    println!("[TIME] ins_set pod {:?}", start.elapsed());

    let payload_bytes = Payload::Update(PayloadUpdate {
        id: Hash::from(RawValue::from(id)), // TODO hash
        proof: compressed_proof,
        new_state: new_state.commitment().into(),
    })
    .to_bytes();

    set_state(StateUpdate::SendingBlobTx).await;
    let tx_hash = crate::eth::send_payload(&ctx.cfg, payload_bytes).await?;

    db::update_set(&ctx.db_pool, id, new_state).await?;

    set_state(StateUpdate::Complete { tx_hash }).await;
    Ok(())
}
