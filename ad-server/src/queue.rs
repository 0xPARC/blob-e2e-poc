use std::{collections::HashMap, path::Path, str::FromStr, sync::Arc};

use alloy::primitives::TxHash;
use anyhow::{Result, anyhow};
use app::{Group, Helper, Op, RevHelper};
use common::{
    ProofType,
    disk::{load_pod, store_pod},
    groth,
    payload::{Payload, PayloadCreate, PayloadProof, PayloadUpdate},
    set_from_value,
    shrink::shrink_compress_pod,
};
use pod2::{
    backends::plonky2::{mainpod::Prover, primitives::merkletree::MerkleClaimAndProof},
    dict,
    frontend::MainPodBuilder,
    middleware::{
        Hash, RawValue, Statement, TypedValue, Value,
        containers::{self, Dictionary},
    },
};
use serde::{Deserialize, Serialize};
use tokio::{sync::mpsc::Receiver, task};
use tracing::{debug, info};
use uuid::Uuid;

use crate::{Context, db};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum State {
    Create(StateCreate),
    Update(StateUpdate),
    UpdateRev(StateUpdateRev),
    Query(StateQuery),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum StateCreate {
    Pending,
    SendingBlobTx,
    Complete { id: i64, tx_hash: TxHash },
    Error(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum StateUpdate {
    Pending,
    ProvingMainPod,
    WrappingMainPod,
    SendingBlobTx,
    Complete { tx_hash: TxHash },
    Error(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum StateUpdateRev {
    Pending,
    ProvingRevMainPod,
    Complete,
    Error(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum StateQuery {
    Pending,
    Complete {
        result: HashMap<Group, MerkleClaimAndProof>,
    },
    Error(String),
}

#[derive(Debug)]
pub enum Request {
    Create { req_id: Uuid },
    Update { req_id: Uuid, id: i64, op: Op },
    UpdateRev { req_id: Uuid, id: i64, num: i64 },
    Query { req_id: Uuid, id: i64, user: Value },
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
    debug!(req = format!("{:?}", req), "handle queue request");
    match req {
        Request::Create { req_id } => {
            if let Err(err) = handle_create(ctx.clone(), req_id).await {
                debug!(req_id = format!("{}", req_id), err = format!("{}", err));
                ctx.queue_state
                    .write()
                    .await
                    .insert(req_id, State::Create(StateCreate::Error(err.to_string())));
            }
        }
        Request::Update { req_id, id, op } => {
            if let Err(err) = handle_update(ctx.clone(), req_id, id, op).await {
                debug!(req_id = format!("{}", req_id), err = format!("{}", err));
                ctx.queue_state
                    .write()
                    .await
                    .insert(req_id, State::Update(StateUpdate::Error(err.to_string())));
            }
        }
        Request::UpdateRev { req_id, id, num } => {
            if let Err(err) = handle_update_rev(ctx.clone(), req_id, id, num).await {
                debug!(req_id = format!("{}", req_id), err = format!("{}", err));
                ctx.queue_state.write().await.insert(
                    req_id,
                    State::UpdateRev(StateUpdateRev::Error(err.to_string())),
                );
            }
        }
        Request::Query { req_id, id, user } => {
            if let Err(err) = handle_query(ctx.clone(), req_id, id, user).await {
                debug!(req_id = format!("{}", req_id), err = format!("{}", err));
                ctx.queue_state
                    .write()
                    .await
                    .insert(req_id, State::Query(StateQuery::Error(err.to_string())));
            }
        }
    }
    Ok(())
}

// TODO: Include proof.
async fn handle_create(ctx: Arc<Context>, req_id: Uuid) -> Result<()> {
    let set_req_state = async |req_state| {
        ctx.queue_state
            .write()
            .await
            .insert(req_id, State::Create(req_state));
    };

    let latest_membership_list_id = match db::get_latest_membership_list(&ctx.db_pool).await {
        Ok(Some(membership_list)) => membership_list.id,
        Ok(None) => 0,
        Err(e) => return Err(e.into()),
    };
    let new_id = latest_membership_list_id + 1;

    // Form new dictionary
    let membership_list = db::AdState {
        id: new_id,
        num: 0,
        state: db::DictContainerSql(containers::Dictionary::new(
            ctx.pod_config.params.max_depth_mt_containers,
            HashMap::new(),
        )?),
    };

    // send the payload to ethereum
    let payload_bytes = Payload::Create(PayloadCreate {
        id: Hash::from(RawValue::from(new_id)), // TODO hash
        custom_predicate_ref: ctx.pod_config.state_predicates.update.clone(),
        vds_root: ctx.pod_config.vd_set.root(),
    })
    .to_bytes();

    set_req_state(StateCreate::SendingBlobTx).await;
    let tx_hash = crate::eth::send_payload(&ctx.cfg, payload_bytes).await?;

    // update db
    db::insert_membership_list(&ctx.db_pool, &membership_list).await?;

    set_req_state(StateCreate::Complete {
        id: membership_list.id,
        tx_hash,
    })
    .await;
    Ok(())
}

async fn handle_update(ctx: Arc<Context>, req_id: Uuid, id: i64, op: Op) -> Result<()> {
    let set_req_state = async |req_state| {
        ctx.queue_state
            .write()
            .await
            .insert(req_id, State::Update(req_state));
    };
    // TODO: User validation

    // get state from db
    let membership_list = db::get_membership_list(&ctx.db_pool, id).await?;

    // with the actual POD
    let state = membership_list.state;
    let num = membership_list.num + 1;

    let start = std::time::Instant::now();

    let mut builder = MainPodBuilder::new(&ctx.pod_config.params, &ctx.pod_config.vd_set);
    let mut helper = Helper::new(&mut builder, &ctx.pod_config.state_predicates);

    let op = Dictionary::from(op);
    let op_raw = RawValue::from(op.commitment());

    let (new_state, st_update) = helper.st_update(state.0.clone(), op)?;
    builder.reveal(&st_update);

    set_req_state(StateUpdate::ProvingMainPod).await;
    let prover = Prover {};
    let pod = task::spawn_blocking(move || builder.prove(&prover).unwrap()).await?;
    println!("# state_pod\n:{}", pod);
    pod.pod.verify().unwrap();

    store_pod(
        Path::new(&ctx.cfg.pods_path),
        &format!("{:08}-{:08}-membership_list", id, num),
        &pod,
    )?;
    set_req_state(StateUpdate::WrappingMainPod).await;
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
    println!("[TIME] state pod {:?}", start.elapsed());

    let payload_bytes = Payload::Update(PayloadUpdate {
        id: Hash::from(RawValue::from(id)), // TODO hash
        proof: compressed_proof,
        new_state: new_state.commitment().into(),
        op: op_raw,
    })
    .to_bytes();

    set_req_state(StateUpdate::SendingBlobTx).await;
    let tx_hash = crate::eth::send_payload(&ctx.cfg, payload_bytes).await?;

    db::update_membership_list(&ctx.db_pool, id, num, new_state).await?;

    set_req_state(StateUpdate::Complete { tx_hash }).await;
    {
        let req_id = Uuid::now_v7();
        ctx.queue_state
            .write()
            .await
            .insert(req_id, State::UpdateRev(StateUpdateRev::Pending));
        ctx.queue_tx
            .send(Request::UpdateRev { req_id, id, num })
            .await?;
        info!(
            "self-scheduling UpdateRev {}-{} with req_id={}",
            id, num, req_id
        );
    }
    Ok(())
}

async fn handle_update_rev(ctx: Arc<Context>, req_id: Uuid, id: i64, num: i64) -> Result<()> {
    let set_req_state = async |req_state| {
        ctx.queue_state
            .write()
            .await
            .insert(req_id, State::UpdateRev(req_state));
    };

    if num == 0 {
        anyhow::bail!("num = 0, state not initialized");
    }
    let name = format!("{:08}-{:08}-membership_list", id, num);
    let state_pod = load_pod(Path::new(&ctx.cfg.pods_path), &name)?;

    let st_update = state_pod.pod.pub_statements()[0].clone();
    let arg2 = st_update.args()[2].literal().unwrap();
    let op = if let TypedValue::Dictionary(op) = arg2.typed() {
        op.clone()
    } else {
        panic!("Value not a Dictionary: {:?}", arg2)
    };

    let (old_rev_state_pod, rev_state) = if num > 1 {
        let rev_name = format!("{:08}-{:08}-rev_membership_list", id, num - 1);
        let old_rev_state_pod = load_pod(Path::new(&ctx.cfg.pods_path), &rev_name)?;
        let rev_state = db::get_rev_membership_list(&ctx.db_pool, id).await?.state;
        (Some(old_rev_state_pod), rev_state.0)
    } else {
        // State at num=1 is the base-case for rev_state and doesn't have a previous rev_state
        (
            None,
            dict!(ctx.pod_config.params.max_depth_mt_containers, {}).unwrap(),
        )
    };

    let start = std::time::Instant::now();
    set_req_state(StateUpdateRev::ProvingRevMainPod).await;

    let mut builder = MainPodBuilder::new(&ctx.pod_config.params, &ctx.pod_config.vd_set);
    let mut rev_helper = RevHelper::new(
        &mut builder,
        &ctx.pod_config.state_predicates,
        &ctx.pod_config.rev_predicates,
    );

    let mut builder = MainPodBuilder::new(&ctx.pod_config.params, &ctx.pod_config.vd_set);
    builder.add_pod(state_pod);
    let old_st_rev_sync = if let Some(old_rev_state_pod) = old_rev_state_pod {
        builder.add_pod(old_rev_state_pod.clone());
        old_rev_state_pod.pod.pub_statements()[0].clone()
    } else {
        Statement::None
    };
    let (rev_state, rev_st_update) =
        rev_helper.st_rev_sync(rev_state, op, st_update, old_st_rev_sync);
    builder.reveal(&rev_st_update);
    let prover = Prover {};
    // FIXME: This prove is failing with this error:
    // `anyhow::Error: Partition containing Wire(Wire { row: 48691, column: 43 }) was set twice with different values: 1 != 0`
    // So for now we return the error so that the queue doesn't panic.  Ideally we'd `unwrap()`
    // adter `prove` inside the blocking task.
    let rev_state_pod = task::spawn_blocking(move || builder.prove(&prover)).await??;
    println!("# rev_state_pod\n:{}", rev_state_pod);
    rev_state_pod.pod.verify().unwrap();

    println!("[TIME] rev_state_pod {:?}", start.elapsed());

    store_pod(
        Path::new(&ctx.cfg.pods_path),
        &format!("{:08}-{:08}-rev_membership_list", id, num),
        &rev_state_pod,
    )?;

    db::update_rev_membership_list(&ctx.db_pool, id, num, rev_state).await?;
    set_req_state(StateUpdateRev::Complete).await;
    Ok(())
}

async fn handle_query(ctx: Arc<Context>, req_id: Uuid, id: i64, user: Value) -> Result<()> {
    let set_req_state = async |req_state| {
        ctx.queue_state
            .write()
            .await
            .insert(req_id, State::Query(req_state));
    };

    // get state from db
    let state = db::get_membership_list(&ctx.db_pool, id).await?.state.0;

    let dict_kvs = state
        .kvs()
        .iter()
        .map(|(group, v)| {
            set_from_value(v).and_then(|s| {
                Group::from_str(group.name())
                    .map_err(|_| anyhow!("Invalid group: {}", group))
                    .map(|group| (group, s))
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let result = dict_kvs
        .into_iter()
        .filter_map(|(group, s)| {
            s.contains(&user).then(|| {
                s.prove(&user)
                    .map(|proof| {
                        (
                            group,
                            MerkleClaimAndProof {
                                root: s.commitment(),
                                key: user.raw(),
                                value: user.raw(),
                                proof,
                            },
                        )
                    })
                    .map_err(|e| e.into())
            })
        })
        .collect::<Result<HashMap<_, _>>>()?;

    set_req_state(StateQuery::Complete { result }).await;

    Ok(())
}
