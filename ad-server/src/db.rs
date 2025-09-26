pub use common::db_connection;
use pod2::middleware::{Value, containers};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, SqlitePool};

#[derive(Debug, FromRow, Serialize, Deserialize)]
pub struct Set {
    pub id: i64, // maybe use u64 (check db compat)
    #[sqlx(try_from = "Vec<u8>")]
    pub set_container: SetContainerSql,
    // maybe store also: pod, proof, etc
}

// TODO
#[derive(Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SetContainerSql(pub containers::Set);

impl TryFrom<Vec<u8>> for SetContainerSql {
    type Error = anyhow::Error;

    fn try_from(bytes: Vec<u8>) -> Result<Self, Self::Error> {
        Ok(Self(minicbor_serde::from_slice(&bytes)?))
    }
}

impl SetContainerSql {
    pub fn to_bytes(&self) -> Vec<u8> {
        minicbor_serde::to_vec(self.0.clone()).unwrap()
    }
}

pub async fn init_db(db_pool: &SqlitePool) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS sets (
            id INTEGER PRIMARY KEY,
            set_container BLOB NOT NULL
        )
        "#,
    )
    .execute(db_pool)
    .await?;

    Ok(())
}

// DB METHODS:

pub async fn insert_set(pool: &SqlitePool, set: &Set) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO sets (id, set_container) VALUES (?, ?);")
        .bind(set.id)
        .bind(set.set_container.to_bytes())
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn get_set(pool: &SqlitePool, id: i64) -> Result<Set, sqlx::Error> {
    let (set_bytes,): (Vec<u8>,) = sqlx::query_as("SELECT set_container FROM sets WHERE id = ?;")
        .bind(id)
        .fetch_one(pool)
        .await?;
    let set_container = SetContainerSql::try_from(set_bytes).expect("Invalid encoding");

    Ok(Set { id, set_container })
}

pub async fn update_set(
    pool: &SqlitePool,
    id: i64,
    new_set: containers::Set,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE sets SET set_container = ? WHERE id = ?")
        .bind(SetContainerSql(new_set).to_bytes())
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

// TODO
pub async fn set_insert(pool: &SqlitePool, id: i64, data: Value) -> Result<Set, sqlx::Error> {
    let old_set = get_set(pool, id).await?;
    let mut new_set = old_set.set_container.0.clone();

    // TODO
    new_set
        .insert(&data)
        .expect("Set should be able to acommodate new entry.");

    let new_set = SetContainerSql(new_set);
    sqlx::query_as::<_, Set>("UPDATE sets SET set_container = ? WHERE id = ?")
        .bind(new_set.to_bytes())
        .bind(id)
        .fetch_one(pool)
        .await?;

    Ok(Set {
        id,
        set_container: new_set,
    })
}
