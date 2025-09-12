use log::LevelFilter;
use serde::{Deserialize, Serialize};
use sqlx::{
    migrate::MigrateDatabase,
    sqlite::{Sqlite, SqliteConnectOptions},
    ConnectOptions, FromRow, SqlitePool,
};
use std::str::FromStr;
use std::time::Duration;

#[derive(Debug, FromRow, Serialize, Deserialize)]
pub struct Counter {
    pub id: i64, // TODO maybe use u64 (check db compat)
    pub count: i64,
    // pod, proof, etc
}

// TODO maybe unify db logic with synchronizer's db helpers (not the db itself)
pub async fn db_connection(url: &str) -> Result<SqlitePool, sqlx::Error> {
    let opts = SqliteConnectOptions::from_str(url)?
        .serialized(false)
        .busy_timeout(Duration::from_secs(3600))
        .log_statements(LevelFilter::Debug)
        .log_slow_statements(LevelFilter::Warn, Duration::from_millis(800));
    Ok(SqlitePool::connect_with(opts).await?)
}

pub async fn init_db(db_pool: &SqlitePool) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS counters (
            id INTEGER PRIMARY KEY,
            count INTEGER NOT NULL
        )
        "#,
    )
    .execute(db_pool)
    .await?;

    Ok(())
}

// DB METHODS:

pub async fn insert_counter(pool: &SqlitePool, counter: &Counter) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO counters (id, count) VALUES (?, ?);")
        .bind(counter.id)
        .bind(counter.count)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn get_counter(pool: &SqlitePool, id: i64) -> Result<Counter, sqlx::Error> {
    let counter = sqlx::query_as::<_, Counter>("SELECT id, count FROM counters WHERE id = ?;")
        .bind(id)
        .fetch_one(pool)
        .await?;
    Ok(counter)
}

pub async fn update_count(pool: &SqlitePool, id: i64, new_count: i64) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE counters SET count = ? WHERE id = ?")
        .bind(new_count)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}
