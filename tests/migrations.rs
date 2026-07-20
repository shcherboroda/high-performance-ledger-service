use sqlx::PgPool;

const ACCOUNT_A: &str = "00000000-0000-0000-0000-000000000001";
const ACCOUNT_B: &str = "00000000-0000-0000-0000-000000000002";
const TRANSFER: &str = "00000000-0000-0000-0000-000000000010";
const REVERSAL: &str = "00000000-0000-0000-0000-000000000011";

async fn insert_account(pool: &PgPool, id: &str, balance_minor: i64) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO accounts (id, owner_id, currency, currency_scale, balance_minor) \
         VALUES (CAST($1 AS text)::uuid, 'owner', 'USD', 2, $2)",
    )
    .bind(id)
    .bind(balance_minor)
    .execute(pool)
    .await?;
    Ok(())
}

async fn insert_transfer(pool: &PgPool, id: &str, reverses: Option<&str>) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO transfers (\
             id, source_account_id, destination_account_id, source_currency, destination_currency,\
             source_amount_minor, destination_amount_minor, total_source_debit_minor, kind,\
             reverses_transfer_id, initiated_by\
         ) VALUES (\
             CAST($1 AS text)::uuid, CAST($2 AS text)::uuid, CAST($3 AS text)::uuid, 'USD', 'USD', 100, 100, 100, 'transfer', CAST($4 AS text)::uuid, 'client'\
         )",
    )
    .bind(id)
    .bind(ACCOUNT_A)
    .bind(ACCOUNT_B)
    .bind(reverses)
    .execute(pool)
    .await?;
    Ok(())
}

#[sqlx::test]
async fn migration_creates_the_expected_schema(pool: PgPool) -> sqlx::Result<()> {
    let tables: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.tables WHERE table_schema = 'public' \
         AND table_name IN ('accounts', 'transfers', 'account_entries', 'idempotency_records')",
    )
    .fetch_one(&pool)
    .await?;
    let types: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_type WHERE typname IN ('account_status', 'transfer_kind', 'entry_direction')",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(tables, 4);
    assert_eq!(types, 3);
    Ok(())
}

#[sqlx::test]
async fn accounts_accept_negative_balances_and_reject_invalid_values(
    pool: PgPool,
) -> sqlx::Result<()> {
    insert_account(&pool, ACCOUNT_A, -1).await?;
    for statement in [
        "INSERT INTO accounts (id, owner_id, currency, currency_scale, balance_minor) VALUES ('10000000-0000-0000-0000-000000000001', 'owner', 'usd', 2, 0)",
        "INSERT INTO accounts (id, owner_id, currency, currency_scale, balance_minor) VALUES ('10000000-0000-0000-0000-000000000002', 'owner', 'USD', -1, 0)",
        "INSERT INTO accounts (id, owner_id, currency, currency_scale, balance_minor, version) VALUES ('10000000-0000-0000-0000-000000000003', 'owner', 'USD', 2, 0, -1)",
        "INSERT INTO accounts (id, owner_id, currency, currency_scale, balance_minor) VALUES ('10000000-0000-0000-0000-000000000004', '   ', 'USD', 2, 0)",
    ] {
        assert!(sqlx::query(statement).execute(&pool).await.is_err());
    }
    Ok(())
}

#[sqlx::test]
async fn transfers_and_entries_enforce_financial_invariants(pool: PgPool) -> sqlx::Result<()> {
    insert_account(&pool, ACCOUNT_A, 0).await?;
    insert_account(&pool, ACCOUNT_B, 0).await?;
    insert_transfer(&pool, TRANSFER, None).await?;
    for (id, account, counterparty, direction) in [
        (
            "00000000-0000-0000-0000-000000000020",
            ACCOUNT_A,
            ACCOUNT_B,
            "debit",
        ),
        (
            "00000000-0000-0000-0000-000000000021",
            ACCOUNT_B,
            ACCOUNT_A,
            "credit",
        ),
    ] {
        sqlx::query(
            "INSERT INTO account_entries (id, account_id, transfer_id, counterparty_account_id, direction, operation_kind, amount_minor, currency) \
             VALUES (CAST($1 AS text)::uuid, CAST($2 AS text)::uuid, CAST($3 AS text)::uuid, CAST($4 AS text)::uuid, $5::entry_direction, 'transfer', 100, 'USD')",
        )
        .bind(id).bind(account).bind(TRANSFER).bind(counterparty).bind(direction)
        .execute(&pool).await?;
    }
    assert!(sqlx::query("INSERT INTO transfers (id, source_account_id, destination_account_id, source_currency, destination_currency, source_amount_minor, destination_amount_minor, total_source_debit_minor, kind, initiated_by) VALUES ('00000000-0000-0000-0000-000000000030', '00000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-000000000001', 'USD', 'USD', 1, 1, 1, 'transfer', 'client')").execute(&pool).await.is_err());
    assert!(sqlx::query("INSERT INTO transfers (id, source_account_id, destination_account_id, source_currency, destination_currency, source_amount_minor, destination_amount_minor, fee_amount_minor, total_source_debit_minor, kind, initiated_by) VALUES ('00000000-0000-0000-0000-000000000032', '00000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-000000000002', 'USD', 'USD', 0, 1, -1, 1, 'transfer', 'client')").execute(&pool).await.is_err());
    assert!(sqlx::query("INSERT INTO account_entries (id, account_id, transfer_id, counterparty_account_id, direction, operation_kind, amount_minor, currency, fee_amount_minor) VALUES ('00000000-0000-0000-0000-000000000031', '00000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-000000000010', '00000000-0000-0000-0000-000000000002', 'debit', 'transfer', 0, 'USD', -1)").execute(&pool).await.is_err());
    insert_transfer(&pool, REVERSAL, Some(TRANSFER)).await?;
    assert!(
        insert_transfer(
            &pool,
            "00000000-0000-0000-0000-000000000012",
            Some(TRANSFER)
        )
        .await
        .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM accounts WHERE id = '00000000-0000-0000-0000-000000000001'")
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM transfers WHERE id = '00000000-0000-0000-0000-000000000010'")
            .execute(&pool)
            .await
            .is_err()
    );
    Ok(())
}

#[sqlx::test]
async fn idempotency_records_accept_only_complete_or_reserved_success_rows(
    pool: PgPool,
) -> sqlx::Result<()> {
    sqlx::query("INSERT INTO idempotency_records (client_id, operation_type, idempotency_key, request_fingerprint, expires_at) VALUES ('client', 'transfer', 'reserved', 'fingerprint', now() + interval '1 hour')").execute(&pool).await?;
    sqlx::query("INSERT INTO idempotency_records (client_id, operation_type, idempotency_key, request_fingerprint, http_status, response_body, resulting_resource_id, expires_at) VALUES ('client', 'transfer', 'complete', 'fingerprint', 201, '{}'::jsonb, '00000000-0000-0000-0000-000000000010', now() + interval '1 hour')").execute(&pool).await?;
    for statement in [
        "INSERT INTO idempotency_records (client_id, operation_type, idempotency_key, request_fingerprint, http_status, expires_at) VALUES ('client', 'transfer', 'partial', 'fingerprint', 201, now() + interval '1 hour')",
        "INSERT INTO idempotency_records (client_id, operation_type, idempotency_key, request_fingerprint, http_status, response_body, resulting_resource_id, expires_at) VALUES ('client', 'transfer', 'failure', 'fingerprint', 400, '{}'::jsonb, '00000000-0000-0000-0000-000000000010', now() + interval '1 hour')",
        "INSERT INTO idempotency_records (client_id, operation_type, idempotency_key, request_fingerprint, expires_at) VALUES ('   ', 'transfer', 'blank', 'fingerprint', now() + interval '1 hour')",
        "INSERT INTO idempotency_records (client_id, operation_type, idempotency_key, request_fingerprint, expires_at) VALUES ('client', 'transfer', 'expired', 'fingerprint', now() - interval '1 hour')",
    ] {
        assert!(sqlx::query(statement).execute(&pool).await.is_err());
    }
    Ok(())
}
