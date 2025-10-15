#![allow(clippy::uninlined_format_args)]
use std::{collections::HashMap, str::FromStr, sync::Arc};

use alloy::primitives::Address;
use anyhow::{Context as _, Result};
use app::build_predicates;
use common::{
    ProofType,
    shrink::{ShrunkMainPodBuild, ShrunkMainPodSetup},
};
use pod2::{
    backends::plonky2::basetypes::DEFAULT_VD_SET,
    middleware::{CustomPredicateBatch, CustomPredicateRef, Params, VDSet},
};
use sqlx::{
    migrate::MigrateDatabase,
    sqlite::{Sqlite, SqlitePool},
};
use tokio::{
    sync::{
        RwLock,
        mpsc::{self, Sender},
    },
    task,
};
use tracing::{info, warn};
use uuid::Uuid;

pub mod db;
pub mod endpoints;
pub mod eth;
pub mod queue;

#[derive(Debug, Clone)]
pub struct Config {
    // The URL for the Ethereum RPC API
    pub rpc_url: String,
    // The path to the sqlite database (it will be a file)
    pub sqlite_path: String,
    // The path to store pods
    pub pods_path: String,
    // Ethereum private key to send txs
    pub priv_key: String,
    // The address that receives AD update via blobs
    pub to_addr: Address,
    pub tx_watch_timeout: u64,
    // set the proving system used to generate the proofs being sent to ethereum
    //   options: plonky2 / groth16
    pub proof_type: ProofType,
}

impl Config {
    fn from_env() -> Result<Self> {
        fn var(v: &str) -> Result<String> {
            dotenvy::var(v).with_context(|| v.to_string())
        }
        Ok(Self {
            rpc_url: var("RPC_URL")?,
            sqlite_path: var("AD_SERVER_SQLITE_PATH")?,
            pods_path: var("PODS_PATH")?,
            priv_key: var("PRIV_KEY")?,
            to_addr: Address::from_str(&var("TO_ADDR")?)?,
            tx_watch_timeout: u64::from_str(&var("TX_WATCH_TIMEOUT")?)?,
            proof_type: ProofType::from_str(&var("PROOF_TYPE")?)?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct PodConfig {
    params: Params,
    vd_set: VDSet,
    batches: Vec<Arc<CustomPredicateBatch>>,
    pred_update: CustomPredicateRef,
}

pub struct Context {
    pub cfg: Config,
    pub db_pool: SqlitePool,
    pub pod_config: PodConfig,
    pub shrunk_main_pod_build: ShrunkMainPodBuild,
    pub queue_tx: Sender<queue::Request>,
    pub queue_state: RwLock<HashMap<Uuid, queue::State>>,
}

impl Context {
    pub fn new(
        cfg: Config,
        db_pool: SqlitePool,
        pod_config: PodConfig,
        shrunk_main_pod_build: ShrunkMainPodBuild,
        queue_tx: Sender<queue::Request>,
    ) -> Self {
        Self {
            cfg,
            db_pool,
            pod_config,
            shrunk_main_pod_build,
            queue_tx,
            queue_state: RwLock::new(HashMap::new()),
        }
    }
}

use tracing_subscriber::{EnvFilter, fmt, prelude::*};
fn log_init() {
    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(EnvFilter::from_default_env())
        .init();
}

#[tokio::main]
async fn main() -> Result<()> {
    // If a thread panics we have a bug, so we exit the entire process instead of staying in a
    // crashed state.
    let default_panic = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default_panic(info);
        std::process::exit(1);
    }));

    log_init();
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
    info!("Prebuilding circuits to calculate vd_set...");
    let vd_set = &*DEFAULT_VD_SET;
    info!("vd_set calculation complete");
    let batches = build_predicates(&params);
    let pred_update = batches[0]
        .predicate_ref_by_name("update")
        .expect("update defined");
    let shrunk_main_pod_build = ShrunkMainPodSetup::new(&params).build()?;
    let pod_config = PodConfig {
        params,
        vd_set: vd_set.clone(),
        batches,
        pred_update,
    };

    if cfg.proof_type == ProofType::Groth16 {
        // initialize groth16 memory
        warn!(
            "WARNING: loading Groth16 artifacts, please wait till the pk & vk are loaded (>30s) and the server is running"
        );
        common::groth::init()?;
    }

    let (queue_tx, queue_rx) = mpsc::channel::<queue::Request>(8);
    let ctx = Arc::new(Context::new(
        cfg,
        db_pool,
        pod_config,
        shrunk_main_pod_build,
        queue_tx,
    ));

    let routes = endpoints::routes(ctx.clone());
    task::spawn(async move {
        queue::handle_loop(ctx, queue_rx).await;
    });

    info!("server at http://0.0.0.0:8000");
    warp::serve(routes).run(([0, 0, 0, 0], 8000)).await;

    Ok(())
}
