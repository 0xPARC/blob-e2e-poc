use std::{io, str::FromStr, time::Duration};

use anyhow::{Result, anyhow};
use backoff::ExponentialBackoffBuilder;
use log::LevelFilter;
use sqlx::{
    ConnectOptions, SqlitePool,
    migrate::MigrateDatabase,
    sqlite::{Sqlite, SqliteConnectOptions},
};
use synchronizer::clients::{
    beacon::{self, BeaconClient, types::BlockId},
    common::{ClientError, ClientResult},
};
use tracing::{debug, info};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

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
    pub sync_start_slot: u32,
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
            sync_start_slot: u32::from_str(&var("SYNC_START_SLOT")?)?,
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

#[derive(Debug)]
struct Node {
    cfg: Config,
    cli: BeaconClient,
    db: SqlitePool,
}

impl Node {
    async fn new(cfg: Config) -> Result<Self> {
        if !Sqlite::database_exists(&cfg.sqlite_path).await? {
            Sqlite::create_database(&cfg.sqlite_path).await?;
        }
        let db = db_connection(&cfg.sqlite_path).await?;

        let http_cli = reqwest::Client::builder()
            .timeout(Duration::from_secs(8))
            .build()?;

        let exp_backoff = Some(ExponentialBackoffBuilder::default().build());
        let beacon_cli_cfg = beacon::Config {
            base_url: cfg.beacon_url.clone(),
            exp_backoff,
        };
        let cli = BeaconClient::try_with_client(http_cli, beacon_cli_cfg)?;

        Ok(Self { cfg, db, cli })
    }
}

fn log_init() {
    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(EnvFilter::from_default_env())
        .init();
}

#[tokio::main]
async fn main() -> Result<()> {
    log_init();
    load_dotenv()?;
    let cfg = Config::from_env()?;
    debug!(?cfg, "Loaded config");

    let node = Node::new(cfg).await?;

    let spec = node.cli.get_spec().await?;
    info!(?spec, "Beacon spec");
    let head = node
        .cli
        .get_block_header(BlockId::Head)
        .await?
        .expect("head is not None");
    info!(?head, "Beacon head");

    for slot in node.cfg.sync_start_slot..head.slot {
        let block = match node.cli.get_block(BlockId::Slot(slot)).await? {
            Some(block) => block,
            None => {
                debug!("slot {} has empty block", slot);
                continue;
            }
        };
        debug!(?block);
    }

    Ok(())
}
