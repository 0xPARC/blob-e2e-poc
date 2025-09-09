use anyhow::{anyhow, Result};
use log::LevelFilter;
use sqlx::migrate::MigrateDatabase;
use sqlx::sqlite::{Sqlite, SqliteConnectOptions};
use sqlx::ConnectOptions;
use sqlx::SqlitePool;
use std::io;
use std::str::FromStr;
use std::time::Duration;

fn load_dotenv() -> Result<()> {
    for filename in [".env.default", ".env"] {
        if let Err(err) = dotenvy::from_filename_override(filename) {
            match err {
                dotenvy::Error::Io(e) if e.kind() == io::ErrorKind::NotFound => {}
                _ => return Err(err)?,
            }
        }
    }
    Ok(())
}

#[derive(Debug)]
pub struct Config {
    pub beacon_url: String,
    pub sqlite_path: String,
    pub sync_start_slot: u64,
}

impl Config {
    fn from_env() -> Result<Self> {
        fn var(v: &str) -> Result<String> {
            match dotenvy::var(v) {
                Err(e) => Err(anyhow!("{} \"{}\"", e, v)),
                Ok(s) => Ok(s),
            }
        }
        Ok(Self {
            beacon_url: var("BEACON_URL")?,
            sqlite_path: var("SQLITE_PATH")?,
            sync_start_slot: u64::from_str(&var("SYNC_START_SLOT")?)?,
        })
    }
}

async fn db_connection(url: &str) -> Result<SqlitePool, sqlx::Error> {
    let opts = SqliteConnectOptions::from_str(url)?
        // https://docs.rs/sqlx/latest/sqlx/sqlite/struct.SqliteConnectOptions.html#method.serialized
        // > Setting this to true may help if you are getting access violation errors or
        // segmentation faults, but will also incur a significant performance penalty. You should
        // leave this set to false if at all possible.
        .serialized(false)
        .busy_timeout(Duration::from_secs(3600))
        .log_statements(LevelFilter::Debug)
        .log_slow_statements(LevelFilter::Warn, Duration::from_millis(800));
    Ok(SqlitePool::connect_with(opts).await?)
}

// #[derive(Debug)]
// struct Client {
//     url: String,
// }

// impl Client {
//     pub async fn get_blobs(&self, block_id: BlockId) -> Result<Vec<BlobData>> {
//         let req_url = format!("{}/eth/v1/beacon/blob_sidecars/{}", beacon_url, block_id);
//         let resp = reqwest::get(req_url).await?.text().await?;
//         let blob_bundle: BeaconBlobBundle = serde_json::from_str(&resp)?;
//         Ok(blob_bundle.data)
//     }
// }

#[derive(Debug)]
struct Node {
    cfg: Config,
    db: SqlitePool,
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();
    load_dotenv()?;
    let cfg = Config::from_env()?;
    log::debug!(cfg:?; "Loaded config");

    if !Sqlite::database_exists(&cfg.sqlite_path).await? {
        Sqlite::create_database(&cfg.sqlite_path).await?;
    }
    let db = db_connection(&cfg.sqlite_path).await?;

    let node = Node{
        cfg,
        db,
    };



    Ok(())
}
