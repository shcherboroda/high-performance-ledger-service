use anyhow::Result;
use uuid::Uuid;

pub const NAMESPACE: Uuid = Uuid::from_u128(0x89d451aa_916e_4e2e_8bc6_4c051d1baf90);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairPlan {
    pub owner: String,
    pub source_id: Uuid,
    pub destination_id: Uuid,
    pub phase: &'static str,
}

pub fn subject(seed: u64, client: usize) -> String {
    format!("benchmark-{seed}-{client}")
}

pub fn plans(seed: u64, clients: usize, warmup: usize, measured: usize) -> Vec<PairPlan> {
    (0..(warmup + measured))
        .map(|index| {
            let phase = if index < warmup { "warmup" } else { "measured" };
            let name = format!("{seed}:{phase}:{index}");
            PairPlan {
                owner: subject(seed, index % clients),
                source_id: Uuid::new_v5(&NAMESPACE, format!("{name}:source").as_bytes()),
                destination_id: Uuid::new_v5(&NAMESPACE, format!("{name}:destination").as_bytes()),
                phase,
            }
        })
        .collect()
}

pub async fn migrate_and_clean(pool: &sqlx::PgPool, seed: u64) -> Result<()> {
    sqlx::migrate!("../migrations").run(pool).await?;
    let prefix = format!("benchmark-{seed}-%");
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM idempotency_records WHERE client_id LIKE $1")
        .bind(&prefix)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM account_entries WHERE transfer_id IN (SELECT id FROM transfers WHERE initiated_by LIKE $1)").bind(&prefix).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM transfers WHERE initiated_by LIKE $1")
        .bind(&prefix)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM accounts WHERE owner_id LIKE $1")
        .bind(&prefix)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plans_are_deterministic_and_phase_separated() {
        let a = plans(9, 2, 1, 3);
        assert_eq!(a, plans(9, 2, 1, 3));
        assert_eq!(a[0].phase, "warmup");
        assert!(a[1..].iter().all(|p| p.phase == "measured"));
        assert_ne!(a[0].source_id, a[1].source_id);
    }
}
