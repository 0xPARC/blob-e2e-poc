use core::fmt;
use std::{
    collections::{HashMap, HashSet},
    str::FromStr,
    sync::Arc,
};

use alloy::primitives::TxHash;
use anyhow::{Result, anyhow};
use app::{DEPTH, Group, Helper};
use common::{
    ProofType, groth,
    payload::{Payload, PayloadInit, PayloadProof, PayloadUpdate},
    shrink::shrink_compress_pod,
};
use pod2::{
    backends::plonky2::{mainpod::Prover, primitives::merkletree::MerkleClaimAndProof},
    dict,
    frontend::MainPodBuilder,
    middleware::{Hash, Key, RawValue, Value, containers},
};
use serde::{Deserialize, Serialize};
use tokio::{sync::mpsc::Receiver, task};
use uuid::Uuid;

use crate::{Context, db};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum State {
    Init(StateInit),
    Update(StateUpdate),
    Query(StateQuery),
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
    WrappingMainPod,
    SendingBlobTx,
    Complete { tx_hash: TxHash },
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
    Init {
        req_id: Uuid,
    },
    Update {
        req_id: Uuid,
        update: Update,
        id: i64,
        group: Group,
        user: Value,
    },
    Query {
        req_id: Uuid,
        id: i64,
        user: Value,
    },
}

#[derive(Debug)]
pub enum Update {
    Add,
    Delete,
}

impl fmt::Display for Update {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let str_rep = match self {
            Self::Add => "add",
            Self::Delete => "del",
        };
        write!(f, "{}", str_rep)
    }
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
        Request::Update {
            req_id,
            update,
            id,
            group,
            user,
        } => {
            if let Err(err) = handle_update(ctx.clone(), req_id, update, id, group, user).await {
                ctx.queue_state
                    .write()
                    .await
                    .insert(req_id, State::Update(StateUpdate::Error(err.to_string())));
            }
        }
        Request::Query { req_id, id, user } => {
            if let Err(err) = handle_query(ctx.clone(), req_id, id, user).await {
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
async fn handle_init(ctx: Arc<Context>, req_id: Uuid) -> Result<()> {
    let set_req_state = async |req_state| {
        ctx.queue_state
            .write()
            .await
            .insert(req_id, State::Init(req_state));
    };

    let latest_membership_list = match db::get_latest_membership_list(&ctx.db_pool).await {
        Ok(Some(membership_list)) => membership_list,
        Ok(None) => db::MembershipList {
            id: 0,
            state: db::DictContainerSql(containers::Dictionary::new(
                ctx.pod_config.params.max_depth_mt_containers,
                Group::iterator()
                    .map(|i| {
                        containers::Set::new(
                            ctx.pod_config.params.max_depth_mt_containers,
                            HashSet::new(),
                        )
                        .map(|s| (Key::from(format!("{}", i)), Value::from(s)))
                    })
                    .collect::<Result<_, _>>()?,
            )?),
        },
        Err(e) => return Err(e.into()),
    };
    let new_id = latest_membership_list.id + 1;

    // Form new dictionary
    let membership_list = db::MembershipList {
        id: new_id,
        state: db::DictContainerSql(containers::Dictionary::new(
            ctx.pod_config.params.max_depth_mt_containers,
            Group::iterator()
                .map(|i| {
                    containers::Set::new(
                        ctx.pod_config.params.max_depth_mt_containers,
                        HashSet::new(),
                    )
                    .map(|s| (Key::from(format!("{}", i)), Value::from(s)))
                })
                .collect::<Result<_, _>>()?,
        )?),
    };

    // send the payload to ethereum
    let payload_bytes = Payload::Init(PayloadInit {
        id: Hash::from(RawValue::from(new_id)), // TODO hash
        custom_predicate_ref: ctx.pod_config.predicates.update.clone(),
        vds_root: ctx.pod_config.vd_set.root(),
    })
    .to_bytes();

    set_req_state(StateInit::SendingBlobTx).await;
    let tx_hash = crate::eth::send_payload(&ctx.cfg, payload_bytes).await?;

    // update db
    db::insert_membership_list(&ctx.db_pool, &membership_list).await?;

    set_req_state(StateInit::Complete {
        id: membership_list.id,
        tx_hash,
    })
    .await;
    Ok(())
}

async fn handle_update(
    ctx: Arc<Context>,
    req_id: Uuid,
    update: Update,
    id: i64,
    group: Group,
    user: Value,
) -> Result<()> {
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

    let start = std::time::Instant::now();

    let mut builder = MainPodBuilder::new(&ctx.pod_config.params, &ctx.pod_config.vd_set);
    let mut helper = Helper::new(&mut builder, &ctx.pod_config.predicates);

    let group_name = format!("{}", group);
    let op = dict!(DEPTH, {"name" => format!("{}",update), "group" => group_name.clone(), "user" => user.clone()}).unwrap();

    let (new_state, st_update) = helper.st_update(state.0.clone(), op);
    builder.reveal(&st_update);

    // sanity check
    println!("set old state: {:?}", state.0);
    println!("set new state: {:?}", new_state);

    // Construct new state
    let mut expected_new_state = state.0.clone();
    let mut group_to_update =
        set_from_value(expected_new_state.get(&Key::from(group_name.clone()))?)?;

    match update {
        Update::Add => group_to_update.insert(&user),
        Update::Delete => group_to_update.delete(&user),
    }?;

    expected_new_state.update(&group_name.into(), &group_to_update.into())?;

    if new_state != expected_new_state {
        // if we're inside this if, means that the pod2 lib has done something
        // wrong, hence, trigger a panic so that we notice it
        panic!(
            "new_state: {:?} != old_state ++ [user]: {:?}",
            new_state, expected_new_state
        );
    }

    set_req_state(StateUpdate::ProvingMainPod).await;
    let prover = Prover {};
    let pod = task::spawn_blocking(move || builder.prove(&prover).unwrap()).await?;
    println!("# pod\n:{}", pod);
    pod.pod.verify().unwrap();

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
    println!("[TIME] ins_set pod {:?}", start.elapsed());

    let payload_bytes = Payload::Update(PayloadUpdate {
        id: Hash::from(RawValue::from(id)), // TODO hash
        proof: compressed_proof,
        new_state: new_state.commitment().into(),
    })
    .to_bytes();

    set_req_state(StateUpdate::SendingBlobTx).await;
    let tx_hash = crate::eth::send_payload(&ctx.cfg, payload_bytes).await?;

    db::update_membership_list(&ctx.db_pool, id, new_state).await?;

    set_req_state(StateUpdate::Complete { tx_hash }).await;
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

fn set_from_value(v: &Value) -> Result<containers::Set> {
    match v.typed() {
        pod2::middleware::TypedValue::Set(s) => Ok(s.clone()),
        _ => Err(anyhow!("Invalid set")),
    }
}
