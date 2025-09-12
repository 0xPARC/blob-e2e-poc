#![allow(clippy::uninlined_format_args)]
use std::{str::FromStr, time::Duration};

use alloy::{
    consensus::Transaction, eips as alloy_eips, eips::eip4844::kzg_to_versioned_hash,
    network as alloy_network, primitives::Address, providers as alloy_provider,
};
use alloy_network::Ethereum;
use alloy_provider::{Provider, RootProvider};
use anyhow::{Context, Result, anyhow};
use backoff::ExponentialBackoffBuilder;
use common::load_dotenv;
use plonky2::plonk::{
    circuit_builder::CircuitBuilder, circuit_data::CircuitConfig, config::GenericConfig,
    proof::CompressedProofWithPublicInputs,
};
use pod2::{
    backends::plonky2::{
        mainpod::{
            cache_get_rec_main_pod_common_circuit_data,
            cache_get_rec_main_pod_verifier_circuit_data,
        },
        serialization::{CommonCircuitDataSerializer, VerifierCircuitDataSerializer},
    },
    cache,
    cache::CacheEntry,
    middleware::{C, CommonCircuitData, D, Params, VerifierCircuitData},
};
use sqlx::{SqlitePool, migrate::MigrateDatabase, sqlite::Sqlite};
use synchronizer::{
    bytes_from_simple_blob,
    clients::beacon::{
        self, BeaconClient,
        types::{Blob, BlockHeader, BlockId},
    },
};
use tokio::time::sleep;
use tracing::{debug, info};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

const AD_TEST_ID: [u8; 3] = [1, 2, 3];

/// performs 1 level recursion (plonky2) to get rid of extra custom gates and zk
pub fn shrunk_mainpod_circuit_data(
    params: &Params,
) -> Result<(CommonCircuitData, VerifierCircuitData)> {
    let common_circuit_data = cache_get_rec_main_pod_common_circuit_data(params);
    let verifier_circuit_data = cache_get_rec_main_pod_verifier_circuit_data(params);

    let config = CircuitConfig::standard_recursion_config();
    let mut builder: CircuitBuilder<<C as GenericConfig<D>>::F, D> = CircuitBuilder::new(config);

    // create circuit logic
    let proof_with_pis_target = builder.add_virtual_proof_with_pis(&common_circuit_data);
    let verifier_circuit_target =
        builder.constant_verifier_data(&verifier_circuit_data.verifier_only);
    builder.verify_proof::<C>(
        &proof_with_pis_target,
        &verifier_circuit_target,
        &common_circuit_data,
    );

    builder.register_public_inputs(&proof_with_pis_target.public_inputs);

    let circuit_data = builder.build::<C>();

    let verifier_data = circuit_data.verifier_data();
    Ok((circuit_data.common, verifier_data))
}

pub fn cache_get_shrunk_main_pod_circuit_data(
    params: &Params,
) -> CacheEntry<(CommonCircuitDataSerializer, VerifierCircuitDataSerializer)> {
    cache::get("shrunk_main_pod_circuit_data", &params, |params| {
        let (common, verifier) = shrunk_mainpod_circuit_data(params).expect("build shrunk_mainpod");
        (
            CommonCircuitDataSerializer(common),
            VerifierCircuitDataSerializer(verifier),
        )
    })
    .expect("cache ok")
}

#[derive(Debug)]
pub struct Config {
    // The URL for the Beacon API
    pub beacon_url: String,
    // The URL for the Ethereum RPC API
    pub rpc_url: String,
    // The path to the sqlite database (it will be a file)
    pub sqlite_path: String,
    // The slot where the AD updates begins
    pub ad_genesis_slot: u32,
    // The address that receives AD update via blobs
    pub to_addr: Address,
    // Max Beacon API + RPC requests per second
    pub request_rate: u64,
}

impl Config {
    fn from_env() -> Result<Self> {
        fn var(v: &str) -> Result<String> {
            dotenvy::var(v).with_context(|| v.to_string())
        }
        Ok(Self {
            beacon_url: var("BEACON_URL")?,
            rpc_url: var("RPC_URL")?,
            sqlite_path: var("SQLITE_PATH")?,
            ad_genesis_slot: u32::from_str(&var("AD_GENESIS_SLOT")?)?,
            to_addr: Address::from_str(&var("TO_ADDR")?)?,
            request_rate: u64::from_str(&var("REQUEST_RATE")?)?,
        })
    }
}

#[derive(Debug)]
struct Node {
    cfg: Config,
    #[allow(dead_code)]
    params: Params,
    beacon_cli: BeaconClient,
    rpc_cli: RootProvider,
    db: SqlitePool,
    common_circuit_data: CommonCircuitData,
    verifier_circuit_data: VerifierCircuitData,
}

// SQL tables
pub mod tables {
    #[derive(Debug, PartialEq, Eq, sqlx::FromRow)]
    pub struct AdUpdate {
        pub id: Vec<u8>,
        pub num: i64,
        pub slot: i64,
        pub tx_index: i64,
        pub blob_index: i64,
        pub update_index: i64,
        pub timestamp: i64,
        pub state: Vec<u8>,
        // pub state_prev: Vec<u8>,
    }
    #[derive(Debug, PartialEq, Eq, sqlx::FromRow)]
    pub struct VisitedSlot {
        pub slot: i64,
    }
}

impl Node {
    async fn db_add_ad_update(&self, update: &tables::AdUpdate) -> Result<()> {
        let mut tx = self.db.begin().await?;
        sqlx::query(
                "INSERT INTO ad_update (id, num, slot, tx_index, blob_index, update_index, timestamp, state) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&update.id)
            .bind(update.num)
            .bind(update.slot)
            .bind(update.tx_index)
            .bind(update.blob_index)
            .bind(update.update_index)
            .bind(update.timestamp)
            .bind(&update.state)
        .execute(&mut *tx)
            .await?;
        tx.commit().await?;

        Ok(())
    }

    async fn db_add_visited_slot(&self, slot: i64) -> Result<()> {
        let mut tx = self.db.begin().await?;
        sqlx::query("INSERT INTO visited_slot (slot) VALUES (?)")
            .bind(slot)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;

        Ok(())
    }

    async fn db_get_ad_state(&self, ad_id: &[u8]) -> Result<Vec<u8>> {
        let (state,) = sqlx::query_as("SELECT state FROM ad_update ORDER BY num DESC LIMIT 1")
            .bind(ad_id)
            .fetch_one(&self.db)
            .await?;
        Ok(state)
    }

    async fn db_get_last_visited_slot(&self) -> Result<u32> {
        let (slot,) = sqlx::query_as("SELECT slot FROM visited_slot ORDER BY slot DESC LIMIT 1")
            .fetch_one(&self.db)
            .await?;
        Ok(slot)
    }

    // To dump the formatted table via cli:
    // ```
    // sqlite3 -header -cmd '.mode columns' /tmp/ad-synchronizer.sqlite 'SELECT hex(id), num, slot, tx_index, blob_index, update_index, timestamp, hex(state) FROM ad_update;'
    // ```
    async fn init_db(db: &SqlitePool) -> Result<()> {
        let mut tx = db.begin().await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS ad_update (
                id BLOB NOT NULL,
                num INTEGER NOT NULL,
                slot INTEGER NOT NULL,
                tx_index INTEGER NOT NULL,
                blob_index INTEGER NOT NULL,
                update_index INTEGER NOT NULL,
                timestamp INTEGER NOT NULL,

                state BLOB NOT NULL,

                PRIMARY KEY (id, num)
            );
            "#,
        )
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS visited_slot (
                slot INTEGER NOT NULL
            );
            "#,
        )
        .execute(&mut *tx)
        .await?;

        // sqlx::query(
        //     r#"
        //     CREATE TABLE IF NOT EXISTS blob (
        //         kzg_commitment  TEXT NOT NULL,
        //         execution_block_hash TEXT NOT NULL,
        //         beacon_block_root TEXT NOT NULL,
        //         index  INTEGER NOT NULL,
        //
        //         PRIMARY KEY (kzg_commitment, beacon_block_root)
        //         FOREIGN KEY(execution_block_hash)  REFERENCES execution_block(hash)
        //         FOREIGN KEY(beacon_block_root)  REFERENCES beacon_block(root)
        //     );
        //     "#,
        // )
        // .execute(&mut *tx)
        // .await?;

        // sqlx::query(
        //     r#"
        //     CREATE TABLE IF NOT EXISTS transaction (
        //         hash  TEXT NOT NULL,
        //         execution_block_hash TEXT NOT NULL,
        //         number  INTEGER NOT NULL,
        //         timestamp INTEGER NOT NULL,
        //         from TEXT NOT NULL,
        //         to TEXT,
        //
        //         PRIMARY KEY (hash, execution_block_hash),
        //         FOREIGN KEY(execution_block_hash)  REFERENCES execution_block(hash)
        //     );
        //     "#,
        // )
        // .execute(&mut *tx)
        // .await?;

        // sqlx::query(
        //     r#"
        //     CREATE TABLE IF NOT EXISTS execution_block (
        //         hash  TEXT PRIMARY KEY,
        //         number  INTEGER NOT NULL,
        //         timestamp INTEGER NOT NULL,
        //         from TEXT NOT NULL,
        //         to TEXT
        //     );
        //     "#,
        // )
        // .execute(&mut *tx)
        // .await?;

        // sqlx::query(
        //     r#"
        //     CREATE TABLE IF NOT EXISTS beacon_block (
        //         slot  INTEGER PRIMARY KEY,
        //         root  TEXT,
        //         parent_root TEXT,
        //         timestamp INTEGER,
        //         execution_block_hash TEXT,
        //         FOREIGN KEY(execution_block_hash)  REFERENCES execution_block(hash)
        //     );
        //     "#,
        // )
        // .execute(&mut *tx)
        // .await?;

        tx.commit().await?;

        Ok(())
    }

    async fn new(cfg: Config) -> Result<Self> {
        if !Sqlite::database_exists(&cfg.sqlite_path).await? {
            Sqlite::create_database(&cfg.sqlite_path).await?;
        }
        let db = common::db_connection(&cfg.sqlite_path).await?;
        Self::init_db(&db).await?;

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

        let params = Params::default();
        info!("Loading circuit data...");
        let (common_circuit_data, verifier_circuit_data) =
            &*cache_get_shrunk_main_pod_circuit_data(&params);

        Ok(Self {
            cfg,
            db,
            beacon_cli,
            rpc_cli,
            params,
            common_circuit_data: (**common_circuit_data).clone(),
            verifier_circuit_data: (**verifier_circuit_data).clone(),
        })
    }

    async fn process_beacon_block_header(
        &self,
        beacon_block_header: &BlockHeader,
    ) -> Result<Option<()>> {
        let beacon_block_root = beacon_block_header.root;
        let slot = beacon_block_header.slot;

        let beacon_block = match self
            .beacon_cli
            .get_block(BlockId::Hash(beacon_block_root))
            .await?
        {
            Some(block) => block,
            None => {
                debug!("slot {} has empty block", slot);
                return Ok(None);
            }
        };
        let execution_payload = match beacon_block.execution_payload {
            Some(payload) => payload,
            None => {
                debug!("slot {} has no execution payload", slot);
                return Ok(None);
            }
        };
        debug!(
            "slot {} has execution block {} at height {}",
            slot, execution_payload.block_hash, execution_payload.block_number
        );

        let has_kzg_blob_commitments = match beacon_block.blob_kzg_commitments {
            Some(commitments) => !commitments.is_empty(),
            None => false,
        };
        if !has_kzg_blob_commitments {
            debug!("slot {} has no blobs", slot);
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

        let indexed_blob_txs: Vec<_> = match execution_block.transactions.as_transactions() {
            Some(txs) => txs
                .iter()
                .enumerate()
                .filter(|(_index, tx)| tx.inner.blob_versioned_hashes().is_some())
                .collect(),
            None => {
                return Err(anyhow!(
                    "Consensus block {beacon_block_root} has blobs but the execution block doesn't have txs"
                ));
            }
        };

        if indexed_blob_txs.is_empty() {
            return Err(anyhow!(
                "Block mismatch: Consensus block \"{beacon_block_root}\" contains blob KZG commitments, but the corresponding execution block \"{execution_block_hash:#?}\" does not contain any blob transactions"
            ));
        }

        let blobs = self.beacon_cli.get_blobs(slot.into()).await?;
        debug!("found {} blobs", blobs.len());

        let blobs_vh: Vec<_> = blobs
            .iter()
            .map(|blob| kzg_to_versioned_hash(blob.kzg_commitment.as_ref()))
            .collect();

        for (tx_index, tx) in indexed_blob_txs {
            let tx = tx.as_recovered();
            let hash = tx.hash();
            let from = tx.signer();
            let to = match tx.to() {
                Some(to) => to,
                None => continue, // blob in a CREATE tx
            };
            let tx_blobs_vh = tx.blob_versioned_hashes().expect("tx has blobs");
            let tx_blob_indices: Vec<_> = tx_blobs_vh
                .iter()
                .map(|tx_blob_vh| {
                    blobs_vh
                        .iter()
                        .position(|blob_vh| blob_vh == tx_blob_vh)
                        .expect("blob in beacon block")
                })
                .collect();
            debug!(?hash, ?from, ?to, ?tx_blob_indices);

            if self.cfg.to_addr == to {
                for blob_index in tx_blob_indices.iter() {
                    let blob = &blobs[*blob_index];
                    info!("Found AD blob");
                    match self.pod_from_blob(blob) {
                        Ok(_) => {
                            info!("MainPod verified!");
                            let update = tables::AdUpdate {
                                id: AD_TEST_ID.to_vec(),
                                num: 0,
                                slot: slot as i64,
                                tx_index: tx_index as i64,
                                blob_index: *blob_index as i64,
                                update_index: 0,
                                timestamp: execution_block.header.timestamp as i64,
                                state: vec![4, 5, 6, 7, 8],
                            };
                            self.db_add_ad_update(&update).await?;
                            // Just for testing
                            let ad_id = &AD_TEST_ID;
                            let state = self.db_get_ad_state(ad_id).await?;
                            info!("State of id={:?} is {:?}", ad_id, state);
                        }
                        Err(e) => {
                            debug!("Invalid pod in blob: {:?}", e);
                            continue;
                        }
                    };
                }
            }
        }
        Ok(Some(()))
    }

    fn pod_from_blob(&self, blob: &Blob) -> Result<()> {
        let bytes =
            bytes_from_simple_blob(blob.blob.inner()).context("Invalid byte encoding in blob")?;
        let proof = CompressedProofWithPublicInputs::<_, C, D>::from_bytes(
            bytes,
            &self.common_circuit_data,
        )
        .context("CompressedProofWithPublicInputs::from_bytes")?
        .decompress(
            &self.verifier_circuit_data.verifier_only.circuit_digest,
            &self.common_circuit_data,
        )
        .context("CompressedProofWithPublicInputs::decompress")?;
        self.verifier_circuit_data.verify(proof)
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

    let initial_slot = node
        .db_get_last_visited_slot()
        .await
        .map(|x| x + 1)
        .unwrap_or(node.cfg.ad_genesis_slot)
        .max(node.cfg.ad_genesis_slot);

    for slot in initial_slot..head.slot {
        info!("checking slot {}", slot);
        println!("checking slot {}", slot);
        let beacon_block_header = match node
            .beacon_cli
            .get_block_header(BlockId::Slot(slot))
            .await?
        {
            Some(block) => block,
            None => {
                debug!("slot {} has empty block", slot);
                continue;
            }
        };

        node.process_beacon_block_header(&beacon_block_header)
            .await?;

        node.db_add_visited_slot(slot as i64).await?;

        if node.cfg.request_rate != 0 {
            let requests = 5;
            let delay_ms = 1000 * requests / node.cfg.request_rate;
            sleep(Duration::from_millis(delay_ms)).await;
        }
    }

    Ok(())
}
