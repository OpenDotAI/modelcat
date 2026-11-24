use crate::state::LocalRecord;
use anyhow::Result;
use sqlx::{Sqlite, SqlitePool, Transaction};

async fn accumulate(tx: &mut Transaction<'_, Sqlite>, record: &LocalRecord) -> Result<()> {
    sqlx::query(
        "INSERT INTO token_minutes (minute, requests, input, output, cache)
        VALUES (?, 1, ?, ?, ?) ON CONFLICT(minute) DO UPDATE SET
        requests = requests + 1, input = input + excluded.input,
        output = output + excluded.output, cache = cache + excluded.cache",
    )
    .bind(record.at.div_euclid(60) * 60)
    .bind(record.input_tokens)
    .bind(record.output_tokens)
    .bind(record.cache_tokens)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn archive(tx: &mut Transaction<'_, Sqlite>, record: &LocalRecord) -> Result<()> {
    sqlx::query("INSERT OR IGNORE INTO request_history (id, at, source, status, payload) VALUES (?, ?, ?, ?, ?)")
        .bind(&record.id).bind(record.at).bind(record.source_id.as_deref().unwrap_or(""))
        .bind(&record.status).bind(serde_json::to_string(record)?).execute(&mut **tx).await?;
    Ok(())
}

pub async fn initialize(pool: &SqlitePool, records: &[LocalRecord]) -> Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("CREATE TABLE IF NOT EXISTS token_minutes (minute INTEGER PRIMARY KEY,
        requests INTEGER NOT NULL, input INTEGER NOT NULL, output INTEGER NOT NULL, cache INTEGER NOT NULL)")
        .execute(&mut *tx).await?;
    sqlx::query("CREATE TABLE IF NOT EXISTS request_history (id TEXT NOT NULL UNIQUE, at INTEGER NOT NULL, source TEXT NOT NULL, status TEXT NOT NULL, payload TEXT NOT NULL)").execute(&mut *tx).await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS request_history_time ON request_history(at)").execute(&mut *tx).await?;
    if sqlx::query("INSERT OR IGNORE INTO settings(k,v) VALUES ('request_history.v1','1')")
        .execute(&mut *tx).await?.rows_affected() > 0 {
        for record in records { archive(&mut tx, record).await?; }
    }
    let inserted =
        sqlx::query("INSERT OR IGNORE INTO settings(k,v) VALUES ('token_minutes.v1','1')")
            .execute(&mut *tx)
            .await?
            .rows_affected();
    if inserted > 0 {
        for record in records {
            accumulate(&mut tx, record).await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

pub async fn save(pool: &SqlitePool, record: &LocalRecord, recent: &str) -> Result<()> {
    let mut tx = pool.begin().await?;
    archive(&mut tx, record).await?;
    accumulate(&mut tx, record).await?;
    sqlx::query("DELETE FROM token_minutes WHERE minute < ?")
        .bind(crate::state::now() - 86460)
        .execute(&mut *tx)
        .await?;
    write_recent(&mut tx, recent).await?;
    tx.commit().await?;
    Ok(())
}

pub async fn update_detail(pool: &SqlitePool, record: &LocalRecord, recent: &str) -> Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("UPDATE request_history SET payload = ? WHERE id = ?")
        .bind(serde_json::to_string(record)?).bind(&record.id).execute(&mut *tx).await?;
    write_recent(&mut tx, recent).await?;
    tx.commit().await?;
    Ok(())
}

async fn write_recent(tx: &mut Transaction<'_, Sqlite>, recent: &str) -> Result<()> {
    sqlx::query("INSERT OR REPLACE INTO settings(k,v) VALUES ('node.records', ?)")
        .bind(recent).execute(&mut **tx).await?;
    Ok(())
}
