use std::sync::Arc;

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
    middleware::{Hash, RawValue},
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use warp::Filter;

use crate::{Config, PodConfig, db};

// HANDLERS:

// GET /counter/{id}
pub async fn handler_get_counter(
    id: i64,
    db_pool: SqlitePool,
) -> Result<impl warp::Reply, warp::Rejection> {
    let counter = db::get_counter(&db_pool, id)
        .await
        .map_err(|e| CustomError(e.to_string()))?;
    Ok(warp::reply::json(&counter))
}

// POST /counter
#[derive(Serialize, Deserialize)]
pub struct NewCounterResp {
    id: i64,
    tx_hash: alloy::primitives::TxHash,
}
pub async fn handler_new_counter(
    cfg: Config,
    db_pool: SqlitePool,
    pod_config: PodConfig,
) -> Result<impl warp::Reply, warp::Rejection> {
    let latest_counter = match sqlx::query_as::<_, db::Counter>(
        "SELECT id, count FROM counters ORDER BY id DESC LIMIT 1",
    )
    .fetch_optional(&db_pool)
    .await
    {
        Ok(Some(counter)) => counter,
        Ok(None) => db::Counter { id: 0, count: 0 },
        Err(e) => return Err(warp::reject::custom(CustomError(e.to_string()))),
    };
    let new_id = latest_counter.id + 1;

    // send the payload to ethereum
    let payload_bytes = Payload::Init(PayloadInit {
        id: Hash::from(RawValue::from(new_id)), // TODO hash
        custom_predicate_ref: pod_config.predicates.update_loop_pred,
        vds_root: pod_config.vd_set.root(),
    })
    .to_bytes();

    let tx_hash = crate::eth::send_payload(cfg, payload_bytes)
        .await
        .map_err(|e| CustomError(e.to_string()))?;

    // update db
    let counter = db::Counter {
        id: new_id,
        count: 0,
    };
    db::insert_counter(&db_pool, &counter)
        .await
        .map_err(|e| CustomError(e.to_string()))?;

    Ok(warp::reply::json(&NewCounterResp {
        id: counter.id,
        tx_hash,
    }))
}

// POST /counter/{id}
pub async fn handler_incr_counter(
    id: i64,
    count: i64, // delta to increment the counter
    cfg: Config,
    db_pool: SqlitePool,
    pod_config: PodConfig,
    shrunk_main_pod_build: Arc<ShrunkMainPodBuild>,
) -> Result<impl warp::Reply, warp::Rejection> {
    if count >= 10 {
        return Err(warp::reject::custom(CustomError(format!(
            "max count is 9, got count={}",
            count
        ))));
    }

    // get counter from db
    let counter = db::get_counter(&db_pool, id)
        .await
        .map_err(|e| CustomError(e.to_string()))?;

    // with the actual POD
    let state = counter.count;

    let start = std::time::Instant::now();

    let mut builder = MainPodBuilder::new(&pod_config.params, &pod_config.vd_set);
    let mut helper = Helper::new(&mut builder, &pod_config.predicates);

    let op = dict!(DEPTH, {"name" => "inc", "n" => count}).unwrap();

    let (new_state, st_update) = helper.st_update(state, &[op]);
    builder.reveal(&st_update);

    // sanity check
    println!("counter old state: {}", state);
    println!("counter new state: {new_state}");
    if new_state != counter.count + count {
        // if we're inside this if, means that the pod2 lib has done something
        // wrong, hence, trigger a panic so that we notice it
        panic!(
            "new_state: {} != counter.count+count: {}",
            new_state,
            counter.count + count
        );
    }

    let prover = Prover {};
    let pod = builder.prove(&prover).unwrap();
    println!("# pod\n:{}", pod);
    pod.pod.verify().unwrap();

    let compressed_proof = shrink_compress_pod(&shrunk_main_pod_build, pod).unwrap();
    println!("[TIME] incr_counter pod {:?}", start.elapsed());

    let payload_bytes = Payload::Update(PayloadUpdate {
        id: Hash::from(RawValue::from(id)), // TODO hash
        shrunk_main_pod_proof: compressed_proof,
        new_state: RawValue::from(new_state),
    })
    .to_bytes();

    let tx_hash = crate::eth::send_payload(cfg, payload_bytes)
        .await
        .map_err(|e| CustomError(e.to_string()))?;

    db::update_count(&db_pool, id, counter.count + count)
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
    get_counter(db_pool.clone())
        .or(new_counter(
            cfg.clone(),
            db_pool.clone(),
            pod_config.clone(),
        ))
        .or(increment_counter(
            cfg,
            db_pool,
            pod_config,
            shrunk_main_pod_build,
        ))
}
fn get_counter(
    db_pool: SqlitePool,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    let db_filter = warp::any().map(move || db_pool.clone());

    warp::path!("counter" / i64)
        .and(warp::get())
        .and(db_filter)
        .and_then(handler_get_counter)
}
fn new_counter(
    cfg: Config,
    db_pool: SqlitePool,
    pod_config: PodConfig,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    let db_filter = warp::any().map(move || db_pool.clone());

    warp::path!("counter")
        .and(warp::post())
        .and(with_config(cfg))
        .and(db_filter)
        .and(with_pod_config(pod_config))
        .and_then(handler_new_counter)
}
fn increment_counter(
    cfg: Config,
    db_pool: SqlitePool,
    pod_config: PodConfig,
    shrunk_main_pod_build: Arc<ShrunkMainPodBuild>,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    let db_filter = warp::any().map(move || db_pool.clone());

    warp::path!("counter" / i64)
        .and(warp::post())
        .and(warp::body::content_length_limit(1024 * 16)) // max 16kb
        .and(warp::body::json())
        .and(with_config(cfg))
        .and(db_filter)
        .and(with_pod_config(pod_config))
        .and(with_shrunk_main_pod_build(shrunk_main_pod_build))
        .and_then(handler_incr_counter)
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

        // set new counter
        let res = warp::test::request()
            .method("POST")
            .path("/counter")
            .reply(&api)
            .await;
        assert_eq!(res.status(), StatusCode::OK);

        // let s = std::str::from_utf8(res.body()).expect("Invalid UTF-8");
        // let received_id: i64 = s.parse()?;
        let resp: NewCounterResp = serde_json::from_slice(res.body()).expect("");
        assert_eq!(resp.id, 1); // counter's id always start at 1
        assert_eq!(
            resp.tx_hash.to_string(),
            "0x0000000000000000000000000000000000000000000000000000000000000000"
        ); // mock tx hash

        // increment the counter
        let res = warp::test::request()
            .method("POST")
            .path("/counter/1")
            .json(&3) // increment 3
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
