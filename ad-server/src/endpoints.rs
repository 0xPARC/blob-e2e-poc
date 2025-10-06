use std::sync::Arc;

use app::Op;
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

// GET /membership_list/{id}
pub async fn handler_get_membership_list(
    id: i64,
    ctx: Arc<Context>,
) -> Result<impl warp::Reply, warp::Rejection> {
    let membership_list = db::get_membership_list(&ctx.db_pool, id)
        .await
        .map_err(|e| CustomError(e.to_string()))?;
    Ok(warp::reply::json(&membership_list))
}

#[derive(Serialize, Deserialize)]
pub struct QueueResp {
    req_id: Uuid,
}

// POST /membership_list
pub async fn handler_new_membership_list(
    ctx: Arc<Context>,
) -> Result<impl warp::Reply, warp::Rejection> {
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

// POST /membership_list/{id}
pub async fn handler_membership_list_update(
    id: i64,
    op: Op,
    ctx: Arc<Context>,
) -> Result<impl warp::Reply, warp::Rejection> {
    let req_id = Uuid::now_v7();
    ctx.queue_state
        .write()
        .await
        .insert(req_id, queue::State::Update(queue::StateUpdate::Pending));
    ctx.queue_tx
        .send(queue::Request::Update { req_id, id, op })
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
            user: Value::from(user),
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
    get_membership_list(ctx.clone())
        .or(get_request(ctx.clone()))
        .or(new_membership_list(ctx.clone()))
        .or(membership_list_update(ctx.clone()))
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
fn get_membership_list(
    ctx: Arc<Context>,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    warp::path!("membership_list" / i64)
        .and(warp::get())
        .and(with_ctx(ctx))
        .and_then(handler_get_membership_list)
}
fn new_membership_list(
    ctx: Arc<Context>,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    warp::path!("membership_list")
        .and(warp::post())
        .and(with_ctx(ctx))
        .and_then(handler_new_membership_list)
}
fn membership_list_update(
    ctx: Arc<Context>,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    warp::path!("membership_list" / i64)
        .and(warp::post())
        .and(warp::body::content_length_limit(1024 * 16)) // max 16kb
        .and(warp::body::json())
        .and(with_ctx(ctx))
        .and_then(handler_membership_list_update)
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
    use app::Group;
    use common::shrink::ShrunkMainPodSetup;
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

        // create new membership_list
        let res = warp::test::request()
            .method("POST")
            .path("/membership_list")
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
                        assert_eq!(id, 1); // membership_list's id always starts at 1
                        assert_eq!(
                            // mock tx hash
                            tx_hash.to_string(),
                            "0x0000000000000000000000000000000000000000000000000000000000000000"
                        );
                        break;
                    }
                    queue::StateInit::Error(e) => panic!("StateInit::Error: {}", e),
                    _ => sleep(Duration::from_millis(100)).await,
                },
                state => panic!("{:?} != StateInit::Complete", state),
            }
        }

        // augment the membership_list
        let res = warp::test::request()
            .method("POST")
            .path("/membership_list/1")
            .json(&Op::Add {
                group: Group::Red,
                user: "alice".to_string(),
            }) // insert "alice" into "red" group
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
            .path("/user/1/alice") // Query Alice's membership in the groups of membership_list 1
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
            .method("POST")
            .path("/membership_list/1")
            .json(&Op::Del {
                group: Group::Red,
                user: "alice".to_string(),
            }) // remove "alice" from "red" group
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
