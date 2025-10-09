pub mod disk;
pub mod payload;

/// 2 options to prepare the POD proofs:
///   A) "groth":
///     first compute the one extra recursive proof from the given MainPod's proof in
///     order to shrink it, together with using the bn254's poseidon variant in the
///     configuration of the plonky2 prover, in order to make it compatible with the
///     Groth16 circuit.
///     Then compute a Groth16 proof which verifies the last plonky2 proof
pub mod groth;
///   B) "shrink":
///     first shrinks the given MainPod's proof, and then compresses it,
///     returning the compressed proof (without public inputs)
pub mod shrink;

use std::{io, str::FromStr, time::Duration};

use anyhow::{Result, anyhow};
use log::LevelFilter;
use pod2::middleware::{Value, containers};
use sqlx::{ConnectOptions, SqlitePool, sqlite::SqliteConnectOptions};

/// struct used to convert sqlx errors to warp errors
#[allow(dead_code)]
#[derive(Debug)]
pub struct CustomError(pub String);
impl warp::reject::Reject for CustomError {}

pub fn load_dotenv() -> Result<()> {
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

pub async fn db_connection(url: &str) -> Result<SqlitePool, sqlx::Error> {
    let opts = SqliteConnectOptions::from_str(url)?
        // https://docs.rs/sqlx/latest/sqlx/sqlite/struct.SqliteConnectOptions.html#method.serialized
        // > Setting this to true may help if you are getting access violation errors or
        // segmentation faults, but will also incur a significant performance penalty. You should
        // leave this set to false if at all possible.
        .serialized(false)
        .busy_timeout(Duration::from_secs(3600))
        .log_statements(LevelFilter::Debug)
        .log_slow_statements(LevelFilter::Warn, Duration::from_millis(800));
    SqlitePool::connect_with(opts).await
}

#[derive(Debug, Clone, PartialEq)]
pub enum ProofType {
    Plonky2,
    Groth16,
}
impl std::str::FromStr for ProofType {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "plonky2" => Ok(ProofType::Plonky2),
            "groth16" => Ok(ProofType::Groth16),
            _ => Err(anyhow!("unsupported PROOF_TYPE {}", s)),
        }
    }
}

impl ProofType {
    pub fn from_byte(input: &u8) -> Result<ProofType> {
        match input {
            0u8 => Ok(ProofType::Plonky2),
            1u8 => Ok(ProofType::Groth16),
            _ => Err(anyhow!("unsupported PROOF_TYPE {}", input)),
        }
    }
    pub fn to_byte(self) -> u8 {
        match self {
            ProofType::Plonky2 => 0u8,
            ProofType::Groth16 => 1u8,
        }
    }
}

pub fn set_from_value(v: &Value) -> Result<containers::Set> {
    match v.typed() {
        pod2::middleware::TypedValue::Set(s) => Ok(s.clone()),
        _ => Err(anyhow!("Invalid set")),
    }
}
