use std::{io, str::FromStr, time::Duration};

use alloy::{
    consensus as alloy_consensus, eips as alloy_eips, network as alloy_network,
    providers as alloy_provider,
};
use alloy_consensus::transaction::Transaction;
use alloy_network::Ethereum;
use alloy_provider::{Provider, ProviderBuilder, RootProvider};
use anyhow::{Context, Result, anyhow};
use backoff::ExponentialBackoffBuilder;
use log::LevelFilter;
use sqlx::{
    ConnectOptions, SqlitePool,
    migrate::MigrateDatabase,
    sqlite::{Sqlite, SqliteConnectOptions},
};
use synchronizer::clients::{
    beacon::{
        self, BeaconClient,
        types::{BlockHeader, BlockId},
    },
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
    pub rpc_url: String,
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
            rpc_url: var("RPC_URL")?,
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
    beacon_cli: BeaconClient,
    rpc_cli: RootProvider,
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
        let beacon_cli = BeaconClient::try_with_client(http_cli, beacon_cli_cfg)?;
        let rpc_cli = RootProvider::<Ethereum>::new_http(cfg.rpc_url.parse()?);

        Ok(Self {
            cfg,
            db,
            beacon_cli,
            rpc_cli,
        })
    }

    async fn process_beacon_block_header(
        &self,
        beacon_block_header: &BlockHeader,
    ) -> Result<Option<()>> {
        let beacon_block_root = beacon_block_header.root;
        let slot = beacon_block_header.slot;

        let beacon_block = match self.beacon_cli.get_block(BlockId::Slot(slot)).await? {
            Some(block) => block,
            None => {
                info!("slot {} has empty block", slot);
                return Ok(None);
            }
        };
        let execution_payload = match beacon_block.execution_payload {
            Some(payload) => payload,
            None => {
                info!("slot {} has no execution payload", slot);
                return Ok(None);
            }
        };
        info!(
            "slot {} has execution block {} at height {}",
            slot, execution_payload.block_hash, execution_payload.block_number
        );

        let has_kzg_blob_commitments = match beacon_block.blob_kzg_commitments {
            Some(commitments) => !commitments.is_empty(),
            None => false,
        };
        if has_kzg_blob_commitments {
            info!("slot {} has no blobs", slot);
            return Ok(None);
        }

        let execution_block_hash = execution_payload.block_hash;

        let execution_block_id = alloy_eips::eip1898::BlockId::Hash(execution_block_hash.into());
        let execution_block = self
            .rpc_cli
            .get_block(execution_block_id)
            .full()
            .await?
            .with_context(|| format!("Execution block {execution_block_hash} not found"))?;

        let blob_txs: Vec<_> = match execution_block.transactions.as_transactions() {
            Some(txs) => txs
                .into_iter()
                .filter(|tx| tx.inner.blob_versioned_hashes().is_some())
                .collect(),
            None => {
                return Err(anyhow!(
                    "Consensus block {beacon_block_root} has blobs but the execution block doesn't have txs"
                ));
            }
        };

        if blob_txs.is_empty() {
            return Err(anyhow!("Block mismatch: Consensus block \"{beacon_block_root}\" contains blob KZG commitments, but the corresponding execution block \"{execution_block_hash:#?}\" does not contain any blob transactions").into());
        }

        let blobs = self.beacon_cli.get_blobs(slot.into()).await?;
        info!("found {} blobs", blobs.len());
        Ok(Some(()))
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
    info!(?cfg, "Loaded config");

    let node = Node::new(cfg).await?;

    let spec = node.beacon_cli.get_spec().await?;
    info!(?spec, "Beacon spec");
    let head = node
        .beacon_cli
        .get_block_header(BlockId::Head)
        .await?
        .expect("head is not None");
    info!(?head, "Beacon head");

    for slot in node.cfg.sync_start_slot..head.slot {
        let beacon_block_header = match node
            .beacon_cli
            .get_block_header(BlockId::Slot(slot))
            .await?
        {
            Some(block) => block,
            None => {
                info!("slot {} has empty block", slot);
                continue;
            }
        };

        node.process_beacon_block_header(&beacon_block_header)
            .await?;
    }

    Ok(())
}
