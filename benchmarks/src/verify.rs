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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplaySnapshot {
    pub transfers: i64,
    pub entries: i64,
    pub balances: BTreeMap<Uuid, i64>,
    pub idempotency_records: i64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountPoolSnapshot {
    pub transfers: i64,
    pub entries: i64,
    pub idempotency_records: i64,
    pub duplicate_side_effects: i64,
    pub balances: BTreeMap<Uuid, i64>,
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
    let committed = transfer_count(pool, &sources).await?;
    let entries = entry_count(pool, &sources).await?;
    let duplicates: i64 = sqlx::query_scalar("SELECT count(*) FROM (SELECT source_account_id FROM transfers WHERE source_account_id=ANY($1) GROUP BY source_account_id HAVING count(*) > 1) duplicate_groups").bind(&sources).fetch_one(pool).await?;
    Ok(evaluate_independent(
        committed,
        entries,
        duplicates,
        balances(
            pool,
            &pairs.iter().flat_map(|(s, d)| [*s, *d]).collect::<Vec<_>>(),
        )
        .await?,
        pairs,
        expected,
    ))
}
pub fn evaluate_independent(
    committed: i64,
    entries: i64,
    duplicates: i64,
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
            "no_duplicate_transfer_side_effects",
            duplicates == 0,
            format!("duplicate transfer groups: {duplicates}"),
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
    let committed = transfer_count(pool, &[source]).await?;
    let entries = entry_count(pool, &[source]).await?;
    let duplicates: i64 = sqlx::query_scalar("SELECT count(*) FROM (SELECT destination_account_id FROM transfers WHERE source_account_id=$1 GROUP BY destination_account_id HAVING count(*) > 1) duplicate_groups").bind(source).fetch_one(pool).await?;
    let mut ids = destinations.to_vec();
    ids.push(source);
    Ok(evaluate_hot(
        committed,
        entries,
        duplicates,
        balances(pool, &ids).await?,
        source,
        destinations,
        expected,
    ))
}
pub fn evaluate_hot(
    committed: i64,
    entries: i64,
    duplicates: i64,
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
            "no_duplicate_transfer_side_effects",
            duplicates == 0,
            format!("duplicate destination groups: {duplicates}"),
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
        check(
            "no_unexpected_overdrafts",
            balances.values().all(|b| *b >= 0),
            "all balances non-negative".into(),
        ),
    ])
}

pub async fn replay_snapshot(
    pool: &sqlx::PgPool,
    accounts: &[(Uuid, Uuid)],
    client_prefix: &str,
    keys: &[String],
) -> anyhow::Result<ReplaySnapshot> {
    let sources = accounts
        .iter()
        .map(|(source, _)| *source)
        .collect::<Vec<_>>();
    let ids = accounts
        .iter()
        .flat_map(|(source, destination)| [*source, *destination])
        .collect::<Vec<_>>();
    let idempotency_records:i64=sqlx::query_scalar("SELECT count(*) FROM idempotency_records WHERE client_id LIKE $1 AND operation_type='transfer' AND idempotency_key=ANY($2)").bind(client_prefix).bind(keys).fetch_one(pool).await?;
    Ok(ReplaySnapshot {
        transfers: transfer_count(pool, &sources).await?,
        entries: entry_count(pool, &sources).await?,
        balances: balances(pool, &ids).await?,
        idempotency_records,
    })
}
pub fn evaluate_replay(
    before: &ReplaySnapshot,
    after: &ReplaySnapshot,
    expected: usize,
    request_ids_match: bool,
) -> Verification {
    finish(vec![
        check(
            "replays_return_original_transfer_ids",
            request_ids_match,
            "each replay key must return its prepared transfer ID".into(),
        ),
        check(
            "no_additional_transfers",
            after.transfers == before.transfers && before.transfers == expected as i64,
            format!(
                "before {}, after {}, expected {expected}",
                before.transfers, after.transfers
            ),
        ),
        check(
            "no_additional_entries",
            after.entries == before.entries && before.entries == expected as i64 * 2,
            format!(
                "before {}, after {}, expected {}",
                before.entries,
                after.entries,
                expected * 2
            ),
        ),
        check(
            "balances_unchanged",
            after.balances == before.balances,
            "replay phase must not change balances".into(),
        ),
        check(
            "one_idempotency_record_per_prepared_key",
            after.idempotency_records == before.idempotency_records
                && before.idempotency_records == expected as i64,
            format!(
                "before {}, after {}, expected {expected}",
                before.idempotency_records, after.idempotency_records
            ),
        ),
    ])
}

pub async fn account_pool(
    pool: &sqlx::PgPool,
    account_ids: &[Uuid],
    expected_balances: &[i64],
    measured_keys: &[String],
    expected_transfers: usize,
) -> anyhow::Result<Verification> {
    let balances = balances(pool, account_ids).await?;
    let transfers: i64 = sqlx::query_scalar("SELECT count(*) FROM transfers WHERE id IN (SELECT resulting_resource_id FROM idempotency_records WHERE operation_type='transfer' AND idempotency_key=ANY($1))").bind(measured_keys).fetch_one(pool).await?;
    let entries: i64 = sqlx::query_scalar("SELECT count(*) FROM account_entries WHERE transfer_id IN (SELECT resulting_resource_id FROM idempotency_records WHERE operation_type='transfer' AND idempotency_key=ANY($1))").bind(measured_keys).fetch_one(pool).await?;
    let records: i64 = sqlx::query_scalar("SELECT count(*) FROM idempotency_records WHERE operation_type='transfer' AND idempotency_key=ANY($1)").bind(measured_keys).fetch_one(pool).await?;
    let duplicate_side_effects: i64 = sqlx::query_scalar("SELECT count(*) FROM (SELECT resulting_resource_id FROM idempotency_records WHERE operation_type='transfer' AND idempotency_key=ANY($1) GROUP BY resulting_resource_id HAVING count(*) > 1) duplicates").bind(measured_keys).fetch_one(pool).await?;
    Ok(evaluate_account_pool(
        AccountPoolSnapshot {
            transfers,
            entries,
            idempotency_records: records,
            duplicate_side_effects,
            balances,
        },
        account_ids,
        expected_balances,
        expected_transfers,
    ))
}

pub fn evaluate_account_pool(
    snapshot: AccountPoolSnapshot,
    account_ids: &[Uuid],
    expected_balances: &[i64],
    expected: usize,
) -> Verification {
    let actual = account_ids
        .iter()
        .map(|id| snapshot.balances.get(id).copied())
        .collect::<Vec<_>>();
    finish(vec![
        check(
            "committed_measured_transfers",
            snapshot.transfers == expected as i64,
            format!("expected {expected}, found {}", snapshot.transfers),
        ),
        check(
            "exactly_two_entries_per_transfer",
            snapshot.entries == expected as i64 * 2,
            format!("expected {}, found {}", expected * 2, snapshot.entries),
        ),
        check(
            "one_idempotency_record_per_planned_key",
            snapshot.idempotency_records == expected as i64,
            format!(
                "expected {expected}, found {}",
                snapshot.idempotency_records
            ),
        ),
        check(
            "no_duplicate_transfer_side_effects",
            snapshot.duplicate_side_effects == 0,
            format!(
                "duplicate transfer side-effect groups: {}",
                snapshot.duplicate_side_effects
            ),
        ),
        check(
            "entire_account_pool_exists",
            snapshot.balances.len() == account_ids.len(),
            format!(
                "expected {}, found {}",
                account_ids.len(),
                snapshot.balances.len()
            ),
        ),
        check(
            "expected_final_balances",
            actual
                .iter()
                .zip(expected_balances)
                .all(|(actual, expected)| *actual == Some(*expected)),
            "every pool balance must match deterministic simulation".into(),
        ),
        check(
            "no_unexpected_overdrafts",
            snapshot.balances.values().all(|balance| *balance >= 0),
            "all balances non-negative".into(),
        ),
        check(
            "total_conservation",
            snapshot.balances.values().sum::<i64>() == expected_balances.iter().sum::<i64>(),
            "pool total must be conserved".into(),
        ),
    ])
}
async fn transfer_count(pool: &sqlx::PgPool, sources: &[Uuid]) -> anyhow::Result<i64> {
    Ok(sqlx::query_scalar(
        "SELECT count(*) FROM transfers WHERE source_account_id=ANY($1) AND kind='transfer'",
    )
    .bind(sources)
    .fetch_one(pool)
    .await?)
}
async fn entry_count(pool: &sqlx::PgPool, sources: &[Uuid]) -> anyhow::Result<i64> {
    Ok(sqlx::query_scalar("SELECT count(*) FROM account_entries WHERE transfer_id IN (SELECT id FROM transfers WHERE source_account_id=ANY($1))").bind(sources).fetch_one(pool).await?)
}
async fn balances(pool: &sqlx::PgPool, ids: &[Uuid]) -> anyhow::Result<BTreeMap<Uuid, i64>> {
    Ok(
        sqlx::query_as::<_, (Uuid, i64)>("SELECT id,balance_minor FROM accounts WHERE id=ANY($1)")
            .bind(ids)
            .fetch_all(pool)
            .await?
            .into_iter()
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot(transfers: i64, entries: i64, balance: i64, records: i64) -> ReplaySnapshot {
        ReplaySnapshot {
            transfers,
            entries,
            balances: BTreeMap::from([(Uuid::nil(), balance)]),
            idempotency_records: records,
        }
    }
    fn pool_snapshot(
        transfers: i64,
        entries: i64,
        records: i64,
        duplicates: i64,
        balances: BTreeMap<Uuid, i64>,
    ) -> AccountPoolSnapshot {
        AccountPoolSnapshot {
            transfers,
            entries,
            idempotency_records: records,
            duplicate_side_effects: duplicates,
            balances,
        }
    }
    #[test]
    fn independent_duplicate_is_invalid() {
        let id = Uuid::nil();
        assert!(
            !evaluate_independent(
                1,
                2,
                1,
                BTreeMap::from([(id, 900), (Uuid::max(), 1100)]),
                &[(id, Uuid::max())],
                1
            )
            .valid
        )
    }
    #[test]
    fn hot_duplicate_and_overdraft_are_invalid() {
        let s = Uuid::nil();
        let d = Uuid::max();
        let result = evaluate_hot(1, 2, 1, BTreeMap::from([(s, -1), (d, 1100)]), s, &[d], 1);
        assert!(!result.valid)
    }
    #[test]
    fn replay_rejects_all_side_effect_mismatches() {
        let before = snapshot(1, 2, 1000, 1);
        for after in [
            snapshot(2, 2, 1000, 1),
            snapshot(1, 3, 1000, 1),
            snapshot(1, 2, 999, 1),
            snapshot(1, 2, 1000, 2),
        ] {
            assert!(!evaluate_replay(&before, &after, 1, true).valid)
        }
        assert!(!evaluate_replay(&before, &before, 1, false).valid)
    }
    #[test]
    fn account_pool_rejects_missing_balances_counts_and_conservation_failures() {
        let ids = [Uuid::nil(), Uuid::max()];
        let valid = || {
            evaluate_account_pool(
                pool_snapshot(
                    2,
                    4,
                    2,
                    0,
                    BTreeMap::from([(ids[0], 10_000), (ids[1], 10_000)]),
                ),
                &ids,
                &[10_000, 10_000],
                2,
            )
        };
        assert!(valid().valid);
        assert!(
            !evaluate_account_pool(
                pool_snapshot(
                    1,
                    4,
                    2,
                    0,
                    BTreeMap::from([(ids[0], 10_000), (ids[1], 10_000)])
                ),
                &ids,
                &[10_000, 10_000],
                2
            )
            .valid
        );
        assert!(
            !evaluate_account_pool(
                pool_snapshot(
                    2,
                    4,
                    2,
                    1,
                    BTreeMap::from([(ids[0], 10_000), (ids[1], 10_000)])
                ),
                &ids,
                &[10_000, 10_000],
                2
            )
            .valid
        );
        assert!(
            !evaluate_account_pool(
                pool_snapshot(
                    2,
                    3,
                    2,
                    0,
                    BTreeMap::from([(ids[0], 10_000), (ids[1], 10_000)])
                ),
                &ids,
                &[10_000, 10_000],
                2
            )
            .valid
        );
        assert!(
            !evaluate_account_pool(
                pool_snapshot(
                    2,
                    4,
                    1,
                    0,
                    BTreeMap::from([(ids[0], 10_000), (ids[1], 10_000)])
                ),
                &ids,
                &[10_000, 10_000],
                2
            )
            .valid
        );
        assert!(
            !evaluate_account_pool(
                pool_snapshot(2, 4, 2, 0, BTreeMap::from([(ids[0], -1)])),
                &ids,
                &[10_000, 10_000],
                2
            )
            .valid
        );
        assert!(
            !evaluate_account_pool(
                pool_snapshot(
                    2,
                    4,
                    2,
                    0,
                    BTreeMap::from([(ids[0], 10_001), (ids[1], 10_000)])
                ),
                &ids,
                &[10_000, 10_000],
                2
            )
            .valid
        );
    }
}
