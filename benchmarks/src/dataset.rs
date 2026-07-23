use anyhow::Result;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScenarioPlan {
    Independent,
    HotAccount,
    IdempotentReplay,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferPlan {
    pub owner: String,
    pub client: usize,
    pub phase: &'static str,
    pub source: usize,
    pub destination: usize,
    pub key: String,
}

pub fn subject(seed: u64, client: usize) -> String {
    format!("benchmark-{seed}-{client}")
}

pub fn plans(
    scenario: ScenarioPlan,
    seed: u64,
    clients: usize,
    warmup: usize,
    measured: usize,
) -> Vec<TransferPlan> {
    (0..(warmup + measured))
        .map(|index| {
            let phase = if index < warmup { "warmup" } else { "measured" };
            let client = index % clients;
            let (source, destination) = match scenario {
                ScenarioPlan::Independent | ScenarioPlan::IdempotentReplay => (index, index),
                ScenarioPlan::HotAccount => (if phase == "warmup" { 0 } else { 1 }, index),
            };
            TransferPlan {
                owner: subject(seed, client),
                client,
                phase,
                source,
                destination,
                key: format!("benchmark-{seed}-{phase}-{index}-{client}"),
            }
        })
        .collect()
}

pub fn deterministic_uuid(seed: u64, level: usize, index: usize) -> Uuid {
    Uuid::new_v5(
        &Uuid::NAMESPACE_OID,
        format!("benchmark-{seed}-{level}-{index}").as_bytes(),
    )
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
        let a = plans(ScenarioPlan::Independent, 9, 2, 1, 3);
        assert_eq!(a, plans(ScenarioPlan::Independent, 9, 2, 1, 3));
        assert_eq!(a[0].phase, "warmup");
        assert!(a[1..].iter().all(|p| p.phase == "measured"));
    }
    #[test]
    fn hot_account_plans_share_only_the_source() {
        let plans = plans(ScenarioPlan::HotAccount, 1, 2, 0, 3);
        assert!(plans.iter().all(|plan| plan.source == 1));
        assert_eq!(
            plans
                .iter()
                .map(|plan| plan.destination)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }
    #[test]
    fn replay_plans_are_stable() {
        assert_eq!(
            plans(ScenarioPlan::IdempotentReplay, 1, 1, 0, 2),
            plans(ScenarioPlan::IdempotentReplay, 1, 1, 0, 2)
        );
    }
}
