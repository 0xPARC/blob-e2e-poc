use sqlx::{FromRow, SqlitePool};
use warp::Filter;

use crate::db;

// TODO rm all unwraps

// HANDLERS:

// GET /counter/{id}
pub async fn handler_get_counter(
    id: i64,
    db_pool: SqlitePool,
) -> Result<impl warp::Reply, warp::Rejection> {
    let counter = db::get_counter(&db_pool, id).await.unwrap();
    Ok(warp::reply::json(&counter))
}

// POST /counter
pub async fn handler_new_counter(db_pool: SqlitePool) -> Result<impl warp::Reply, warp::Rejection> {
    let latest_counter = match sqlx::query_as::<_, db::Counter>(
        "SELECT id, count FROM counters ORDER BY id DESC LIMIT 1",
    )
    .fetch_optional(&db_pool)
    .await
    {
        Ok(Some(counter)) => counter,
        Ok(None) => db::Counter { id: 0, count: 0 },
        Err(e) => panic!("TODO {}", e),
    };

    let counter = db::Counter {
        id: latest_counter.id + 1,
        count: 0,
    };
    db::insert_counter(&db_pool, &counter).await.unwrap();
    Ok(warp::reply::json(&counter.id))
}

// POST /counter/{id}
pub async fn handler_incr_counter(
    id: i64,
    count: i64,
    db_pool: SqlitePool,
) -> Result<impl warp::Reply, warp::Rejection> {
    assert!(count < 10);

    // TODO work with an actual POD

    // update db value TODO do this in a single db operation
    let counter = db::get_counter(&db_pool, id).await.unwrap();
    db::update_count(&db_pool, id, counter.count + count)
        .await
        .unwrap();

    // the next block of code is temporal, generates a plonky2 proof to simulate
    // the rest of the flow. To be replaced by actual POD proofs
    let (vd, ccd, p) = crate::pod::simple_circuit().unwrap();
    dbg!("simple_circuit proof generated");
    let (verifier_data, common_circuit_data, proof_with_pis) =
        crate::pod::shrink_proof(vd.verifier_only, ccd, p).unwrap();
    dbg!("shrink_proof done");
    let compressed_proof = proof_with_pis
        .compress(
            &verifier_data.verifier_only.circuit_digest,
            &common_circuit_data.common,
        )
        .unwrap();
    let proof_bytes = compressed_proof.to_bytes();

    let tx_hash = crate::eth::send_pod_proof(proof_bytes).await.unwrap();
    Ok(warp::reply::json(&tx_hash))
}

// ROUTES:

// build the routes
pub fn routes(
    db_pool: SqlitePool,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    get_counter(db_pool.clone())
        .or(new_counter(db_pool.clone()))
        .or(increment_counter(db_pool.clone()))
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
    db_pool: SqlitePool,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    let db_filter = warp::any().map(move || db_pool.clone());

    warp::path!("counter")
        .and(warp::post())
        .and(db_filter)
        .and_then(handler_new_counter)
}
fn increment_counter(
    db_pool: SqlitePool,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    let db_filter = warp::any().map(move || db_pool.clone());

    warp::path!("counter" / i64)
        .and(warp::post())
        .and(warp::body::content_length_limit(1024 * 16)) // max 16kb
        .and(warp::body::json())
        .and(db_filter)
        .and_then(handler_incr_counter)
}

#[cfg(test)]
mod tests {
    use super::*;
    use warp::http::StatusCode;

    #[tokio::test]
    async fn test_post_pod_success() -> anyhow::Result<()> {
        let db_pool = sqlx::sqlite::SqlitePoolOptions::new()
            .min_connections(1) // db config for tests
            .max_connections(1)
            .idle_timeout(None)
            .max_lifetime(None)
            .connect(":memory:")
            .await
            .expect("cannot connect to db");
        db::init_db(&db_pool).await?;

        let api = routes(db_pool);

        // set new counter
        let res = warp::test::request()
            .method("POST")
            .path("/counter")
            .reply(&api)
            .await;
        assert_eq!(res.status(), StatusCode::OK);
        dbg!(res.body());
        let s = std::str::from_utf8(res.body()).expect("Invalid UTF-8");
        let received_id: i64 = s.parse()?;
        assert_eq!(received_id, 1); // counter's id always start at 1

        // increment the counter
        let res = warp::test::request()
            .method("POST")
            .path("/counter/1")
            .json(&1)
            .reply(&api)
            .await;
        assert_eq!(res.status(), StatusCode::OK);

        // The body should contain the mocked tx hash
        let body: String = serde_json::from_slice(res.body()).unwrap();
        assert_eq!(
            body,
            "0x0000000000000000000000000000000000000000000000000000000000000000"
        );

        Ok(())
    }
}
