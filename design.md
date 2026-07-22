# Ledger Service Design

## 1. Overview

The Ledger Service provides authenticated REST APIs for account creation, balance reads, atomic transfers, transfer reversal and account-centric transaction history.

The design prioritizes:

* strong consistency of balances;
* safe concurrent execution across stateless instances;
* short PostgreSQL transactions;
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

Accounts are created as `active` and may be set to `inactive`. Both participating accounts must exist and be active for a normal transfer. Future states such as `blocked`, `suspended` and `closed` may be introduced later.

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

Entries contain summary data needed by history lists:

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

`destination_amount_minor > 0` validates the amount credited by the operation, not the destination account balance.

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
    http_status INTEGER,
    response_body JSONB,
    resulting_resource_id UUID,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (client_id, operation_type, idempotency_key),
    CHECK (
        (http_status IS NULL
            AND response_body IS NULL
            AND resulting_resource_id IS NULL)
        OR
        (http_status IS NOT NULL
            AND response_body IS NOT NULL
            AND resulting_resource_id IS NOT NULL)
    )
);
```

The nullable result fields allow a reservation row to exist inside the owning uncommitted transaction before the business result is known. Application logic guarantees that an incomplete reservation is never committed: it is either completed and committed with the side effect, or rolled back with the operation.

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
2. Reserve the scoped key using `INSERT ... ON CONFLICT DO NOTHING`, inserting the key, fingerprint and null result fields.
3. If the insert succeeds, this transaction owns the reservation and may execute the operation.
4. If the insert affects no row, load the now-committed conflicting record.
5. PostgreSQL waits on an uncommitted conflicting unique key before deciding the `ON CONFLICT` outcome, so a concurrent duplicate cannot pass the reservation step while the first transaction is unresolved.
6. If the committed record has the same fingerprint, return its stored successful status and body without repeating the side effect.
7. If the committed record has a different fingerprint, return `409 idempotency_conflict`.
8. For the owning transaction, execute business validation and the account or financial mutation.
9. Populate `http_status`, `response_body` and `resulting_resource_id` in the reserved row.
10. Commit balances, transfer or account, entries and the completed idempotency record atomically.

Illustrative reservation statement:

```sql
INSERT INTO idempotency_records (
    client_id,
    operation_type,
    idempotency_key,
    request_fingerprint,
    expires_at
)
VALUES ($1, $2, $3, $4, $5)
ON CONFLICT DO NOTHING;
```

A plain insert that raises a unique-constraint violation is not used because that error would abort the PostgreSQL transaction and prevent replay logic from continuing in the same transaction.

If business validation fails, or an infrastructure/internal error aborts the operation, the whole transaction rolls back, including the incomplete reservation. No failure response is retained. A later retry with the same key re-evaluates current state.

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
3. reserve or replay the idempotency key where required;
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

### FX configuration foundation

Exchange rates and fee rules are written by an external configuration component, not by this
service. The Ledger Service reads those PostgreSQL records directly and exposes no configuration
administration or provider API. Rates are strictly directional: a EUR-to-PLN record neither
authorizes PLN-to-EUR nor permits inversion or triangulation.

Published exchange-rate and fee-rule rows are immutable. Configuration changes create new rows
and validity intervals; published rate values, currency pairs, fee values, and intervals are not
modified. A completed transfer retains its exact rate and fee snapshots, and its restrictive rate
foreign key prevents deletion of the referenced audit record.

Both rates and fee rules use half-open validity intervals (`valid_from <= operation_time <
valid_until`). A future financial transaction supplies one operation timestamp (preferably
PostgreSQL transaction time) to all lookups. Missing configuration and multiple applicable rows
fail closed; an overlapping row is never resolved by choosing a newest, oldest, or otherwise
preferred record. Pair-specific fee rules take precedence over defaults, while ambiguity is
evaluated only within the selected specificity level.

Rates are stored as `NUMERIC(30,12)` and parsed as an exact decimal coefficient and scale.
Conversion uses arbitrary-precision integer intermediates because schema-valid source amounts,
rate coefficients, and scale factors can exceed `i128`; only final ledger values are checked
`i64`. The destination amount is rounded half-up once, at the final destination minor-unit
division. Fees are calculated from source principal with half-up integer division, must be
between 0 and 10,000 basis points inclusive, and cannot exceed 100% of principal. The resulting
fee and total source debit are checked `i64` values. A transfer's rate reference is restrictive,
so a rate used by a completed transfer remains available for audit even after it expires.

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

* source entry: total debit, with principal and fee summary;
* destination entry: received destination amount.

There is no separate fee entry.

## 12. Reversal

A reversal is a full one-time financial inverse of a completed transfer.

Rules:

* only the owner of the original destination account may request it;
* partial reversal is not supported;
* reversal of a reversal is not supported;
* only one reversal may reference an original transfer;
* original stored amounts, fee and rate snapshot are reused;
* current exchange rates and fees are not recalculated.

The reversal transaction:

1. reserve or replay the idempotency key;
2. lock the original transfer;
3. verify it exists and is not itself a reversal;
4. verify caller authorization;
5. verify no reversal already exists;
6. lock both accounts in deterministic ID order;
7. verify both accounts exist and are active;
8. debit the original destination by the amount it received;
9. credit the original source by the original total debit;
10. insert the reversal transfer;
11. insert two reversal account entries;
12. store the successful response;
13. commit atomically.

The original destination may become negative during reversal. Incoming transfers remain allowed; ordinary outgoing transfers remain forbidden unless the current balance covers the full debit.

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

History is account-relative, so one business operation appears once from the selected account's perspective.

Full transfer, FX and reversal details are returned by `GET /v1/transfers/{transfer_id}`.

### Cursor pagination

History uses keyset pagination:

* order: `created_at DESC, id DESC`;
* cursor: the last returned `(created_at, id)` pair;
* default limit: 50;
* maximum limit: 100.

`OFFSET` pagination is not used.

## 14. Consistency and concurrency

PostgreSQL is authoritative for:

* account balances and statuses;
* transfers and reversals;
* account entries;
* FX snapshots;
* committed successful idempotency results.

No cache participates in transfer validation or balance mutation.

Independent accounts may be processed concurrently. Operations touching the same account are serialized with row locks.

All multi-account operations lock accounts in deterministic ascending ID order.

The side effect, audit records, account entries and successful idempotency result commit atomically.

A business or technical failure commits none of them.

## 15. API outline

```text
POST /accounts
GET  /accounts/{account_id}/balance

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

The complete request, response and error schemas belong in OpenAPI 3.1.

## 16. Error handling

Errors use a stable structure:

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
* unavailable FX rate;
* idempotency conflict;
* transfer already reversed;
* internal error.

Internal database details are logged but not returned.

## 17. Observability

Runtime logs are JSON structured and retain `RUST_LOG` filtering. Output uses a bounded,
lossy non-blocking queue; its worker guard remains alive through normal shutdown so queued
events can flush. Each HTTP request has one `x-request-id`: one valid caller value is kept,
otherwise a UUID v4 is generated and returned on every response. Request spans and completion
events record only the request ID, method, matched route template (or `unmatched`), status, and
latency. They never record headers, query strings, raw URIs, JWTs, financial identifiers, or
request/response bodies.

`GET /metrics` exposes process-local Prometheus text without business authentication. It reports
`ledger_http_requests_total{method,route,status_class}` and
`ledger_http_request_duration_seconds{method,route}` using bounded route templates and explicit
histogram buckets. It performs no network export; deployments must protect access to this endpoint
at their network boundary when appropriate.

## 18. Testing strategy

Unit tests cover:

* decimal-string parsing;
* currency scale validation;
* fingerprint normalization;
* FX rounding and fee calculation;
* domain validation.

Integration tests against PostgreSQL cover:

* account creation and balance reads;
* normal and self-owned-account transfers;
* insufficient funds;
* inactive and missing accounts;
* currency mismatch;
* same-account rejection;
* successful FX transfer;
* unavailable or stale FX rate;
* successful reversal;
* reversal overdraft;
* unauthorized and duplicate reversal;
* history and cursor pagination;
* successful idempotent replay;
* same key with different fingerprint;
* concurrent identical requests committing one side effect;
* business failure rolling back the reservation;
* retry after a state-changing business failure;
* uncommitted reservation visibility and rollback behavior.

Concurrency tests verify:

* no overdraft from concurrent transfers;
* conservation of funds;
* safe opposing transfers;
* one committed reversal under concurrent attempts;
* one committed side effect for concurrent duplicate successful requests.

## 19. Key invariants

* account currency and scale are immutable;
* source and destination IDs differ;
* transfer amounts are positive;
* fees are non-negative;
* ordinary outgoing operations require enough balance for the full debit;
* only reversal may create a negative balance;
* one reversal exists per original transfer;
* each completed operation creates exactly two account entries;
* incomplete idempotency reservations never commit;
* each committed successful side effect has one completed idempotency record;
* the idempotency key scope is unique;
* failures leave no committed idempotency record.

## 20. Scalability and future evolution

The first version scales through stateless application instances and one PostgreSQL database.

Potential later improvements:

* PgBouncer;
* history read replicas;
* transfer-table partitioning;
* transactional outbox;
* asynchronous read projections;
* externally managed FX rates;
* additional account statuses and lifecycle APIs;
* fine-grained permissions.

These are introduced only when requirements or measurements justify their complexity.

## 21. Trade-offs

### Current balance plus immutable history

Balance reads are constant-size, while immutable transfers and entries provide auditability. All projections are updated atomically.

### Row locking

Row locks give predictable correctness under contention. Deterministic lock order reduces deadlock risk.

### Success-only idempotency

Only committed successful outcomes are retained. The reservation and business mutation share one transaction, so failures naturally remove the reservation and retries can re-evaluate changed state.

Nullable result fields are an internal transactional mechanism, not a persisted incomplete state. The application must complete them before commit.

This avoids leases, claim tokens, failure replay and abandoned-work recovery while preserving the essential guarantee of one committed side effect for duplicate successful requests.

### Account-entry projection

The extra two rows per completed operation increase write volume but make account and account-pair history direct indexed reads without reconstructing direction from transfer columns.
