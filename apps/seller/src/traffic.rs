use crate::state::{now, App};
use anyhow::Result;
use serde::Serialize;
use sqlx::SqlitePool;
use std::{sync::Arc, time::Duration};
use tokio::sync::oneshot;

const RETENTION: i64 = 24 * 60 * 60;
const MAX_SAMPLE_GAP: i64 = 30;
const SAMPLE_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Clone, Copy)]
struct Sample {
    at: i64,
    rx: u64,
    tx: u64,
    session_start: bool,
}

#[derive(Default)]
pub struct History {
    samples: Vec<Sample>,
}

#[derive(Clone, Serialize)]
pub struct Rate {
    start: i64,
    end: i64,
    rx: f64,
    tx: f64,
}

impl Sample {
    fn rate_since(self, previous: Self, cutoff: i64, end: i64) -> Option<Rate> {
        let elapsed = self.at - previous.at;
        if self.session_start
            || !(1..=MAX_SAMPLE_GAP).contains(&elapsed)
            || self.at <= cutoff
            || self.at > end
        {
            return None;
        }
        Some(Rate {
            start: previous.at.max(cutoff),
            end: self.at,
            rx: self.rx.checked_sub(previous.rx)? as f64 / elapsed as f64,
            tx: self.tx.checked_sub(previous.tx)? as f64 / elapsed as f64,
        })
    }
}

impl History {
    pub(crate) async fn load(pool: &SqlitePool, time: i64) -> Result<Self> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS gateway_traffic (
                id INTEGER PRIMARY KEY, at INTEGER NOT NULL,
                rx INTEGER NOT NULL, tx INTEGER NOT NULL, session_start INTEGER NOT NULL
            )",
        )
        .execute(pool)
        .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS gateway_traffic_at ON gateway_traffic(at)")
            .execute(pool)
            .await?;
        let rows = sqlx::query_as::<_, (i64, i64, i64, bool)>(
            "SELECT at, rx, tx, session_start FROM gateway_traffic
             WHERE at >= ? OR id = (SELECT MAX(id) FROM gateway_traffic) ORDER BY id",
        )
        .bind(time - RETENTION - MAX_SAMPLE_GAP)
        .fetch_all(pool)
        .await?;
        let mut samples = Vec::with_capacity(rows.len());
        for (at, rx, tx, session_start) in rows {
            samples.push(Sample {
                at,
                rx: u64::try_from(rx)?,
                tx: u64::try_from(tx)?,
                session_start,
            });
        }
        Ok(Self { samples })
    }

    pub(crate) fn totals(&self) -> (u64, u64) {
        self.samples
            .last()
            .map_or((0, 0), |sample| (sample.rx, sample.tx))
    }

    async fn persist(&mut self, pool: &SqlitePool, sample: Sample) -> Result<()> {
        let cutoff = sample.at - RETENTION - MAX_SAMPLE_GAP;
        let mut transaction = pool.begin().await?;
        // A shutdown checkpoint may land in the same second as a periodic sample.
        let replace =
            !sample.session_start && self.samples.last().is_some_and(|last| last.at == sample.at);
        let rx = i64::try_from(sample.rx)?;
        let tx = i64::try_from(sample.tx)?;
        if replace {
            sqlx::query("UPDATE gateway_traffic SET rx = ?, tx = ? WHERE id = (SELECT MAX(id) FROM gateway_traffic)")
                .bind(rx).bind(tx).execute(&mut *transaction).await?;
        } else {
            let row = sqlx::query(
                "INSERT INTO gateway_traffic (at, rx, tx, session_start) VALUES (?, ?, ?, ?)",
            )
            .bind(sample.at)
            .bind(rx)
            .bind(tx)
            .bind(sample.session_start)
            .execute(&mut *transaction)
            .await?
            .last_insert_rowid();
            sqlx::query("DELETE FROM gateway_traffic WHERE at < ? AND id < ?")
                .bind(cutoff)
                .bind(row)
                .execute(&mut *transaction)
                .await?;
        }
        transaction.commit().await?;

        if replace {
            let last = self
                .samples
                .last_mut()
                .expect("checkpoint at the same timestamp");
            last.rx = sample.rx;
            last.tx = sample.tx;
        } else {
            self.samples.retain(|previous| previous.at >= cutoff);
            self.samples.push(sample);
        }
        Ok(())
    }

    pub fn rates(&self, end: i64, window: i64) -> Vec<Rate> {
        let cutoff = end - window;
        let bucket = (window / 240).max(1);
        let mut rates: Vec<Rate> = Vec::new();
        for pair in self.samples.windows(2) {
            let Some(rate) = pair[1].rate_since(pair[0], cutoff, end) else {
                continue;
            };
            if let Some(last) = rates.last_mut() {
                if last.end == rate.start && last.start / bucket == rate.start / bucket {
                    let weight = (rate.end - rate.start) as f64 / (rate.end - last.start) as f64;
                    last.rx += (rate.rx - last.rx) * weight;
                    last.tx += (rate.tx - last.tx) * weight;
                    last.end = rate.end;
                    continue;
                }
            }
            rates.push(rate);
        }
        rates
    }

    pub fn latest(&self, end: i64) -> Option<Rate> {
        let [.., previous, current] = self.samples.as_slice() else {
            return None;
        };
        current.rate_since(*previous, end - MAX_SAMPLE_GAP, end)
    }
}

pub async fn checkpoint(app: &App, session_start: bool) -> Result<()> {
    let (rx, tx) = {
        let runtime = app.runtime.read().await;
        (runtime.gw_bytes_in, runtime.gw_bytes_out)
    };
    let sample = Sample {
        at: now(),
        rx,
        tx,
        session_start,
    };
    app.traffic
        .write()
        .await
        .persist(&app.store.pool, sample)
        .await
}

pub async fn collect(app: Arc<App>, mut shutdown: oneshot::Receiver<()>) -> Result<()> {
    loop {
        tokio::select! {
            _ = &mut shutdown => return checkpoint(&app, false).await,
            _ = tokio::time::sleep(SAMPLE_INTERVAL) => {
                if let Err(error) = checkpoint(&app, false).await {
                    tracing::warn!(%error, "could not save gateway traffic");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(at: i64, rx: u64, tx: u64) -> Sample {
        Sample {
            at,
            rx,
            tx,
            session_start: false,
        }
    }

    #[test]
    fn windows_preserve_gaps_and_weighted_rates() {
        let mut history = History::default();
        for i in 0..=9000 {
            if !(5000..5010).contains(&i) {
                history
                    .samples
                    .push(sample(10 * i, i as u64 * 250, i as u64 * 400));
            }
        }
        for window in [300, 3600, 86400] {
            let rates = history.rates(90000, window);
            assert_eq!(rates.first().unwrap().start, 90000 - window);
            assert_eq!(rates.last().unwrap().end, 90000);
            assert!(rates.len() < 300);
            assert!(rates.iter().all(|r| r.rx == 25.0 && r.tx == 40.0));
            assert_eq!(
                rates.windows(2).filter(|p| p[0].end != p[1].start).count(),
                usize::from(window == 86400)
            );
        }
    }

    #[test]
    fn five_minute_view_keeps_published_rates_between_polls() {
        let mut history = History::default();
        history.samples.push(sample(100, 0, 0));
        history.samples.push(sample(102, 40, 60));
        assert_eq!(history.latest(103).unwrap().rx, 20.0);
        history.samples.push(sample(104, 60, 60));
        for now in [104, 105] {
            let points = history.rates(now, 300);
            assert_eq!(points.len(), 2);
            assert_eq!((points[0].end, points[0].rx), (102, 20.0));
            assert_eq!((points[1].end, points[1].rx), (104, 10.0));
            assert_eq!(history.latest(now).unwrap().rx, points[1].rx);
        }
    }

    #[tokio::test]
    async fn restores_totals_and_keeps_a_gap_across_restart() -> Result<()> {
        let pool = SqlitePool::connect("sqlite::memory:").await?;
        let mut history = History::load(&pool, 100).await?;
        history
            .persist(
                &pool,
                Sample {
                    session_start: true,
                    ..sample(100, 0, 0)
                },
            )
            .await?;
        history.persist(&pool, sample(110, 100, 200)).await?;
        history.persist(&pool, sample(110, 110, 220)).await?;
        let mut restarted = History::load(&pool, 115).await?;
        assert_eq!(restarted.totals(), (110, 220));
        assert_eq!(restarted.latest(115).unwrap().rx, 11.0);
        restarted
            .persist(
                &pool,
                Sample {
                    session_start: true,
                    ..sample(115, 110, 220)
                },
            )
            .await?;
        assert!(restarted.latest(115).is_none());
        restarted.persist(&pool, sample(125, 210, 420)).await?;
        let saved = History::load(&pool, 125).await?;
        assert_eq!(
            saved
                .rates(125, 86400)
                .iter()
                .map(|rate| (rate.start, rate.end))
                .collect::<Vec<_>>(),
            [(100, 110), (115, 125)]
        );
        let later = History::load(&pool, 200_000).await?;
        assert_eq!(later.totals(), (210, 420));
        assert!(later.rates(200_000, 86400).is_empty());
        Ok(())
    }
}
