pub use common::db_connection;
use pod2::middleware::containers;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, SqlitePool};

#[derive(Debug, FromRow, Serialize, Deserialize)]
pub struct AdState {
    pub id: i64,  // maybe use u64 (check db compat)
    pub num: i64, // maybe use u64 (check db compat)
    #[sqlx(try_from = "Vec<u8>")]
    pub state: DictContainerSql,
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
        CREATE TABLE IF NOT EXISTS membership_list (
            id INTEGER PRIMARY KEY,
            num INTEGER NOT NULL,
            state BLOB NOT NULL
        )
        "#,
    )
    .execute(db_pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS rev_membership_list (
            id INTEGER PRIMARY KEY,
            num INTEGER NOT NULL,
            state BLOB NOT NULL
        )
        "#,
    )
    .execute(db_pool)
    .await?;

    Ok(())
}

// DB METHODS:

pub async fn get_latest_membership_list(pool: &SqlitePool) -> Result<Option<AdState>, sqlx::Error> {
    sqlx::query_as::<_, AdState>(
        "SELECT id, num, state FROM membership_list ORDER BY id DESC LIMIT 1",
    )
    .fetch_optional(pool)
    .await
}

pub async fn insert_membership_list(
    pool: &SqlitePool,
    membership_list: &AdState,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO membership_list (id, num, state) VALUES (?, ?, ?);")
        .bind(membership_list.id)
        .bind(membership_list.num)
        .bind(membership_list.state.to_bytes())
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn insert_rev_membership_list(
    pool: &SqlitePool,
    rev_membership_list: &AdState,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO rev_membership_list (id, num, state) VALUES (?, ?, ?);")
        .bind(rev_membership_list.id)
        .bind(rev_membership_list.num)
        .bind(rev_membership_list.state.to_bytes())
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn get_membership_list(pool: &SqlitePool, id: i64) -> Result<AdState, sqlx::Error> {
    let (state_bytes, num): (Vec<u8>, i64) =
        sqlx::query_as("SELECT state, num FROM membership_list WHERE id = ?;")
            .bind(id)
            .fetch_one(pool)
            .await?;
    let state = DictContainerSql::try_from(state_bytes).expect("Invalid encoding");

    Ok(AdState { id, num, state })
}

pub async fn get_rev_membership_list(pool: &SqlitePool, id: i64) -> Result<AdState, sqlx::Error> {
    let (state_bytes, num): (Vec<u8>, i64) =
        sqlx::query_as("SELECT state, num FROM rev_membership_list WHERE id = ?;")
            .bind(id)
            .fetch_one(pool)
            .await?;
    let state = DictContainerSql::try_from(state_bytes).expect("Invalid encoding");

    Ok(AdState { id, num, state })
}

pub async fn update_membership_list(
    pool: &SqlitePool,
    id: i64,
    num: i64,
    state: containers::Dictionary,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE membership_list SET state = ?, num = ? WHERE id = ?")
        .bind(DictContainerSql(state).to_bytes())
        .bind(num)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn update_rev_membership_list(
    pool: &SqlitePool,
    id: i64,
    num: i64,
    state: containers::Dictionary,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE rev_membership_list SET state = ?, num = ? WHERE id = ?")
        .bind(DictContainerSql(state).to_bytes())
        .bind(num)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}
