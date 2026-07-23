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

pub async fn verify(
    pool: &sqlx::PgPool,
    measured: &[(Uuid, Uuid)],
    expected: usize,
) -> anyhow::Result<Verification> {
    let sources: Vec<Uuid> = measured.iter().map(|(source, _)| *source).collect();
    let committed: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM transfers WHERE source_account_id = ANY($1) AND kind = 'transfer'",
    )
    .bind(&sources)
    .fetch_one(pool)
    .await?;
    let entries: i64 = sqlx::query_scalar("SELECT count(*) FROM account_entries WHERE transfer_id IN (SELECT id FROM transfers WHERE source_account_id = ANY($1))").bind(&sources).fetch_one(pool).await?;
    let duplicates: i64 = sqlx::query_scalar("SELECT count(*) FROM (SELECT source_account_id, count(*) FROM transfers WHERE source_account_id = ANY($1) GROUP BY source_account_id HAVING count(*) > 1) duplicate_keys").bind(&sources).fetch_one(pool).await?;
    let ids: Vec<Uuid> = measured.iter().flat_map(|(a, b)| [*a, *b]).collect();
    let balances: Vec<(Uuid, i64)> =
        sqlx::query_as("SELECT id, balance_minor FROM accounts WHERE id = ANY($1)")
            .bind(&ids)
            .fetch_all(pool)
            .await?;
    let map = balances.into_iter().collect::<BTreeMap<_, _>>();
    let pair_balances = measured.iter().all(|(source, destination)| {
        map.get(source)
            .zip(map.get(destination))
            .is_some_and(|(source, destination)| *source == 900 && *destination == 1100)
    });
    let no_overdraft = map.values().all(|balance| *balance >= 0);
    let checks = vec![
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
            format!("duplicate operation groups: {duplicates}"),
        ),
        check(
            "expected_final_balances_and_pair_conservation",
            pair_balances,
            "each measured pair must be 9.00 / 11.00".into(),
        ),
        check(
            "no_unexpected_overdrafts",
            no_overdraft,
            "all measured account balances must be non-negative".into(),
        ),
    ];
    Ok(Verification {
        valid: checks.iter().all(|check| check.passed),
        checks,
    })
}
fn check(name: &str, passed: bool, detail: String) -> Check {
    Check {
        name: name.into(),
        passed,
        detail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn verification_reports_failed_check() {
        let check = check("count", false, "mismatch".into());
        assert!(
            !Verification {
                valid: check.passed,
                checks: vec![check]
            }
            .valid
        );
    }
}
