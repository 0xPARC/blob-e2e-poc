#![allow(clippy::uninlined_format_args)]
use std::{str::FromStr, sync::Arc, time::Duration};

use alloy::{
    consensus::Transaction, eips as alloy_eips, eips::eip4844::kzg_to_versioned_hash,
    network as alloy_network, primitives::Address, providers as alloy_provider,
};
use alloy_network::Ethereum;
use alloy_provider::{Provider, RootProvider};
use anyhow::{Context, Result, anyhow};
use backoff::ExponentialBackoffBuilder;
use chrono::{DateTime, Utc};
use common::{
    circuits::ShrunkMainPodSetup,
    load_dotenv,
    payload::{Payload, PayloadInit, PayloadUpdate},
};
use hex::ToHex;
use plonky2::plonk::proof::CompressedProofWithPublicInputs;
use pod2::{
    backends::plonky2::{
        mainpod::calculate_statements_hash,
        serialization::{CommonCircuitDataSerializer, VerifierCircuitDataSerializer},
    },
    cache,
    cache::CacheEntry,
    middleware::{
        CommonCircuitData, EMPTY_VALUE, Hash, Params, RawValue, Statement, Value,
        VerifierCircuitData,
    },
};
use sqlx::{SqlitePool, migrate::MigrateDatabase, sqlite::Sqlite};
use synchronizer::{
    bytes_from_simple_blob,
    clients::beacon::{
        self, BeaconClient,
        types::{Blob, BlockHeader, BlockId},
    },
};
use tokio::{runtime::Runtime, time::sleep};
use tracing::{debug, info, warn};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

pub mod endpoints;

type B256 = [u8; 32];

pub fn cache_get_shrunk_main_pod_circuit_data(
    params: &Params,
) -> CacheEntry<(CommonCircuitDataSerializer, VerifierCircuitDataSerializer)> {
    cache::get("shrunk_main_pod_circuit_data", &params, |params| {
        let shrunk_main_pod_build = ShrunkMainPodSetup::new(params)
            .build()
            .expect("successful build");
        let verifier = shrunk_main_pod_build.circuit_data.verifier_data();
        let common = shrunk_main_pod_build.circuit_data.common;
        (
            CommonCircuitDataSerializer(common),
            VerifierCircuitDataSerializer(verifier),
        )
    })
    .expect("cache ok")
}

#[derive(Clone, Debug)]
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
            sqlite_path: var("SYNCHRONIZER_SQLITE_PATH")?,
            ad_genesis_slot: u32::from_str(&var("AD_GENESIS_SLOT")?)?,
            to_addr: Address::from_str(&var("TO_ADDR")?)?,
            request_rate: u64::from_str(&var("REQUEST_RATE")?)?,
        })
    }
}

#[derive(Clone, Debug)]
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
    use anyhow::Error;
    use common::payload::{
        read_custom_predicate_ref, read_elems, write_custom_predicate_ref, write_elems,
    };
    use pod2::middleware::{CustomPredicateRef, Hash, RawValue};

    use super::B256;

    #[derive(Debug, Eq, PartialEq)]
    pub struct HashSql(pub Hash);

    impl TryFrom<Vec<u8>> for HashSql {
        type Error = Error;

        fn try_from(bytes: Vec<u8>) -> Result<Self, Self::Error> {
            Ok(Self(Hash(read_elems(&mut bytes.as_slice())?)))
        }
    }

    impl HashSql {
        pub fn to_bytes(&self) -> Vec<u8> {
            let mut buffer = Vec::with_capacity(32);
            write_elems(&mut buffer, &self.0.0);
            buffer
        }
    }

    #[derive(Debug, Eq, PartialEq)]
    pub struct RawValueSql(pub RawValue);

    impl TryFrom<Vec<u8>> for RawValueSql {
        type Error = Error;

        fn try_from(bytes: Vec<u8>) -> Result<Self, Self::Error> {
            Ok(Self(RawValue(read_elems(&mut bytes.as_slice())?)))
        }
    }

    impl RawValueSql {
        pub fn to_bytes(&self) -> Vec<u8> {
            let mut buffer = Vec::with_capacity(32);
            write_elems(&mut buffer, &self.0.0);
            buffer
        }
    }

    #[derive(Debug, Eq, PartialEq)]
    pub struct CustomPredicateRefSql(pub CustomPredicateRef);

    impl TryFrom<Vec<u8>> for CustomPredicateRefSql {
        type Error = Error;

        fn try_from(bytes: Vec<u8>) -> Result<Self, Self::Error> {
            Ok(Self(read_custom_predicate_ref(&mut bytes.as_slice())?))
        }
    }

    impl CustomPredicateRefSql {
        pub fn to_bytes(&self) -> Vec<u8> {
            let mut buffer = Vec::with_capacity(32);
            write_custom_predicate_ref(&mut buffer, &self.0);
            buffer
        }
    }

    #[derive(Debug, PartialEq, Eq, sqlx::FromRow)]
    pub struct Ad {
        #[sqlx(try_from = "Vec<u8>")]
        pub id: HashSql,
        #[sqlx(try_from = "Vec<u8>")]
        pub custom_predicate_ref: CustomPredicateRefSql,
        #[sqlx(try_from = "Vec<u8>")]
        pub vds_root: HashSql,
        #[sqlx(try_from = "Vec<u8>")]
        pub blob_versioned_hash: B256,
    }

    #[derive(Debug, PartialEq, Eq, sqlx::FromRow)]
    pub struct AdUpdate {
        #[sqlx(try_from = "Vec<u8>")]
        pub id: HashSql,
        pub num: i64,
        #[sqlx(try_from = "Vec<u8>")]
        pub state: RawValueSql,
        #[sqlx(try_from = "Vec<u8>")]
        pub blob_versioned_hash: B256,
    }

    #[derive(Debug, PartialEq, Eq, sqlx::FromRow)]
    pub struct Blob {
        #[sqlx(try_from = "Vec<u8>")]
        pub versioned_hash: B256,
        pub slot: i64,
        pub block: i64,
        pub blob_index: i64,
        pub timestamp: i64,
    }

    #[derive(Debug, PartialEq, Eq, sqlx::FromRow)]
    pub struct VisitedSlot {
        pub slot: i64,
    }
}

use tables::{CustomPredicateRefSql, HashSql, RawValueSql};

struct Database<E>(E);

/// Implementation of database queries that works with transactions and database:
/// ```
/// Database(&mut *tx)
/// Database(&db)
/// ```
impl<'a, E> Database<E>
where
    E: sqlx::Executor<'a, Database = sqlx::Sqlite>,
{
    async fn add_blob(self, blob: &tables::Blob) -> Result<()> {
        sqlx::query(
            "INSERT INTO blob (versioned_hash, slot, block, blob_index, timestamp) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(blob.versioned_hash.as_slice())
        .bind(blob.slot)
        .bind(blob.block)
        .bind(blob.blob_index)
        .bind(blob.timestamp)
        .execute(self.0)
        .await?;

        Ok(())
    }

    async fn add_ad(self, ad: &tables::Ad) -> Result<()> {
        sqlx::query(
            "INSERT INTO ad (id, custom_predicate_ref, vds_root, blob_versioned_hash) VALUES (?, ?, ?, ?)",
        )
        .bind(ad.id.to_bytes())
        .bind(ad.custom_predicate_ref.to_bytes())
        .bind(ad.vds_root.to_bytes())
        .bind(ad.blob_versioned_hash.as_slice())
        .execute(self.0)
        .await?;

        Ok(())
    }

    async fn add_ad_update(self, update: &tables::AdUpdate) -> Result<()> {
        sqlx::query(
            "INSERT INTO ad_update (id, num, state, blob_versioned_hash) VALUES (?, ?, ?, ?)",
        )
        .bind(update.id.to_bytes())
        .bind(update.num)
        .bind(update.state.to_bytes())
        .bind(update.blob_versioned_hash.as_slice())
        .execute(self.0)
        .await?;

        Ok(())
    }

    async fn add_visited_slot(self, slot: i64) -> Result<()> {
        sqlx::query("INSERT INTO visited_slot (slot) VALUES (?)")
            .bind(slot)
            .execute(self.0)
            .await?;

        Ok(())
    }

    async fn get_ad(self, ad_id: Hash) -> Result<tables::Ad> {
        Ok(sqlx::query_as("SELECT * FROM ad WHERE id = ?")
            .bind(HashSql(ad_id).to_bytes())
            .fetch_one(self.0)
            .await?)
    }

    async fn get_ad_update_last(self, ad_id: Hash) -> Result<tables::AdUpdate> {
        Ok(
            sqlx::query_as("SELECT * FROM ad_update WHERE id = ? ORDER BY num DESC LIMIT 1")
                .bind(HashSql(ad_id).to_bytes())
                .fetch_one(self.0)
                .await?,
        )
    }

    async fn get_ad_update_last_state(self, ad_id: Hash) -> Result<RawValue> {
        let (state,): (Vec<u8>,) =
            sqlx::query_as("SELECT state FROM ad_update WHERE id = ? ORDER BY num DESC LIMIT 1")
                .bind(HashSql(ad_id).to_bytes())
                .fetch_one(self.0)
                .await?;
        Ok(RawValueSql::try_from(state).expect("32 bytes").0)
    }

    async fn get_visited_slot_last(self) -> Result<u32> {
        let (slot,) = sqlx::query_as("SELECT slot FROM visited_slot ORDER BY slot DESC LIMIT 1")
            .fetch_one(self.0)
            .await?;
        Ok(slot)
    }
}

impl Node {
    // To dump the formatted table via cli:
    // ```
    // sqlite3 -header -cmd '.mode columns' /tmp/ad-synchronizer.sqlite 'SELECT hex(id), num, slot, tx_index, blob_index, update_index, timestamp, hex(state) FROM ad_update;'
    // ```
    async fn init_db(db: &SqlitePool) -> Result<()> {
        let mut tx = db.begin().await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS blob (
                versioned_hash BLOB PRIMARY KEY,
                slot INTEGER NOT NULL,
                block INTEGER NOT NULL,
                blob_index INTEGER NOT NULL,
                timestamp INTEGER NOT NULL
            );
            "#,
        )
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS ad_update (
                id BLOB NOT NULL,
                num INTEGER NOT NULL,
                state BLOB NOT NULL,
                blob_versioned_hash BLOB NOT NULL,

                PRIMARY KEY (id, num)
            );
            "#,
        )
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS ad (
                id BLOB PRIMARY KEY,
                custom_predicate_ref BLOB NOT NULL,
                vds_root BLOB NOT NULL,
                blob_versioned_hash BLOB NOT NULL
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
        db_tx: &mut sqlx::SqliteTransaction<'_>,
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

        info!(
            "processing slot {} from {}",
            slot,
            DateTime::<Utc>::from_timestamp_secs(execution_payload.timestamp as i64)
                .unwrap_or_default(),
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

        for (_tx_index, tx) in indexed_blob_txs {
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
                    match self.process_ad_blob(db_tx, blob).await {
                        Ok(_) => {
                            info!(
                                "Valid ad_blob at slot {}, blob_index {}!",
                                slot, *blob_index
                            );
                        }
                        Err(e) => {
                            info!("Invalid ad_blob: {:?}", e);
                            continue;
                        }
                    };

                    Database(&mut **db_tx)
                        .add_blob(&tables::Blob {
                            versioned_hash: kzg_to_versioned_hash(blob.kzg_commitment.as_ref()).0,
                            slot: slot as i64,
                            block: execution_block.header.number as i64,
                            blob_index: *blob_index as i64,
                            timestamp: execution_block.header.timestamp as i64,
                        })
                        .await?;
                }
            }
        }
        Ok(Some(()))
    }

    async fn process_ad_blob(
        &self,
        db_tx: &mut sqlx::SqliteTransaction<'_>,
        blob: &Blob,
    ) -> Result<()> {
        let bytes =
            bytes_from_simple_blob(blob.blob.inner()).context("Invalid byte encoding in blob")?;
        let payload = Payload::from_bytes(&bytes, &self.common_circuit_data)?;

        match payload {
            Payload::Init(payload) => self.process_payload_init(db_tx, blob, payload).await,
            Payload::Update(payload) => self.process_payload_update(db_tx, blob, payload).await,
        }
    }

    async fn process_payload_init(
        &self,
        db_tx: &mut sqlx::SqliteTransaction<'_>,
        blob: &Blob,
        payload: PayloadInit,
    ) -> Result<()> {
        match Database(&mut **db_tx).get_ad(payload.id).await {
            Err(err) => match err.root_cause().downcast_ref::<sqlx::Error>() {
                Some(&sqlx::Error::RowNotFound) => {}
                _ => return Err(err),
            },
            Ok(ad) => {
                return Err(anyhow!(
                    "got init payload {:?} but AD already exists {:?}",
                    payload,
                    ad
                ));
            }
        };

        let blob_versioned_hash = kzg_to_versioned_hash(blob.kzg_commitment.as_ref()).0;
        let ad = tables::Ad {
            id: HashSql(payload.id),
            custom_predicate_ref: CustomPredicateRefSql(payload.custom_predicate_ref),
            vds_root: HashSql(payload.vds_root),
            blob_versioned_hash,
        };
        Database(&mut **db_tx).add_ad(&ad).await?;
        let ad_update = tables::AdUpdate {
            id: HashSql(payload.id),
            num: 0,
            state: RawValueSql(EMPTY_VALUE),
            blob_versioned_hash,
        };
        Database(&mut **db_tx).add_ad_update(&ad_update).await?;
        tracing::info!(payload = "Init", ad_id = payload.id.encode_hex::<String>());
        Ok(())
    }

    async fn process_payload_update(
        &self,
        db_tx: &mut sqlx::SqliteTransaction<'_>,
        blob: &Blob,
        payload: PayloadUpdate,
    ) -> Result<()> {
        let ad = Database(&mut **db_tx).get_ad(payload.id).await?;
        let ad_update_last = Database(&mut **db_tx)
            .get_ad_update_last(payload.id)
            .await?;

        let st = Statement::Custom(
            ad.custom_predicate_ref.0,
            vec![
                Value::from(payload.new_state),
                Value::from(ad_update_last.state.0),
            ],
        );
        let sts_hash = calculate_statements_hash(&[st.into()], &self.params);
        let public_inputs = [sts_hash.0, ad.vds_root.0.0].concat();
        let proof_with_pis = CompressedProofWithPublicInputs {
            proof: payload.shrunk_main_pod_proof,
            public_inputs,
        };
        let proof = proof_with_pis
            .decompress(
                &self.verifier_circuit_data.verifier_only.circuit_digest,
                &self.common_circuit_data,
            )
            .context("CompressedProofWithPublicInputs::decompress")?;
        self.verifier_circuit_data.verify(proof)?;

        let blob_versioned_hash = kzg_to_versioned_hash(blob.kzg_commitment.as_ref()).0;
        let ad_update = tables::AdUpdate {
            id: HashSql(payload.id),
            num: ad_update_last.num + 1,
            state: RawValueSql(payload.new_state),
            blob_versioned_hash,
        };
        Database(&mut **db_tx).add_ad_update(&ad_update).await?;
        tracing::info!(
            payload = "Update",
            ad_id = payload.id.encode_hex::<String>(),
            num = ad_update.num,
            old_state = ad_update_last.state.0.encode_hex::<String>(),
            new_state = payload.new_state.encode_hex::<String>()
        );
        Ok(())
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

    {
        let node = node.clone();
        std::thread::spawn(move || -> Result<_, std::io::Error> {
            Runtime::new().map(|rt| {
                rt.block_on(async {
                    let routes = endpoints::routes(Arc::new(node));
                    warp::serve(routes).run(([0, 0, 0, 0], 8001)).await
                })
            })
        });
    }
    info!("Started HTTP server");

    let initial_slot = Database(&node.db)
        .get_visited_slot_last()
        .await
        .map(|x| x + 1)
        .unwrap_or(node.cfg.ad_genesis_slot)
        .max(node.cfg.ad_genesis_slot);

    let mut slot = initial_slot;
    loop {
        debug!("checking slot {}", slot);
        let some_beacon_block_header = if slot <= head.slot {
            node.beacon_cli
                .get_block_header(BlockId::Slot(slot))
                .await?
        } else {
            // TODO: Be more fancy and replace this with a stream from an event subscription to
            // Beacon Headers
            tokio::time::sleep(Duration::from_secs(5)).await;
            loop {
                let head = node
                    .beacon_cli
                    .get_block_header(BlockId::Head)
                    .await?
                    .expect("head is not None");
                if head.slot > slot {
                    debug!(
                        "head is {}, slot {} was skipped, retreiving...",
                        head.slot, slot
                    );
                    break node
                        .beacon_cli
                        .get_block_header(BlockId::Slot(slot))
                        .await?;
                } else if head.slot == slot {
                    break Some(head);
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        };
        let beacon_block_header = match some_beacon_block_header {
            Some(block) => block,
            None => {
                debug!("slot {} has empty block", slot);
                Database(&node.db).add_visited_slot(slot as i64).await?;
                slot += 1;
                continue;
            }
        };

        let mut tx = node.db.begin().await?;
        node.process_beacon_block_header(&mut tx, &beacon_block_header)
            .await?;
        Database(&mut *tx).add_visited_slot(slot as i64).await?;
        tx.commit().await?;

        if node.cfg.request_rate != 0 {
            let requests = 5;
            let delay_ms = 1000 * requests / node.cfg.request_rate;
            sleep(Duration::from_millis(delay_ms)).await;
        }

        slot += 1;
    }
}
