use std::{collections::HashSet, sync::Arc};

use app::{DEPTH, Helper};
use common::{
    CustomError,
    circuits::{ShrunkMainPodBuild, shrink_compress_pod},
    payload::{Payload, PayloadInit, PayloadUpdate},
};
use pod2::{
    backends::plonky2::mainpod::Prover,
    dict,
    frontend::MainPodBuilder,
    middleware::{Hash, RawValue, Value, containers},
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use warp::Filter;

use crate::{Config, PodConfig, db};

// HANDLERS:

// GET /set/{id}
pub async fn handler_get_set(
    id: i64,
    db_pool: SqlitePool,
) -> Result<impl warp::Reply, warp::Rejection> {
    let set = db::get_set(&db_pool, id)
        .await
        .map_err(|e| CustomError(e.to_string()))?;
    Ok(warp::reply::json(&set))
}

// POST /set
#[derive(Serialize, Deserialize)]
pub struct NewSetResp {
    id: i64,
    tx_hash: alloy::primitives::TxHash,
}
pub async fn handler_new_set(
    cfg: Config,
    db_pool: SqlitePool,
    pod_config: PodConfig,
) -> Result<impl warp::Reply, warp::Rejection> {
    let latest_set = match sqlx::query_as::<_, db::Set>(
        "SELECT id, set_container FROM sets ORDER BY id DESC LIMIT 1",
    )
    .fetch_optional(&db_pool)
    .await
    {
        Ok(Some(set)) => set,
        Ok(None) => db::Set {
            id: 0,
            set_container: db::SetContainerSql(
                containers::Set::new(pod_config.params.max_depth_mt_containers, HashSet::new())
                    .expect("Should be able to construct empty set."),
            ),
        },
        Err(e) => return Err(warp::reject::custom(CustomError(e.to_string()))),
    };
    let new_id = latest_set.id + 1;

    // send the payload to ethereum
    let payload_bytes = Payload::Init(PayloadInit {
        id: Hash::from(RawValue::from(new_id)), // TODO hash
        custom_predicate_ref: pod_config.predicates.update_pred,
        vds_root: pod_config.vd_set.root(),
    })
    .to_bytes();

    let tx_hash = crate::eth::send_payload(cfg, payload_bytes)
        .await
        .map_err(|e| CustomError(e.to_string()))?;

    // update db
    let set = db::Set {
        id: new_id,
        set_container: db::SetContainerSql(
            containers::Set::new(pod_config.params.max_depth_mt_containers, HashSet::new())
                .expect("Should be able to construct empty set."),
        ),
    };
    db::insert_set(&db_pool, &set)
        .await
        .map_err(|e| CustomError(e.to_string()))?;

    Ok(warp::reply::json(&NewSetResp {
        id: set.id,
        tx_hash,
    }))
}

// POST /set/{id}
pub async fn handler_set_ins(
    id: i64,
    data: Value, // data to insert
    cfg: Config,
    db_pool: SqlitePool,
    pod_config: PodConfig,
    shrunk_main_pod_build: Arc<ShrunkMainPodBuild>,
) -> Result<impl warp::Reply, warp::Rejection> {
    // TODO: Data validation

    // get state from db
    let set = db::get_set(&db_pool, id)
        .await
        .map_err(|e| CustomError(e.to_string()))?;

    // with the actual POD
    let state = set.set_container;

    let start = std::time::Instant::now();

    let mut builder = MainPodBuilder::new(&pod_config.params, &pod_config.vd_set);
    let mut helper = Helper::new(&mut builder, &pod_config.predicates);

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

    let prover = Prover {};
    let pod = builder.prove(&prover).unwrap();
    println!("# pod\n:{}", pod);
    pod.pod.verify().unwrap();

    let compressed_proof = shrink_compress_pod(&shrunk_main_pod_build, pod).unwrap();
    println!("[TIME] ins_set pod {:?}", start.elapsed());

    let payload_bytes = Payload::Update(PayloadUpdate {
        id: Hash::from(RawValue::from(id)), // TODO hash
        shrunk_main_pod_proof: compressed_proof,
        new_state: new_state.commitment().into(),
    })
    .to_bytes();

    let tx_hash = crate::eth::send_payload(cfg, payload_bytes)
        .await
        .map_err(|e| CustomError(e.to_string()))?;

    db::update_set(&db_pool, id, new_state)
        .await
        .map_err(|e| CustomError(e.to_string()))?;

    Ok(warp::reply::json(&tx_hash))
}

// ROUTES:

// build the routes
pub fn routes(
    cfg: Config,
    db_pool: SqlitePool,
    pod_config: PodConfig,
    shrunk_main_pod_build: Arc<ShrunkMainPodBuild>,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    get_set(db_pool.clone())
        .or(new_set(cfg.clone(), db_pool.clone(), pod_config.clone()))
        .or(set_insert(cfg, db_pool, pod_config, shrunk_main_pod_build))
}
fn get_set(
    db_pool: SqlitePool,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    let db_filter = warp::any().map(move || db_pool.clone());

    warp::path!("set" / i64)
        .and(warp::get())
        .and(db_filter)
        .and_then(handler_get_set)
}
fn new_set(
    cfg: Config,
    db_pool: SqlitePool,
    pod_config: PodConfig,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    let db_filter = warp::any().map(move || db_pool.clone());

    warp::path!("set")
        .and(warp::post())
        .and(with_config(cfg))
        .and(db_filter)
        .and(with_pod_config(pod_config))
        .and_then(handler_new_set)
}
fn set_insert(
    cfg: Config,
    db_pool: SqlitePool,
    pod_config: PodConfig,
    shrunk_main_pod_build: Arc<ShrunkMainPodBuild>,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    let db_filter = warp::any().map(move || db_pool.clone());

    warp::path!("set" / i64)
        .and(warp::post())
        .and(warp::body::content_length_limit(1024 * 16)) // max 16kb
        .and(warp::body::json())
        .and(with_config(cfg))
        .and(db_filter)
        .and(with_pod_config(pod_config))
        .and(with_shrunk_main_pod_build(shrunk_main_pod_build))
        .and_then(handler_set_ins)
}

fn with_config(
    cfg: Config,
) -> impl Filter<Extract = (Config,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || cfg.clone())
}
fn with_pod_config(
    pod_config: PodConfig,
) -> impl Filter<Extract = (PodConfig,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || pod_config.clone())
}
fn with_shrunk_main_pod_build(
    shrunk_main_pod_build: Arc<ShrunkMainPodBuild>,
) -> impl Filter<Extract = (Arc<ShrunkMainPodBuild>,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || shrunk_main_pod_build.clone())
}

#[cfg(test)]
mod tests {
    use common::circuits::ShrunkMainPodSetup;
    use pod2::{backends::plonky2::basetypes::DEFAULT_VD_SET, middleware::Params};
    use warp::http::StatusCode;

    use super::*;

    #[tokio::test]
    async fn test_post_pod_success() -> anyhow::Result<()> {
        println!("!!!{}", serde_json::to_string(&Value::from(5)).unwrap());
        common::load_dotenv()?;
        let cfg = Config::from_env()?;

        let db_pool = sqlx::sqlite::SqlitePoolOptions::new()
            .min_connections(1) // db config for tests
            .max_connections(1)
            .idle_timeout(None)
            .max_lifetime(None)
            .connect(":memory:")
            .await
            .expect("cannot connect to db");
        db::init_db(&db_pool).await?;

        // initialize pod data
        let params = Params::default();
        println!("Prebuilding circuits to calculate vd_set...");
        let vd_set = &*DEFAULT_VD_SET;
        println!("vd_set calculation complete");
        let predicates = app::build_predicates(&params);
        let shrunk_main_pod_build = Arc::new(ShrunkMainPodSetup::new(&params).build()?);
        let pod_config = PodConfig {
            params,
            vd_set: vd_set.clone(),
            predicates,
        };

        let api = routes(cfg, db_pool, pod_config, shrunk_main_pod_build);

        // create new set
        let res = warp::test::request()
            .method("POST")
            .path("/set")
            .reply(&api)
            .await;
        assert_eq!(res.status(), StatusCode::OK);

        // let s = std::str::from_utf8(res.body()).expect("Invalid UTF-8");
        // let received_id: i64 = s.parse()?;
        let resp: NewSetResp = serde_json::from_slice(res.body()).expect("");
        assert_eq!(resp.id, 1); // set's id always starts at 1
        assert_eq!(
            resp.tx_hash.to_string(),
            "0x0000000000000000000000000000000000000000000000000000000000000000"
        ); // mock tx hash

        // augment the set
        let res = warp::test::request()
            .method("POST")
            .path("/set/1")
            .json(&Value::from(3)) // insert 3
            .reply(&api)
            .await;
        assert_eq!(res.status(), StatusCode::OK);

        // the body should contain the mocked tx hash
        let body: String = serde_json::from_slice(res.body()).unwrap();
        assert_eq!(
            body,
            "0x0000000000000000000000000000000000000000000000000000000000000000"
        );

        Ok(())
    }
}
