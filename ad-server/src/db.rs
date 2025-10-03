pub use common::db_connection;
use pod2::middleware::containers;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, SqlitePool};

#[derive(Debug, FromRow, Serialize, Deserialize)]
pub struct Dict {
    pub id: i64, // maybe use u64 (check db compat)
    #[sqlx(try_from = "Vec<u8>")]
    pub dict_container: DictContainerSql,
    // maybe store also: pod, proof, etc
}

// TODO: Use better serialisation.
#[derive(Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DictContainerSql(pub containers::Dictionary);

impl TryFrom<Vec<u8>> for DictContainerSql {
    type Error = anyhow::Error;

    fn try_from(bytes: Vec<u8>) -> Result<Self, Self::Error> {
        Ok(Self(minicbor_serde::from_slice(&bytes)?))
    }
}

impl DictContainerSql {
    pub fn to_bytes(&self) -> Vec<u8> {
        minicbor_serde::to_vec(self.0.clone()).unwrap()
    }
}

pub async fn init_db(db_pool: &SqlitePool) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS dicts (
            id INTEGER PRIMARY KEY,
            dict_container BLOB NOT NULL
        )
        "#,
    )
    .execute(db_pool)
    .await?;

    Ok(())
}

// DB METHODS:

pub async fn insert_dict(pool: &SqlitePool, dict: &Dict) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO dicts (id, dict_container) VALUES (?, ?);")
        .bind(dict.id)
        .bind(dict.dict_container.to_bytes())
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn get_dict(pool: &SqlitePool, id: i64) -> Result<Dict, sqlx::Error> {
    let (set_bytes,): (Vec<u8>,) = sqlx::query_as("SELECT dict_container FROM dicts WHERE id = ?;")
        .bind(id)
        .fetch_one(pool)
        .await?;
    let set_container = DictContainerSql::try_from(set_bytes).expect("Invalid encoding");

    Ok(Dict {
        id,
        dict_container: set_container,
    })
}

pub async fn update_dict(
    pool: &SqlitePool,
    id: i64,
    new_dict: containers::Dictionary,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE dicts SET dict_container = ? WHERE id = ?")
        .bind(DictContainerSql(new_dict).to_bytes())
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

// user/NAME (get groups that user NAME belongs to): /user/MEMBER. Returns { "red": MERKLE_PF, ... }
