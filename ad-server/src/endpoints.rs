use std::sync::Arc;

use app::Index;
use common::CustomError;
use pod2::middleware::Value;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use warp::Filter;

use crate::{Context, db, queue};

// HANDLERS:

// GET /request/{req_id}
pub async fn handler_get_request(
    req_id: Uuid,
    ctx: Arc<Context>,
) -> Result<impl warp::Reply, warp::Rejection> {
    let state = match ctx.queue_state.read().await.get(&req_id).cloned() {
        Some(s) => s,
        None => return Err(CustomError("req_id not found".to_string()).into()),
    };
    Ok(warp::reply::json(&state))
}

// GET /dict/{id}
pub async fn handler_get_dict(
    id: i64,
    ctx: Arc<Context>,
) -> Result<impl warp::Reply, warp::Rejection> {
    let dict = db::get_dict(&ctx.db_pool, id)
        .await
        .map_err(|e| CustomError(e.to_string()))?;
    Ok(warp::reply::json(&dict))
}

// POST /dict
#[derive(Serialize, Deserialize)]
pub struct QueueResp {
    req_id: Uuid,
}

pub async fn handler_new_dict(ctx: Arc<Context>) -> Result<impl warp::Reply, warp::Rejection> {
    let req_id = Uuid::now_v7();
    ctx.queue_state
        .write()
        .await
        .insert(req_id, queue::State::Init(queue::StateInit::Pending));
    ctx.queue_tx
        .send(queue::Request::Init { req_id })
        .await
        .map_err(|e| CustomError(e.to_string()))?;
    Ok(warp::reply::json(&QueueResp { req_id }))
}

// PUT /dict/{id}/{idx}
pub async fn handler_dict_ins(
    id: i64,
    idx: Index,
    user: Value, // user to insert
    ctx: Arc<Context>,
) -> Result<impl warp::Reply, warp::Rejection> {
    let req_id = Uuid::now_v7();
    ctx.queue_state
        .write()
        .await
        .insert(req_id, queue::State::Update(queue::StateUpdate::Pending));
    ctx.queue_tx
        .send(queue::Request::Update {
            req_id,
            update: queue::Update::Insert,
            id,
            idx,
            data: user,
        })
        .await
        .map_err(|e| CustomError(e.to_string()))?;
    Ok(warp::reply::json(&QueueResp { req_id }))
}

// DELETE /dict/{id}/{idx}
pub async fn handler_dict_del(
    id: i64,
    idx: Index,
    user: Value, // user to insert
    ctx: Arc<Context>,
) -> Result<impl warp::Reply, warp::Rejection> {
    let req_id = Uuid::now_v7();
    ctx.queue_state
        .write()
        .await
        .insert(req_id, queue::State::Update(queue::StateUpdate::Pending));
    ctx.queue_tx
        .send(queue::Request::Update {
            req_id,
            update: queue::Update::Delete,
            id,
            idx,
            data: user,
        })
        .await
        .map_err(|e| CustomError(e.to_string()))?;
    Ok(warp::reply::json(&QueueResp { req_id }))
}

// GET /user/{id}/{user}
// TODO: Maybe allow types other than strings?
pub async fn handler_user_get(
    id: i64,
    user: String, // user to insert
    ctx: Arc<Context>,
) -> Result<impl warp::Reply, warp::Rejection> {
    let req_id = Uuid::now_v7();
    ctx.queue_state
        .write()
        .await
        .insert(req_id, queue::State::Query(queue::StateQuery::Pending));
    ctx.queue_tx
        .send(queue::Request::Query {
            req_id,
            id,
            data: Value::from(user),
        })
        .await
        .map_err(|e| CustomError(e.to_string()))?;
    Ok(warp::reply::json(&QueueResp { req_id }))
}

// ROUTES:

// build the routes
pub fn routes(
    ctx: Arc<Context>,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    get_dict(ctx.clone())
        .or(get_request(ctx.clone()))
        .or(new_dict(ctx.clone()))
        .or(dict_insert(ctx.clone()))
        .or(dict_delete(ctx.clone()))
        .or(user_get(ctx.clone()))
}
fn get_request(
    ctx: Arc<Context>,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    warp::path!("request" / Uuid)
        .and(warp::get())
        .and(with_ctx(ctx))
        .and_then(handler_get_request)
}
fn get_dict(
    ctx: Arc<Context>,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    warp::path!("dict" / i64)
        .and(warp::get())
        .and(with_ctx(ctx))
        .and_then(handler_get_dict)
}
fn new_dict(
    ctx: Arc<Context>,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    warp::path!("dict")
        .and(warp::post())
        .and(with_ctx(ctx))
        .and_then(handler_new_dict)
}
fn dict_insert(
    ctx: Arc<Context>,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    warp::path!("dict" / i64 / Index)
        .and(warp::put())
        .and(warp::body::content_length_limit(1024 * 16)) // max 16kb
        .and(warp::body::json())
        .and(with_ctx(ctx))
        .and_then(handler_dict_ins)
}

fn dict_delete(
    ctx: Arc<Context>,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    warp::path!("dict" / i64 / Index)
        .and(warp::delete())
        .and(warp::body::content_length_limit(1024 * 16)) // max 16kb
        .and(warp::body::json())
        .and(with_ctx(ctx))
        .and_then(handler_dict_del)
}

fn user_get(
    ctx: Arc<Context>,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    warp::path!("user" / i64 / String)
        .and(warp::get())
        .and(with_ctx(ctx))
        .and_then(handler_user_get)
}

fn with_ctx(
    ctx: Arc<Context>,
) -> impl Filter<Extract = (Arc<Context>,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || ctx.clone())
}

#[cfg(test)]
mod tests {
    use common::circuits::ShrunkMainPodSetup;
    use pod2::{backends::plonky2::basetypes::DEFAULT_VD_SET, middleware::Params};
    use tokio::{
        sync::mpsc,
        task,
        time::{Duration, sleep},
    };
    use warp::http::StatusCode;

    use super::*;
    use crate::{Config, PodConfig};

    #[tokio::test]
    async fn test_post_pod_success() -> anyhow::Result<()> {
        common::load_dotenv()?;
        let mut cfg = Config::from_env()?;
        cfg.priv_key = "".to_string();

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
        let shrunk_main_pod_build = ShrunkMainPodSetup::new(&params).build()?;
        let pod_config = PodConfig {
            params,
            vd_set: vd_set.clone(),
            predicates,
        };

        let (queue_tx, queue_rx) = mpsc::channel::<queue::Request>(8);
        let ctx = Arc::new(Context::new(
            cfg,
            db_pool,
            pod_config,
            shrunk_main_pod_build,
            queue_tx,
        ));

        let api = routes(ctx.clone());
        task::spawn(async move {
            queue::handle_loop(ctx.clone(), queue_rx).await;
        });

        // create new dict
        let res = warp::test::request()
            .method("POST")
            .path("/dict")
            .reply(&api)
            .await;
        assert_eq!(res.status(), StatusCode::OK);

        // let s = std::str::from_utf8(res.body()).expect("Invalid UTF-8");
        // let received_id: i64 = s.parse()?;
        let resp: QueueResp = serde_json::from_slice(res.body()).expect("");
        loop {
            let res = warp::test::request()
                .method("GET")
                .path(&format!("/request/{}", resp.req_id))
                .reply(&api)
                .await;
            assert_eq!(res.status(), StatusCode::OK);
            let resp: queue::State = serde_json::from_slice(res.body()).expect("");
            match resp {
                queue::State::Init(state_init) => match state_init {
                    queue::StateInit::Complete { id, tx_hash } => {
                        assert_eq!(id, 1); // dict's id always starts at 1
                        assert_eq!(
                            tx_hash.to_string(),
                            "0x0000000000000000000000000000000000000000000000000000000000000000"
                        ); // mock tx hash
                        break;
                    }
                    queue::StateInit::Error(e) => panic!("StateInit::Error: {}", e),
                    _ => sleep(Duration::from_millis(100)).await,
                },
                state => panic!("{:?} != StateInit::Complete", state),
            }
        }

        // augment the dict
        let res = warp::test::request()
            .method("PUT")
            .path("/dict/1/red")
            .json(&Value::from("alice")) // insert "alice" into "red" group
            .reply(&api)
            .await;
        assert_eq!(res.status(), StatusCode::OK);

        let resp: QueueResp = serde_json::from_slice(res.body()).expect("");
        loop {
            let res = warp::test::request()
                .method("GET")
                .path(&format!("/request/{}", resp.req_id))
                .reply(&api)
                .await;
            assert_eq!(res.status(), StatusCode::OK);
            let resp: queue::State = serde_json::from_slice(res.body()).expect("");
            match resp {
                queue::State::Update(state_update) => match state_update {
                    queue::StateUpdate::Complete { tx_hash } => {
                        // should contain the mocked tx hash
                        assert_eq!(
                            tx_hash.to_string(),
                            "0x0000000000000000000000000000000000000000000000000000000000000000"
                        ); // mock tx hash
                        break;
                    }
                    queue::StateUpdate::Error(e) => panic!("StateUpdate::Error: {}", e),
                    _ => sleep(Duration::from_millis(100)).await,
                },
                state => panic!("{:?} != StateUpdate::Complete", state),
            }
        }

        // Query Alice's membership.
        let res = warp::test::request()
            .method("GET")
            .path("/user/1/alice") // Query Alice's membership in the groups of dict 1
            .reply(&api)
            .await;
        assert_eq!(res.status(), StatusCode::OK);

        let resp: QueueResp = serde_json::from_slice(res.body()).expect("");
        loop {
            let res = warp::test::request()
                .method("GET")
                .path(&format!("/request/{}", resp.req_id))
                .reply(&api)
                .await;
            assert_eq!(res.status(), StatusCode::OK);
            let resp: queue::State = serde_json::from_slice(res.body()).expect("");
            match resp {
                queue::State::Query(state_query) => match state_query {
                    queue::StateQuery::Complete { result } => {
                        println!("{:?}", result);
                        break;
                    }
                    queue::StateQuery::Error(e) => panic!("StateQuery::Error: {}", e),
                    _ => sleep(Duration::from_millis(100)).await,
                },
                state => panic!("{:?} != StateQuery::Complete", state),
            }
        }

        // Delete Alice.
        let res = warp::test::request()
            .method("DELETE")
            .path("/dict/1/red")
            .json(&Value::from("alice")) // remove "alice" from "red" group
            .reply(&api)
            .await;
        assert_eq!(res.status(), StatusCode::OK);

        let resp: QueueResp = serde_json::from_slice(res.body()).expect("");
        loop {
            let res = warp::test::request()
                .method("GET")
                .path(&format!("/request/{}", resp.req_id))
                .reply(&api)
                .await;
            assert_eq!(res.status(), StatusCode::OK);
            let resp: queue::State = serde_json::from_slice(res.body()).expect("");
            match resp {
                queue::State::Update(state_update) => match state_update {
                    queue::StateUpdate::Complete { tx_hash } => {
                        // should contain the mocked tx hash
                        assert_eq!(
                            tx_hash.to_string(),
                            "0x0000000000000000000000000000000000000000000000000000000000000000"
                        ); // mock tx hash
                        break;
                    }
                    queue::StateUpdate::Error(e) => panic!("StateUpdate::Error: {}", e),
                    _ => sleep(Duration::from_millis(100)).await,
                },
                state => panic!("{:?} != StateUpdate::Complete", state),
            }
        }

        Ok(())
    }
}
