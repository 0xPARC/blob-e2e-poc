#![allow(clippy::uninlined_format_args)]
use std::{str::FromStr, sync::Arc};

use alloy::primitives::Address;
use anyhow::{Context, Result};
use app::{Predicates, build_predicates};
use common::circuits::ShrunkMainPodSetup;
use pod2::{
    backends::plonky2::basetypes::DEFAULT_VD_SET,
    middleware::{Params, VDSet},
};
use sqlx::{migrate::MigrateDatabase, sqlite::Sqlite};
use tracing::info;

pub mod db;
pub mod endpoints;
pub mod eth;

#[derive(Debug, Clone)]
pub struct Config {
    // The URL for the Ethereum RPC API
    pub rpc_url: String,
    // The path to the sqlite database (it will be a file)
    pub sqlite_path: String,
    // Ethereum private key to send txs
    pub priv_key: String,
    // The address that receives AD update via blobs
    pub to_addr: Address,
    pub tx_watch_timeout: u64,
}

impl Config {
    fn from_env() -> Result<Self> {
        fn var(v: &str) -> Result<String> {
            dotenvy::var(v).with_context(|| v.to_string())
        }
        Ok(Self {
            rpc_url: var("RPC_URL")?,
            sqlite_path: var("AD_SERVER_SQLITE_PATH")?,
            priv_key: var("PRIV_KEY")?,
            to_addr: Address::from_str(&var("TO_ADDR")?)?,
            tx_watch_timeout: u64::from_str(&var("TX_WATCH_TIMEOUT")?)?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct PodConfig {
    params: Params,
    vd_set: VDSet,
    predicates: Predicates,
}

#[tokio::main]
async fn main() -> Result<()> {
    common::load_dotenv()?;
    let cfg = Config::from_env()?;
    info!(?cfg, "Loaded config");

    // initialize db
    if !Sqlite::database_exists(&cfg.sqlite_path).await? {
        Sqlite::create_database(&cfg.sqlite_path).await?;
    }
    let db_pool = db::db_connection(&cfg.sqlite_path).await?;
    db::init_db(&db_pool).await?;

    // initialize pod data
    let params = Params::default();
    println!("Prebuilding circuits to calculate vd_set...");
    let vd_set = &*DEFAULT_VD_SET;
    println!("vd_set calculation complete");
    let predicates = build_predicates(&params);
    let shrunk_main_pod_build = Arc::new(ShrunkMainPodSetup::new(&params).build()?);
    let pod_config = PodConfig {
        params,
        vd_set: vd_set.clone(),
        predicates,
    };

    let routes = endpoints::routes(cfg, db_pool, pod_config, shrunk_main_pod_build);
    println!("server at http://0.0.0.0:8000");
    warp::serve(routes).run(([0, 0, 0, 0], 8000)).await;

    Ok(())
}
