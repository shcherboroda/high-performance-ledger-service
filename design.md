# Ledger Service Design

## 1. Overview

The Ledger Service provides authenticated REST APIs for account creation, balance reads, atomic transfers, transfer reversal and account-centric transaction history.

The design prioritizes:

* strong consistency of balances;
* safe concurrent execution across stateless instances;
* short financial transactions;
* immutable transfer records;
* efficient account-history reads;
* success-only request idempotency;
* optional cross-currency transfers.

PostgreSQL is the authoritative source of financial state and cross-instance coordination.

## 2. Goals

The implementation must:

* create accounts with an initial balance and currency;
* return the current account balance;
* transfer funds atomically between distinct accounts;
* support transfers between different accounts of the same owner;
* prevent overdrafts for normal and FX transfers;
* reject regular transfers between incompatible currencies;
* reverse completed transfers atomically;
* allow the original destination account to become negative during reversal;
* expose account and account-pair history;
* authenticate business operations using JWT;
* prevent duplicate committed side effects for retried successful requests;
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

Application instances are stateless. Any instance may process any request. PostgreSQL owns balances, transfers, entries, exchange-rate snapshots and committed idempotency records.

Logical components:

* HTTP API;
* JWT middleware;
* account application service;
* transfer and reversal application services;
* history query service;
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

Only committed successful side-effecting operations are retained.

```sql
CREATE TABLE idempotency_records (
    client_id TEXT NOT NULL,
    operation_type TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    request_fingerprint TEXT NOT NULL,
    http_status INTEGER NOT NULL,
    response_body JSONB NOT NULL,
    resulting_resource_id UUID NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (client_id, operation_type, idempotency_key)
);
```

There is no `processing` state, lease, claim token, takeover protocol or persisted failure state.

## 8. Success-only idempotency

Account creation, transfer, FX transfer and reversal accept an `Idempotency-Key` where applicable.

The key scope is:

```text
(client_id, operation_type, idempotency_key)
```

A normalized fingerprint contains all business-significant client input. For a same-currency transfer it includes at least source account, destination account, currency and amount. For FX it does not include the server-selected rate or calculated fee.

The idempotency record is handled inside the same PostgreSQL transaction as the side effect.

Protocol:

1. Start the operation transaction.
2. Attempt to insert the idempotency record key and fingerprint under the unique constraint as part of that transaction.
3. If a committed row already exists with the same fingerprint, return its stored successful status and body without repeating the side effect.
4. If a committed row exists with a different fingerprint, return `409 idempotency_conflict`.
5. If another transaction is currently attempting the same key, PostgreSQL unique-key coordination makes the duplicate wait until the first transaction commits or rolls back.
6. Execute business validation and the financial mutation.
7. Store the successful response in the idempotency row.
8. Commit balances, transfer or account, entries and the completed idempotency record atomically.

If business validation fails, or an infrastructure/internal error aborts the operation, the whole transaction rolls back, including the idempotency reservation. No failure response is retained. A later retry with the same key re-evaluates current state.

Consequences:

* duplicate successful requests produce one committed side effect;
* a retry after `insufficient_funds`, inactive account or unavailable rate may succeed after state changes;
* no abandoned in-progress record can block future work;
* `409 idempotency_conflict` applies only when a committed successful record exists for the same scoped key with a different fingerprint;
* successful records are retained for a configurable window, default 24 hours, and may be deleted asynchronously after expiry.

## 9. Account creation

Account creation runs in one transaction:

1. parse and validate the decimal-string initial balance;
2. validate the currency and scale against the allowlist;
3. reserve the idempotency key where required;
4. insert one active account;
5. store the successful response in the idempotency row;
6. commit atomically.

A negative initial balance is rejected and rolls back the reservation. No transfer or account entry is created.

## 10. Atomic same-currency transfer

A normal transfer runs in one PostgreSQL transaction:

1. parse and validate the positive decimal-string amount;
2. reserve or replay the idempotency key;
3. lock both account rows using `SELECT ... FOR UPDATE` in deterministic ascending account-ID order;
4. verify both accounts exist and are active;
5. verify source and destination differ;
6. verify the caller owns the source account;
7. verify both accounts use the same currency;
8. verify sufficient source balance;
9. debit the source and credit the destination;
10. increment account versions;
11. insert the immutable transfer;
12. insert one debit and one credit account entry;
13. store the successful response in the idempotency row;
14. commit atomically.

Any business error rolls back all work, including the idempotency reservation.

The transaction contains no remote network calls, event publishing or expensive computation.

## 11. FX transfer

The optional FX operation uses a client-supplied source amount. The system calculates the destination amount.

```text
fee = round_half_up(source_amount * fee_bps / 10_000)
total_source_debit = source_amount + fee
destination_amount = round_half_up(source_amount * exchange_rate)
```

The destination amount is rounded half-up to the destination currency scale. The fee is calculated in source currency and may be zero. The balance check uses `total_source_debit`.

The applied rate, rate record, fee basis points, fee amount, source amount, destination amount and total debit are captured immutably in the transfer.

The rate is read and validated before account locks where practical and must still be valid for the operation. No external rate-provider call occurs while account rows are locked.

One FX transfer creates only two account entries:

* source entry: total debit, with optional principal and fee summary;
* destination entry: received destination amount.

A failed FX operation rolls back its idempotency reservation and may be retried against a newer rate or changed account state.

## 12. Transfer reversal

A reversal is a full, one-time financial inverse of a completed transfer. Partial reversals and reversal of a reversal are unsupported.

Only the owner of the original destination account may request reversal.

The reversal runs in one PostgreSQL transaction:

1. reserve or replay the idempotency key;
2. lock the original transfer;
3. verify it exists and is not itself a reversal;
4. verify the caller owns the original destination account;
5. verify no reversal already exists;
6. lock both accounts in deterministic ascending account-ID order;
7. verify both accounts exist and are active;
8. debit the original destination by the exact amount received;
9. credit the original source by the exact original total debit;
10. insert the immutable reversal transfer;
11. insert two reversal account entries;
12. store the successful response in the idempotency row;
13. commit atomically.

The original destination may become negative regardless of its current balance. Incoming transfers remain allowed and can restore it. Ordinary outgoing and FX transfers still require enough balance for their complete debit.

A reversal uses the original amounts, fee and rate snapshot. It never uses a current rate or recalculates the fee.

The partial unique index on `reverses_transfer_id` is the final database guarantee against concurrent duplicate reversals.

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

A business operation appears once from the selected account's perspective. History returns summary data; full FX and reversal details come from `GET /v1/transfers/{transfer_id}`.

### Cursor pagination

History uses keyset pagination rather than `OFFSET`:

* order: `created_at DESC, id DESC`;
* cursor: last returned `(created_at, id)`;
* default limit: 50;
* maximum limit: 100.

Using both fields keeps ordering deterministic when timestamps match. Newer inserts do not shift subsequent pages.

## 14. Consistency and concurrency

PostgreSQL is authoritative for:

* account balances and statuses;
* completed transfers and reversals;
* account entries;
* exchange-rate snapshots used by completed FX transfers;
* completed successful idempotency records.

No cache participates in validation or balance mutation.

Operations on independent accounts may execute concurrently. Operations touching the same account serialize through row locks.

All multi-account operations lock account rows in deterministic ascending account-ID order.

For each successful side-effecting request, the financial mutation, transfer/account record, account entries and idempotency result commit in one transaction. On any failure, all of them roll back.

Concurrent duplicate requests coordinate through the unique idempotency key. After the winning transaction commits, duplicates replay the stored success; after rollback, a duplicate may proceed and re-evaluate current state.

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

The OpenAPI 3.1 specification defines exact request, response and error schemas.

## 16. Error handling

Errors use a stable shape:

```json
{
  "error": {
    "code": "insufficient_funds",
    "message": "The source account has insufficient funds"
  }
}
```

Expected categories include:

* invalid request;
* unauthorized;
* forbidden;
* account not found;
* account not active;
* transfer not found;
* insufficient funds;
* currency mismatch;
* same-account transfer;
* missing or stale FX rate;
* idempotency conflict;
* transfer already reversed;
* internal error.

Business and technical failures are not persisted as idempotency outcomes. Internal database details are logged but never returned.

## 17. Observability

Structured logs and traces include:

* endpoint and request ID;
* authenticated client ID where appropriate;
* operation result and latency;
* transfer or resulting resource ID on success;
* idempotency replay or conflict;
* database error category;
* deadlock and retry information.

Sensitive values, JWTs and full financial request bodies are not logged.

Metrics may include:

* request count and latency;
* transfer success and rejection counts;
* idempotent success replay and conflict counts;
* transaction duration;
* database-pool utilization;
* deadlock and retry counts.

## 18. Testing strategy

Unit tests cover:

* decimal parsing and currency scale;
* half-up rounding;
* fee and FX calculations;
* fingerprint normalization;
* domain validation.

Integration tests against PostgreSQL cover:

* account creation and zero initial balance;
* rejection of negative initial balance;
* active-account validation;
* successful same-owner and different-owner transfers;
* insufficient funds;
* currency mismatch and same-account rejection;
* successful FX transfer and fee calculation;
* missing or stale rate;
* successful reversal and reversal overdraft;
* unauthorized, duplicate and reversal-of-reversal rejection;
* account and account-pair history;
* cursor pagination;
* successful idempotent replay;
* same key with different fingerprint after committed success;
* retry with the same key after a business failure;
* rollback of both financial work and idempotency reservation on failure.

Concurrency tests verify:

* concurrent transfers cannot create a normal overdraft;
* total funds are conserved;
* opposing transfers do not corrupt balances;
* concurrent reversal attempts create one reversal;
* concurrent identical requests create one committed side effect;
* a duplicate waits for the winning transaction and replays its committed success;
* a duplicate can proceed after the winning transaction rolls back.

Load tests report throughput and p50, p95 and p99 latency for independent accounts and intentionally contended hot accounts.

## 19. Performance strategy

The critical path uses:

* stateless Tokio/Axum instances;
* SQLx connection pooling;
* short PostgreSQL transactions;
* deterministic row locking;
* minimal round trips;
* prepared statements;
* minimal write indexes;
* cursor-based history pagination;
* no remote calls while account rows are locked;
* no authoritative distributed cache.

The idempotency unique key adds one indexed write to successful side-effecting operations and coordinates duplicate requests without a separate work-claim subsystem.

Pool sizes are configurable and must account for total connections across all instances. PgBouncer may be introduced if measurements justify it.

## 20. Scalability and future evolution

The service scales horizontally by adding stateless instances against PostgreSQL.

Possible future improvements:

* additional account states and status-transition APIs;
* read replicas for non-authoritative history;
* time-based partitioning;
* PgBouncer;
* transactional outbox;
* asynchronous read projections;
* externally supplied exchange rates;
* asymmetric JWT verification;
* fine-grained account permissions.

These are introduced only when requirements or measurements justify their complexity.

## 21. Key invariants

* source and destination account IDs differ;
* both participating accounts exist and are active;
* account currency and scale are immutable;
* initial balance is non-negative;
* normal and FX transfers never overdraw the source account;
* reversal may make the original destination negative;
* transfer amounts are positive;
* one completed business operation creates exactly two account entries;
* at most one reversal exists for an original transfer;
* reversal of a reversal is forbidden;
* all participating accounts are locked in deterministic order;
* successful mutation and successful idempotency record commit atomically;
* failed operations leave no committed idempotency record;
* one scoped idempotency key maps to one fingerprint only after a successful commit;
* history ordering is stable by `(created_at, id)`.

## 22. Key trade-offs

### Success-only idempotency

Persisting only successful outcomes keeps the guarantee that duplicate successful requests create one side effect while allowing retries after state-dependent business failures. It avoids processing states, leases, takeover rules and failure-result retention.

### PostgreSQL instead of an authoritative cache

This favors correctness and operational simplicity and avoids dual-write consistency problems.

### Row locking instead of optimistic retries

Row locking gives predictable correctness under contention. Optimistic concurrency may reduce lock waits at low contention but can create repeated retries for hot accounts.

### Current balance plus immutable transfers and entries

The account row provides constant-size balance reads. Transfers provide the authoritative business audit record. Entries provide efficient account-centric history. All are updated atomically.

### Bonus-ready schema without bonus-first implementation

The schema preserves both monetary sides, rate and fee data needed for FX while keeping the mandatory same-currency path small and testable.
