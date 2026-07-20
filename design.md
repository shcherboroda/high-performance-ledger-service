# Ledger Service Design

## 1. Overview

The Ledger Service provides authenticated REST APIs for account creation, balance reads, atomic transfers, transfer reversal and account-centric transaction history.

The design prioritizes:

* strong consistency of balances;
* safe concurrent execution across stateless instances;
* short financial transactions;
* immutable transfer records;
* efficient account-history reads;
* explicit idempotency and crash recovery;
* optional cross-currency transfers.

PostgreSQL is the authoritative source of financial state and cross-instance coordination.

## 2. Goals

The implementation must:

* create accounts with an initial balance and currency;
* return the current account balance;
* transfer funds atomically between distinct accounts;
* support transfers between different accounts of the same owner;
* prevent overdrafts for normal and FX transfers;
* reject same-currency transfers between incompatible currencies;
* reverse completed transfers atomically;
* allow the original destination account to become negative during reversal;
* expose account and account-pair history;
* authenticate business operations using JWT;
* provide replay-safe idempotency;
* return structured errors;
* expose an OpenAPI 3.1 specification;
* remain correct with multiple application instances.

## 3. Non-goals

The following are outside the required scope:

* user registration and credential management;
* JWT issuance and refresh-token handling;
* distributed databases;
* distributed transactions across external services;
* shared or jointly owned accounts;
* configurable role-based access control;
* account status transition endpoints;
* real-time settlement with external financial institutions.

## 4. Architecture

```text
Clients
   |
   | HTTPS + JWT
   v
Load Balancer
   |
   +-------------------+
   |                   |
   v                   v
Ledger Instance A   Ledger Instance B
   |                   |
   +---------+---------+
             |
             v
         PostgreSQL
```

Application instances are stateless. Any instance may process any request. PostgreSQL owns balances, transfers, entries, exchange-rate snapshots and idempotency state.

Logical components:

* HTTP API;
* JWT middleware;
* account application service;
* transfer and reversal application services;
* history query service;
* idempotency coordinator;
* SQLx persistence layer;
* configuration and observability.

HTTP handlers perform transport validation and response mapping. Application services own use-case logic and transaction boundaries.

## 5. Authentication and authorization

All business endpoints require:

```http
Authorization: Bearer <JWT>
```

JWT issuance belongs to an external provider. The service validates signature, `exp`, `sub`, configured `iss` and configured `aud`.

The `sub` claim identifies the client.

Authorization rules:

* the creator owns an account;
* only the owner may read its balance or history;
* the caller must own the source account of a normal or FX transfer;
* the destination may belong to the same or another client;
* only the owner of the original destination account may request reversal.

Health and readiness endpoints do not require JWT. Metrics access is deployment-specific.

## 6. Domain model

### Account

An account has:

* a unique ID;
* one owner;
* one immutable currency and scale;
* a current balance;
* a status;
* a monotonic version;
* creation and update timestamps.

A client may own multiple accounts, including several in the same currency. Transfers between distinct accounts of one owner are valid.

The first version supports only `active`. Both participating accounts must exist and be active. Future states such as `blocked`, `suspended` and `closed` may be introduced later.

### Money

Balances and monetary amounts are stored as signed 64-bit integers in minor units.

Examples:

* `10.25 PLN` with scale 2 is stored as `1025`;
* `100 JPY` with scale 0 is stored as `100`.

Binary floating point is not used for balances, amounts, fees or rates.

REST monetary input is a decimal string:

```json
{
  "amount": "10.25"
}
```

Input may not have more fractional digits than the currency scale. Normal monetary input is never silently rounded. Arithmetic overflow is rejected.

Account creation requires `initial_balance >= 0`. Transfer amounts must be positive.

### Transfer

A transfer is the immutable authoritative record of one completed business operation. It stores:

* source and destination account IDs;
* source and destination currencies;
* source and destination amounts;
* fee and total source debit;
* rate and rate reference where applicable;
* initiating client;
* operation kind;
* original transfer reference for reversal;
* creation timestamp.

Failed attempts do not create transfer records.

### Account entry

An account entry is an immutable account-centric history projection. A completed operation creates:

* one debit entry for the source account;
* one credit entry for the destination account.

This does not duplicate rows in an account query because each query is scoped to one `account_id`.

Entries contain only summary data needed by history lists:

* account and transfer IDs;
* counterparty account ID;
* direction;
* amount and currency from the selected account's perspective;
* operation kind;
* optional principal and fee summary for outgoing FX;
* timestamp.

Rate details and reversal relationships remain in `transfers` and are returned by the transfer-details endpoint.

Account creation does not create a transfer or account entry. The account row represents creation and its initial balance.

## 7. Data model

The SQL is illustrative; implementation names may change while preserving the invariants.

### accounts

```sql
CREATE TYPE account_status AS ENUM ('active');

CREATE TABLE accounts (
    id UUID PRIMARY KEY,
    owner_id TEXT NOT NULL,
    currency CHAR(3) NOT NULL,
    currency_scale SMALLINT NOT NULL,
    balance_minor BIGINT NOT NULL,
    status account_status NOT NULL DEFAULT 'active',
    version BIGINT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (currency_scale >= 0)
);
```

The current balance is stored directly in the account row. It is not recalculated from history.

No global `balance_minor >= 0` check is used because reversal may make the original destination negative. Domain logic prevents normal outgoing operations from overdrawing an account.

### transfers

```sql
CREATE TYPE transfer_kind AS ENUM (
    'transfer',
    'fx_transfer',
    'reversal'
);

CREATE TABLE transfers (
    id UUID PRIMARY KEY,
    source_account_id UUID NOT NULL REFERENCES accounts(id),
    destination_account_id UUID NOT NULL REFERENCES accounts(id),
    source_currency CHAR(3) NOT NULL,
    destination_currency CHAR(3) NOT NULL,
    source_amount_minor BIGINT NOT NULL,
    destination_amount_minor BIGINT NOT NULL,
    fee_amount_minor BIGINT NOT NULL DEFAULT 0,
    total_source_debit_minor BIGINT NOT NULL,
    fee_bps INTEGER,
    exchange_rate NUMERIC(30, 12),
    exchange_rate_id UUID,
    kind transfer_kind NOT NULL,
    reverses_transfer_id UUID REFERENCES transfers(id),
    initiated_by TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (source_account_id <> destination_account_id),
    CHECK (source_amount_minor > 0),
    CHECK (destination_amount_minor > 0),
    CHECK (fee_amount_minor >= 0),
    CHECK (total_source_debit_minor > 0)
);

CREATE UNIQUE INDEX transfers_single_reversal
ON transfers(reverses_transfer_id)
WHERE reverses_transfer_id IS NOT NULL;
```

### account_entries

```sql
CREATE TYPE entry_direction AS ENUM ('debit', 'credit');

CREATE TABLE account_entries (
    id UUID PRIMARY KEY,
    account_id UUID NOT NULL REFERENCES accounts(id),
    transfer_id UUID NOT NULL REFERENCES transfers(id),
    counterparty_account_id UUID NOT NULL REFERENCES accounts(id),
    direction entry_direction NOT NULL,
    operation_kind transfer_kind NOT NULL,
    amount_minor BIGINT NOT NULL,
    currency CHAR(3) NOT NULL,
    principal_amount_minor BIGINT,
    fee_amount_minor BIGINT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (amount_minor > 0),
    CHECK (fee_amount_minor IS NULL OR fee_amount_minor >= 0)
);

CREATE INDEX account_entries_history
ON account_entries(account_id, created_at DESC, id DESC);

CREATE INDEX account_entries_pair_history
ON account_entries(account_id, counterparty_account_id, created_at DESC, id DESC);
```

### idempotency_records

```sql
CREATE TYPE idempotency_state AS ENUM (
    'processing',
    'succeeded',
    'business_failed'
);

CREATE TABLE idempotency_records (
    client_id TEXT NOT NULL,
    operation_type TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    request_fingerprint TEXT NOT NULL,
    state idempotency_state NOT NULL,
    claim_token UUID,
    lease_expires_at TIMESTAMPTZ,
    http_status INTEGER,
    response_body JSONB,
    resulting_resource_id UUID,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (client_id, operation_type, idempotency_key)
);
```

`claim_token` identifies the worker currently allowed to finalize a `processing` record. A final row contains the original HTTP status and response body, allowing exact replay.

## 8. Account creation

Account creation validates the allowlisted currency, scale and non-negative initial balance, then inserts one active account. It creates no transfer or account entry.

Where account creation is idempotent, it follows the same protocol described below: claim first, commit the account and successful outcome atomically, or finalize a deterministic business error separately.

## 9. Idempotency protocol

Account creation, transfer, FX transfer and reversal accept an `Idempotency-Key` where applicable.

The key scope is:

```text
(client_id, operation_type, idempotency_key)
```

A normalized fingerprint contains all business-significant client input. For same-currency transfer it includes at least source account, destination account, currency and amount. For FX it does not include the server-selected rate or calculated fee.

### 9.1 Claim transaction

Before the financial transaction, the service opens a short transaction that atomically claims or loads the key.

For a new key it inserts:

* the fingerprint;
* `state = processing`;
* a random `claim_token`;
* a short `lease_expires_at`;
* the configured result-retention `expires_at`.

For an existing key:

* a different fingerprint returns `409 idempotency_conflict`;
* `succeeded` or `business_failed` returns the stored status and body;
* an unexpired `processing` record indicates another worker owns the claim, so the request returns a retryable in-progress response rather than executing concurrently;
* an expired `processing` record may be taken over atomically by replacing `claim_token` and renewing the lease.

The claim transaction commits before financial work starts. Therefore row locks on accounts are never held while waiting for idempotency ownership.

### 9.2 Successful financial operation

The financial operation runs in its own transaction. The current `claim_token` is checked before mutation and again when finalizing the idempotency row.

The transaction atomically commits:

* all balance changes;
* the immutable transfer or created account;
* account entries where applicable;
* `state = succeeded`;
* the response status and body;
* the resulting resource ID;
* removal of the processing lease.

If the transaction commits, both the financial mutation and replayable success exist. If it rolls back, neither exists.

### 9.3 Deterministic business failure

A deterministic business failure such as insufficient funds must not commit financial changes, but it must remain replayable.

The financial transaction is rolled back first. While still logically owning the claim, the service opens a separate short transaction that:

* verifies the same `claim_token` still owns the non-expired processing record;
* changes the state to `business_failed`;
* stores the original business-error HTTP status and response body;
* clears the processing lease;
* commits.

Examples include:

* insufficient funds;
* account not found;
* account not active;
* same-account transfer;
* currency mismatch;
* unauthorized reversal;
* already-reversed transfer.

The separate finalization transaction resolves the apparent contradiction between rolling back financial work and persisting a deterministic error.

### 9.4 Transient and internal failure

Timeouts, unavailable infrastructure and unexpected `5xx` errors are not stored as final outcomes.

The worker either:

* deletes/releases its processing claim in a short transaction when it can do so safely; or
* leaves it to expire when the failure prevents cleanup.

A later request may take over only after `lease_expires_at`. This makes abandoned claims recoverable after process crashes.

A takeover must never overwrite a completed result. Updates use both the key and current `claim_token` in their predicates.

### 9.5 Retention

Final results are retained for a configurable window, default 24 hours. Expired final rows may be deleted asynchronously. A processing lease is much shorter than result retention and is configurable separately.

## 10. Atomic same-currency transfer

After acquiring the idempotency claim, a transfer transaction:

1. verifies ownership of the active claim;
2. locks both account rows using `SELECT ... FOR UPDATE` in deterministic ascending account-ID order;
3. verifies both accounts exist and are active;
4. verifies IDs differ;
5. verifies the caller owns the source;
6. verifies equal currencies;
7. verifies sufficient source balance;
8. debits source and credits destination;
9. increments account versions;
10. inserts the transfer;
11. inserts source debit and destination credit entries;
12. finalizes idempotency as `succeeded`;
13. commits.

If a deterministic validation fails inside this transaction, it rolls back and the error is finalized according to section 9.3.

Both accounts should be selected and locked in one round trip where practical. No network call, event publication or expensive computation occurs while account locks are held.

## 11. FX transfer

The optional FX operation uses a client-supplied source amount. The system calculates destination amount.

```text
fee = round_half_up(source_amount * fee_bps / 10_000)
total_source_debit = source_amount + fee
destination_amount = round_half_up(source_amount * exchange_rate)
```

Destination amount is rounded half-up to destination scale. Fee is denominated in source currency and may be zero. The balance check uses total source debit.

The transfer immutably captures source and destination amounts, currencies, rate, rate reference, fee basis points, fee and total debit.

The rate is selected before account locks where possible and validated against its database validity semantics in the operation. No external provider call occurs while accounts are locked.

An FX operation creates only two entries:

* source entry for total debit, optionally including principal and fee summary;
* destination entry for the amount received.

There is no separate fee entry.

## 12. Reversal

A reversal is a full, one-time inverse of a completed transfer. Partial reversal and reversal of a reversal are unsupported.

Only the owner of the original destination may request it.

After acquiring an idempotency claim, the reversal transaction:

1. verifies ownership of the active claim;
2. locks the original transfer;
3. verifies it exists and is not a reversal;
4. verifies caller authorization;
5. verifies no reversal exists;
6. locks both accounts in deterministic order;
7. verifies both accounts exist and are active;
8. debits the original destination by the exact received amount;
9. credits the original source by the exact original total debit;
10. inserts the reversal transfer;
11. inserts two reversal entries;
12. finalizes idempotency as `succeeded`;
13. commits.

The original destination may become negative regardless of its current balance. Incoming transfers remain allowed; normal outgoing operations require enough balance for the complete debit.

Reversal reuses the exact original amounts, fee and rate snapshot. The unique index on `reverses_transfer_id` is the final database guarantee against concurrent duplicate reversals.

## 13. History queries

### Account history

```sql
SELECT ...
FROM account_entries
WHERE account_id = $1
  AND (created_at, id) < ($2, $3)
ORDER BY created_at DESC, id DESC
LIMIT $4;
```

### Account-pair history

```sql
SELECT ...
FROM account_entries
WHERE account_id = $1
  AND counterparty_account_id = $2
  AND (created_at, id) < ($3, $4)
ORDER BY created_at DESC, id DESC
LIMIT $5;
```

A business operation appears once from the selected account's perspective. History returns summary fields only. Full transfer, FX and reversal details come from `GET /v1/transfers/{transfer_id}`.

History uses keyset pagination:

* order: `created_at DESC, id DESC`;
* cursor: last returned `(created_at, id)`;
* default limit: 50;
* maximum limit: 100.

`OFFSET` is not used.

## 14. Consistency and concurrency

PostgreSQL is authoritative for balances, statuses, completed transfers, entries, reversals, applied FX snapshots and idempotency state.

No cache participates in financial validation or balance mutation.

Operations on independent accounts execute concurrently. Operations on the same accounts serialize through row-level locks.

All multi-account operations lock accounts in deterministic ascending ID order. Transfer reversal also locks the original transfer, and the unique reversal index remains the final duplicate-prevention guarantee.

The idempotency claim transaction is deliberately separate from financial work. Successful idempotency completion is atomic with financial mutation. Deterministic business-error completion is committed only after the financial transaction has rolled back.

## 15. API outline

```text
POST /v1/accounts
GET  /v1/accounts/{account_id}/balance

POST /v1/transfers
POST /v1/fx-transfers
GET  /v1/transfers/{transfer_id}
POST /v1/transfers/{transfer_id}/reversal

GET  /v1/accounts/{account_id}/entries
GET  /v1/accounts/{account_id}/entries?counterparty_account_id=...

GET  /health
GET  /ready
GET  /metrics
```

OpenAPI defines request, response, cursor and error schemas.

## 16. Errors

Errors use a stable structure:

```json
{
  "error": {
    "code": "insufficient_funds",
    "message": "The source account has insufficient funds"
  }
}
```

Expected categories include invalid request, unauthorized, forbidden, account not found, account not active, transfer not found, insufficient funds, currency mismatch, idempotency conflict, operation in progress, already reversed and internal error.

Database details are logged, never returned.

## 17. Observability

Structured logs and traces include endpoint, request ID, client ID where appropriate, operation result, latency and database-error category.

Idempotency telemetry includes claim creation, replay, conflict, in-progress response, lease takeover, business-error finalization and abandoned-claim recovery.

Sensitive values, JWTs and complete financial request bodies are not logged.

Metrics may include request count and latency, transfer outcomes, financial transaction duration, pool utilization, deadlocks, retries and idempotency lease takeovers.

## 18. Testing

Unit tests cover money parsing, scale validation, fingerprints, fee calculation, half-up rounding and cursor encoding.

Integration tests cover:

* account creation and balance reads;
* same-owner transfers;
* inactive and missing accounts;
* successful and rejected transfers;
* FX calculation and rounding;
* reversal, reversal overdraft and duplicate reversal;
* account and pair history;
* successful idempotent replay;
* deterministic-error replay;
* fingerprint conflict;
* concurrent requests with one key;
* processing response while a lease is active;
* takeover of an expired processing lease;
* crash after claim but before financial work;
* rollback before business-error finalization;
* transient failure remaining retryable.

Concurrency tests verify that balances cannot be overdrawn by normal operations, funds are conserved, opposing transfers do not corrupt state, only one reversal succeeds and only the current claim owner may finalize an idempotency record.

Load tests report throughput and p50, p95 and p99 latency for independent and contended accounts.

## 19. Future evolution

Possible measured extensions:

* additional account statuses and transitions;
* read replicas for history;
* time-based partitioning;
* PgBouncer;
* transactional outbox;
* asynchronous read projections;
* externally managed exchange rates;
* finer account permissions.

These are not introduced without a requirement or benchmark justification.

## 20. Key invariants

* account currency and scale are immutable;
* initial balance is non-negative;
* source and destination IDs differ;
* normal and FX debits require sufficient balance;
* only reversal may make the original destination negative;
* one completed operation creates one transfer and exactly two entries;
* one original transfer has at most one reversal;
* reversal of reversal is forbidden;
* all account locks follow deterministic order;
* only the current idempotency claim owner may execute or finalize;
* a successful outcome is atomic with financial mutation;
* a deterministic business error is finalized only after financial rollback;
* transient failures never become replayable final outcomes;
* history uses stable cursor pagination.
