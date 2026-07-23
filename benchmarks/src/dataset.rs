use anyhow::Result;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScenarioPlan {
    Independent,
    HotAccount,
    IdempotentReplay,
    AccountPool,
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

pub const ACCOUNT_POOL_INITIAL_BALANCE_MINOR: i64 = 10_000;

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
            let client = match scenario {
                ScenarioPlan::HotAccount => 0,
                ScenarioPlan::Independent | ScenarioPlan::IdempotentReplay => index % clients,
                ScenarioPlan::AccountPool => unreachable!("use account_pool_plans"),
            };
            let (source, destination) = match scenario {
                ScenarioPlan::Independent | ScenarioPlan::IdempotentReplay => (index, index),
                ScenarioPlan::AccountPool => unreachable!("use account_pool_plans"),
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

pub fn account_pool_plans(
    seed: u64,
    clients: usize,
    pool_size: usize,
    warmup: usize,
    measured: usize,
) -> Vec<TransferPlan> {
    let offset = (seed as usize % (pool_size - 1)) + 1;
    (0..(warmup + measured))
        .map(|index| {
            let source = (index + seed as usize) % pool_size;
            let destination = (source + offset) % pool_size;
            let phase = if index < warmup { "warmup" } else { "measured" };
            TransferPlan {
                owner: subject(seed, source % clients),
                client: source % clients,
                phase,
                source,
                destination,
                key: format!("benchmark-{seed}-account-pool-{phase}-{index}-{source}"),
            }
        })
        .collect()
}

pub fn expected_pool_balances(pool_size: usize, plans: &[TransferPlan]) -> Vec<i64> {
    let mut balances = vec![ACCOUNT_POOL_INITIAL_BALANCE_MINOR; pool_size];
    for plan in plans {
        balances[plan.source] -= 100;
        balances[plan.destination] += 100;
    }
    balances
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
        assert!(plans.iter().all(|plan| plan.client == 0));
    }
    #[test]
    fn replay_plans_are_stable() {
        assert_eq!(
            plans(ScenarioPlan::IdempotentReplay, 1, 1, 0, 2),
            plans(ScenarioPlan::IdempotentReplay, 1, 1, 0, 2)
        );
    }
    #[test]
    fn account_pool_plan_is_deterministic_safe_and_uses_distinct_accounts() {
        let plans = account_pool_plans(9, 3, 4, 2, 10);
        assert_eq!(plans, account_pool_plans(9, 3, 4, 2, 10));
        assert!(plans.iter().all(|plan| plan.source != plan.destination));
        assert_eq!(
            plans
                .iter()
                .map(|plan| &plan.key)
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            plans.len()
        );
        assert!(
            expected_pool_balances(4, &plans)
                .iter()
                .all(|balance| *balance >= 0)
        );
        assert!(plans.iter().all(|plan| plan.client == plan.source % 3));
        assert_ne!(
            plans[0].source,
            account_pool_plans(10, 3, 4, 2, 10)[0].source
        );
    }
    #[test]
    fn account_pool_options_do_not_change_existing_scenario_plans() {
        let baseline = vec![
            TransferPlan {
                owner: "benchmark-2-0".into(),
                client: 0,
                phase: "warmup",
                source: 0,
                destination: 0,
                key: "benchmark-2-warmup-0-0".into(),
            },
            TransferPlan {
                owner: "benchmark-2-1".into(),
                client: 1,
                phase: "measured",
                source: 1,
                destination: 1,
                key: "benchmark-2-measured-1-1".into(),
            },
            TransferPlan {
                owner: "benchmark-2-0".into(),
                client: 0,
                phase: "measured",
                source: 2,
                destination: 2,
                key: "benchmark-2-measured-2-0".into(),
            },
            TransferPlan {
                owner: "benchmark-2-1".into(),
                client: 1,
                phase: "measured",
                source: 3,
                destination: 3,
                key: "benchmark-2-measured-3-1".into(),
            },
        ];
        assert_eq!(plans(ScenarioPlan::Independent, 2, 2, 1, 3), baseline);
    }
}
