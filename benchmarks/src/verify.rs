use serde::Serialize;
use std::collections::BTreeMap;
use uuid::Uuid;
#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub name: String,
    pub passed: bool,
    pub detail: String,
}
#[derive(Debug, Clone, Serialize)]
pub struct Verification {
    pub checks: Vec<Check>,
    pub valid: bool,
}
fn check(name: &str, passed: bool, detail: String) -> Check {
    Check {
        name: name.into(),
        passed,
        detail,
    }
}
fn finish(checks: Vec<Check>) -> Verification {
    let valid = checks.iter().all(|check| check.passed);
    Verification { checks, valid }
}
pub async fn independent(
    pool: &sqlx::PgPool,
    pairs: &[(Uuid, Uuid)],
    expected: usize,
) -> anyhow::Result<Verification> {
    let sources: Vec<Uuid> = pairs.iter().map(|pair| pair.0).collect();
    let committed: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM transfers WHERE source_account_id=ANY($1) AND kind='transfer'",
    )
    .bind(&sources)
    .fetch_one(pool)
    .await?;
    let entries:i64=sqlx::query_scalar("SELECT count(*) FROM account_entries WHERE transfer_id IN (SELECT id FROM transfers WHERE source_account_id=ANY($1))").bind(&sources).fetch_one(pool).await?;
    let ids: Vec<Uuid> = pairs.iter().flat_map(|p| [p.0, p.1]).collect();
    let balances: BTreeMap<Uuid, i64> =
        sqlx::query_as::<_, (Uuid, i64)>("SELECT id,balance_minor FROM accounts WHERE id=ANY($1)")
            .bind(&ids)
            .fetch_all(pool)
            .await?
            .into_iter()
            .collect();
    Ok(evaluate_independent(
        committed, entries, balances, pairs, expected,
    ))
}
pub fn evaluate_independent(
    committed: i64,
    entries: i64,
    balances: BTreeMap<Uuid, i64>,
    pairs: &[(Uuid, Uuid)],
    expected: usize,
) -> Verification {
    finish(vec![
        check(
            "committed_measured_transfers",
            committed == expected as i64,
            format!("expected {expected}, found {committed}"),
        ),
        check(
            "exactly_two_entries_per_transfer",
            entries == expected as i64 * 2,
            format!("expected {}, found {entries}", expected * 2),
        ),
        check(
            "expected_final_balances_and_pair_conservation",
            pairs
                .iter()
                .all(|(s, d)| balances.get(s) == Some(&900) && balances.get(d) == Some(&1100)),
            "each pair must be 9.00 / 11.00".into(),
        ),
        check(
            "no_unexpected_overdrafts",
            balances.values().all(|b| *b >= 0),
            "all balances non-negative".into(),
        ),
    ])
}
pub async fn hot_account(
    pool: &sqlx::PgPool,
    source: Uuid,
    destinations: &[Uuid],
    expected: usize,
) -> anyhow::Result<Verification> {
    let committed: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM transfers WHERE source_account_id=$1 AND kind='transfer'",
    )
    .bind(source)
    .fetch_one(pool)
    .await?;
    let entries:i64=sqlx::query_scalar("SELECT count(*) FROM account_entries WHERE transfer_id IN (SELECT id FROM transfers WHERE source_account_id=$1)").bind(source).fetch_one(pool).await?;
    let mut ids = destinations.to_vec();
    ids.push(source);
    let balances: BTreeMap<Uuid, i64> =
        sqlx::query_as::<_, (Uuid, i64)>("SELECT id,balance_minor FROM accounts WHERE id=ANY($1)")
            .bind(&ids)
            .fetch_all(pool)
            .await?
            .into_iter()
            .collect();
    Ok(evaluate_hot(
        committed,
        entries,
        balances,
        source,
        destinations,
        expected,
    ))
}
pub fn evaluate_hot(
    committed: i64,
    entries: i64,
    balances: BTreeMap<Uuid, i64>,
    source: Uuid,
    destinations: &[Uuid],
    expected: usize,
) -> Verification {
    finish(vec![
        check(
            "committed_measured_transfers",
            committed == expected as i64,
            format!("expected {expected}, found {committed}"),
        ),
        check(
            "exactly_two_entries_per_transfer",
            entries == expected as i64 * 2,
            format!("expected {}, found {entries}", expected * 2),
        ),
        check(
            "shared_source_final_balance",
            balances.get(&source) == Some(&1000),
            "source balance must equal initial funding minus measured amount".into(),
        ),
        check(
            "every_destination_received_once",
            destinations
                .iter()
                .all(|id| balances.get(id) == Some(&1100)),
            "every destination must be 11.00".into(),
        ),
        check(
            "total_conservation",
            balances.values().sum::<i64>() == (expected as i64 * 1100) + 1000,
            "sum of shared-source and destination balances must be conserved".into(),
        ),
    ])
}
pub async fn replay(
    pool: &sqlx::PgPool,
    transfer_ids: &[Uuid],
    accounts: &[Uuid],
    before: &BTreeMap<Uuid, i64>,
) -> anyhow::Result<Verification> {
    let transfers: i64 = sqlx::query_scalar("SELECT count(*) FROM transfers WHERE id=ANY($1)")
        .bind(transfer_ids)
        .fetch_one(pool)
        .await?;
    let entries: i64 =
        sqlx::query_scalar("SELECT count(*) FROM account_entries WHERE transfer_id=ANY($1)")
            .bind(transfer_ids)
            .fetch_one(pool)
            .await?;
    let after: BTreeMap<Uuid, i64> =
        sqlx::query_as::<_, (Uuid, i64)>("SELECT id,balance_minor FROM accounts WHERE id=ANY($1)")
            .bind(accounts)
            .fetch_all(pool)
            .await?
            .into_iter()
            .collect();
    Ok(evaluate_replay(
        transfers,
        entries,
        after,
        before,
        transfer_ids.len(),
    ))
}
pub fn evaluate_replay(
    transfers: i64,
    entries: i64,
    after: BTreeMap<Uuid, i64>,
    before: &BTreeMap<Uuid, i64>,
    expected: usize,
) -> Verification {
    finish(vec![
        check(
            "no_additional_transfers",
            transfers == expected as i64,
            format!("expected {expected}, found {transfers}"),
        ),
        check(
            "no_additional_entries",
            entries == expected as i64 * 2,
            format!("expected {}, found {entries}", expected * 2),
        ),
        check(
            "balances_unchanged",
            &after == before,
            "replay phase must not change balances".into(),
        ),
    ])
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hot_mismatch_is_invalid() {
        let s = Uuid::new_v4();
        let d = Uuid::new_v4();
        assert!(!evaluate_hot(1, 2, BTreeMap::from([(s, 99), (d, 1100)]), s, &[d], 1).valid)
    }
    #[test]
    fn replay_mismatch_is_invalid() {
        assert!(!evaluate_replay(2, 2, BTreeMap::new(), &BTreeMap::new(), 1).valid)
    }
}
