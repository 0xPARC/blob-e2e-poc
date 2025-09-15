use common::CustomError;
use warp::Filter;

use crate::Node;

// HANDLERS:

// GET /ad_state/{id}
pub(crate) async fn handler_get_ad_state(
    ad_id_str: String,
    node: Node,
) -> Result<impl warp::Reply, warp::Rejection> {
    let ad_id = hex::decode(&ad_id_str).map_err(|e| CustomError(e.to_string()))?;
    let ad_state = node
        .db_get_ad_state(&ad_id)
        .await
        .map_err(|e| CustomError(e.to_string()))?;
    Ok(warp::reply::json(&ad_state))
}

// ROUTES:

// build the routes
pub(crate) fn routes(
    node: Node,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    get_ad_state(node)
}
fn get_ad_state(
    node: Node,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    let node_filter = warp::any().map(move || node.clone());

    warp::path!("ad_state" / String)
        .and(warp::get())
        .and(node_filter)
        .and_then(handler_get_ad_state)
}
