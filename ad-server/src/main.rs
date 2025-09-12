use anyhow::Result;
use sqlx::{migrate::MigrateDatabase, sqlite::Sqlite};

pub mod db;
pub mod endpoints;
pub mod eth;
pub mod pod;

const SQLITE_PATH: &str = "./sqlite.db";

#[tokio::main]
async fn main() -> Result<()> {
    if !Sqlite::database_exists(&SQLITE_PATH).await? {
        Sqlite::create_database(&SQLITE_PATH).await?;
    }
    let db_pool = db::db_connection(&SQLITE_PATH).await?;

    db::init_db(&db_pool).await?;

    let routes = endpoints::routes(db_pool);
    println!("server at http://0.0.0.0:8000");
    warp::serve(routes).run(([0, 0, 0, 0], 8000)).await;

    Ok(())
}
