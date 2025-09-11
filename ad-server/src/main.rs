use std::str::FromStr;

use alloy::primitives::Address;
use anyhow::{Context, Result};
use sqlx::{migrate::MigrateDatabase, sqlite::Sqlite};
use tracing::info;

pub mod db;
pub mod endpoints;
pub mod eth;
pub mod pod;

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
}

impl Config {
    fn from_env() -> Result<Self> {
        fn var(v: &str) -> Result<String> {
            dotenvy::var(v).with_context(|| v.to_string())
        }
        Ok(Self {
            rpc_url: var("RPC_URL")?,
            sqlite_path: var("SQLITE_PATH")?,
            priv_key: var("PRIV_KEY")?,
            to_addr: Address::from_str(&var("TO_ADDR")?)?,
        })
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    common::load_dotenv()?;
    let cfg = Config::from_env()?;
    info!(?cfg, "Loaded config");

    if !Sqlite::database_exists(&cfg.sqlite_path).await? {
        Sqlite::create_database(&cfg.sqlite_path).await?;
    }
    let db_pool = db::db_connection(&cfg.sqlite_path).await?;
    db::init_db(&db_pool).await?;

    let routes = endpoints::routes(cfg, db_pool);
    println!("server at http://0.0.0.0:8000");
    warp::serve(routes).run(([0, 0, 0, 0], 8000)).await;

    Ok(())
}
