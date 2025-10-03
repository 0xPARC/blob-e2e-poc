use core::fmt;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use alloy::primitives::TxHash;
use anyhow::{Result, anyhow};
use app::{DEPTH, Helper, Index};
use common::{
    circuits::shrink_compress_pod,
    payload::{Payload, PayloadInit, PayloadUpdate},
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
    ShrinkingMainPod,
    SendingBlobTx,
    Complete { tx_hash: TxHash },
    Error(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum StateQuery {
    Pending,
    ProvingMainPod,
    ShrinkingMainPod,
    SendingBlobTx,
    Complete {
        result: HashMap<Index, MerkleClaimAndProof>,
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
        idx: Index,
        data: Value,
    },
    Query {
        req_id: Uuid,
        id: i64,
        data: Value,
    },
}

#[derive(Debug)]
pub enum Update {
    Insert,
    Delete,
}

impl fmt::Display for Update {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let str_rep = match self {
            Self::Insert => "add",
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
            idx,
            data,
        } => {
            if let Err(err) = handle_update(ctx.clone(), req_id, update, id, idx, data).await {
                ctx.queue_state
                    .write()
                    .await
                    .insert(req_id, State::Update(StateUpdate::Error(err.to_string())));
            }
        }
        Request::Query { req_id, id, data } => {
            if let Err(err) = handle_query(ctx.clone(), req_id, id, data).await {
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
    let dict_state = async |state| {
        ctx.queue_state
            .write()
            .await
            .insert(req_id, State::Init(state));
    };

    let latest_dict = match sqlx::query_as::<_, db::Dict>(
        "SELECT id, dict_container FROM dicts ORDER BY id DESC LIMIT 1",
    )
    .fetch_optional(&ctx.db_pool)
    .await
    {
        Ok(Some(dict)) => dict,
        Ok(None) => db::Dict {
            id: 0,
            dict_container: db::DictContainerSql(containers::Dictionary::new(
                ctx.pod_config.params.max_depth_mt_containers,
                Index::iterator()
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
    let new_id = latest_dict.id + 1;

    // Form new dictionary
    let dict = db::Dict {
        id: new_id,
        dict_container: db::DictContainerSql(containers::Dictionary::new(
            ctx.pod_config.params.max_depth_mt_containers,
            Index::iterator()
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

    dict_state(StateInit::SendingBlobTx).await;
    let tx_hash = crate::eth::send_payload(&ctx.cfg, payload_bytes).await?;

    // update db
    db::insert_dict(&ctx.db_pool, &dict).await?;

    dict_state(StateInit::Complete {
        id: dict.id,
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
    idx: Index,
    user: Value,
) -> Result<()> {
    let dict_state = async |state| {
        ctx.queue_state
            .write()
            .await
            .insert(req_id, State::Update(state));
    };
    // TODO: Data validation

    // get state from db
    let dict = db::get_dict(&ctx.db_pool, id).await?;

    // with the actual POD
    let state = dict.dict_container;

    let start = std::time::Instant::now();

    let mut builder = MainPodBuilder::new(&ctx.pod_config.params, &ctx.pod_config.vd_set);
    let mut helper = Helper::new(&mut builder, &ctx.pod_config.predicates);

    let group_name = format!("{}", idx);
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
        Update::Insert => group_to_update.insert(&user),
        Update::Delete => group_to_update.delete(&user),
    }?;

    expected_new_state.update(&group_name.into(), &group_to_update.into())?;

    if new_state != expected_new_state {
        // if we're inside this if, means that the pod2 lib has done something
        // wrong, hence, trigger a panic so that we notice it
        panic!(
            "new_state: {:?} != old_state ++ [data]: {:?}",
            new_state, expected_new_state
        );
    }

    dict_state(StateUpdate::ProvingMainPod).await;
    let prover = Prover {};
    let pod = task::spawn_blocking(move || builder.prove(&prover).unwrap()).await?;
    println!("# pod\n:{}", pod);
    pod.pod.verify().unwrap();

    dict_state(StateUpdate::ShrinkingMainPod).await;
    let compressed_proof = {
        let ctx = ctx.clone();
        task::spawn_blocking(move || shrink_compress_pod(&ctx.shrunk_main_pod_build, pod).unwrap())
            .await?
    };
    println!("[TIME] ins_set pod {:?}", start.elapsed());

    let payload_bytes = Payload::Update(PayloadUpdate {
        id: Hash::from(RawValue::from(id)), // TODO hash
        shrunk_main_pod_proof: compressed_proof,
        new_state: new_state.commitment().into(),
    })
    .to_bytes();

    dict_state(StateUpdate::SendingBlobTx).await;
    let tx_hash = crate::eth::send_payload(&ctx.cfg, payload_bytes).await?;

    db::update_dict(&ctx.db_pool, id, new_state).await?;

    dict_state(StateUpdate::Complete { tx_hash }).await;
    Ok(())
}

async fn handle_query(ctx: Arc<Context>, req_id: Uuid, id: i64, data: Value) -> Result<()> {
    let dict_state = async |state| {
        ctx.queue_state
            .write()
            .await
            .insert(req_id, State::Query(state));
    };

    // get state from db
    let dict = db::get_dict(&ctx.db_pool, id).await?.dict_container.0;

    let dict_kvs = dict
        .kvs()
        .iter()
        .map(|(idx, v)| {
            set_from_value(v).and_then(|s| {
                idx.name()
                    .try_into()
                    .map_err(|_| anyhow!("Invalid group: {}", idx))
                    .map(|idx| (idx, s))
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let result = dict_kvs
        .into_iter()
        .filter_map(|(idx, s)| {
            s.contains(&data).then(|| {
                s.prove(&data)
                    .map(|proof| {
                        (
                            idx,
                            MerkleClaimAndProof {
                                root: s.commitment(),
                                key: data.raw(),
                                value: data.raw(),
                                proof,
                            },
                        )
                    })
                    .map_err(|e| e.into())
            })
        })
        .collect::<Result<HashMap<_, _>>>()?;

    dict_state(StateQuery::Complete { result }).await;

    Ok(())
}

fn set_from_value(v: &Value) -> Result<containers::Set> {
    match v.typed() {
        pod2::middleware::TypedValue::Set(s) => Ok(s.clone()),
        _ => Err(anyhow!("Invalid set")),
    }
}
