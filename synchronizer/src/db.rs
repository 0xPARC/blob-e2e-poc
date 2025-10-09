use anyhow::Result;
use pod2::middleware::{Hash, RawValue};
use sqlx::SqlitePool;
use tables::{HashSql, RawValueSql};

// To dump the formatted table via cli:
// ```
// sqlite3 -header -cmd '.mode columns' /tmp/ad-synchronizer.sqlite 'SELECT hex(id), num, slot, tx_index, blob_index, update_index, timestamp, hex(state) FROM ad_update;'
// ```
pub(crate) async fn init_db(db: &SqlitePool) -> Result<()> {
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

pub(crate) struct Database<E>(pub(crate) E);

/// Implementation of database queries that works with transactions and database:
/// ```
/// Database(&mut *tx)
/// Database(&db)
/// ```
impl<'a, E> Database<E>
where
    E: sqlx::Executor<'a, Database = sqlx::Sqlite>,
{
    pub(crate) async fn add_blob(self, blob: &tables::Blob) -> Result<()> {
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

    pub(crate) async fn add_ad(self, ad: &tables::Ad) -> Result<()> {
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

    pub(crate) async fn add_ad_update(self, update: &tables::AdUpdate) -> Result<()> {
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

    pub(crate) async fn add_visited_slot(self, slot: i64) -> Result<()> {
        sqlx::query("INSERT INTO visited_slot (slot) VALUES (?)")
            .bind(slot)
            .execute(self.0)
            .await?;

        Ok(())
    }

    pub(crate) async fn get_ad(self, ad_id: Hash) -> Result<tables::Ad> {
        Ok(sqlx::query_as("SELECT * FROM ad WHERE id = ?")
            .bind(HashSql(ad_id).to_bytes())
            .fetch_one(self.0)
            .await?)
    }

    pub(crate) async fn get_ad_update_last(self, ad_id: Hash) -> Result<tables::AdUpdate> {
        Ok(
            sqlx::query_as("SELECT * FROM ad_update WHERE id = ? ORDER BY num DESC LIMIT 1")
                .bind(HashSql(ad_id).to_bytes())
                .fetch_one(self.0)
                .await?,
        )
    }

    pub(crate) async fn get_ad_update_last_state(self, ad_id: Hash) -> Result<RawValue> {
        let (state,): (Vec<u8>,) =
            sqlx::query_as("SELECT state FROM ad_update WHERE id = ? ORDER BY num DESC LIMIT 1")
                .bind(HashSql(ad_id).to_bytes())
                .fetch_one(self.0)
                .await?;
        Ok(RawValueSql::try_from(state).expect("32 bytes").0)
    }

    pub(crate) async fn get_visited_slot_last(self) -> Result<u32> {
        let (slot,) = sqlx::query_as("SELECT slot FROM visited_slot ORDER BY slot DESC LIMIT 1")
            .fetch_one(self.0)
            .await?;
        Ok(slot)
    }
}

// SQL tables
pub mod tables {
    use anyhow::Error;
    use common::payload::{
        read_custom_predicate_ref, read_elems, write_custom_predicate_ref, write_elems,
    };
    use pod2::middleware::{CustomPredicateRef, Hash, RawValue};

    pub type B256Sql = [u8; 32];

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
        pub blob_versioned_hash: B256Sql,
    }

    #[derive(Debug, PartialEq, Eq, sqlx::FromRow)]
    pub struct AdUpdate {
        #[sqlx(try_from = "Vec<u8>")]
        pub id: HashSql,
        pub num: i64,
        #[sqlx(try_from = "Vec<u8>")]
        pub state: RawValueSql,
        #[sqlx(try_from = "Vec<u8>")]
        pub blob_versioned_hash: B256Sql,
    }

    #[derive(Debug, PartialEq, Eq, sqlx::FromRow)]
    pub struct Blob {
        #[sqlx(try_from = "Vec<u8>")]
        pub versioned_hash: B256Sql,
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
