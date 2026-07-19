# Ledger Service Design

## 1. Overview

The Ledger Service provides authenticated REST APIs for account management, atomic money transfers, transfer reversal and transaction history.

The service is designed for:

* strong consistency of balances;
* safe concurrent execution;
* low latency on the critical transfer path;
* horizontal scaling of stateless application instances;
* an immutable transfer audit trail;
* future cross-currency support.

PostgreSQL is the authoritative source of financial state and provides transaction isolation, row-level locking and cross-instance concurrency control.

## 2. Goals

The implementation must:

* create accounts with an initial balance and currency;
* return the current account balance;
* transfer funds atomically between distinct accounts;
* prevent overdrafts for normal transfers;
* reject normal transfers between incompatible currencies;
* reverse completed transfers atomically;
* allow an overdraft on the original destination during reversal;
* expose account and account-pair transfer history;
* authenticate all business operations using JWT;
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
* real-time settlement with external financial institutions.

## 4. System architecture

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

Ledger application instances are stateless. Any instance may process a request for any client.

No application instance owns an account, client or balance. All authoritative state is stored in PostgreSQL.

The application consists of the following logical components:

* HTTP API;
* JWT authentication middleware;
* account application service;
* transfer application service;
* history query service;
* idempotency handling;
* SQLx persistence layer;
* configuration and observability.

HTTP handlers perform transport-level validation and response mapping. Application services own use-case logic and transaction boundaries. SQLx persistence functions execute database operations.

## 5. Authentication and authorization

All business endpoints require an access token in the HTTP header:

```http
Authorization: Bearer <JWT>
```

JWT issuance is delegated to an external authentication provider.

The Ledger Service validates:

* token signature;
* expiration time (`exp`);
* subject (`sub`);
* configured issuer (`iss`);
* configured audience (`aud`).

The `sub` claim is used as the authenticated client identifier.

JWT verification occurs once at the beginning of every protected HTTP request. Internal services receive an already authenticated client context and do not parse the token again.

Authentication proves the client identity. Authorization is checked separately for each operation.

Authorization rules:

* an account is owned by the authenticated client that created it;
* only the owner may read an account balance or history;
* the authenticated client must own the source account of a transfer;
* the destination account may belong to another client;
* a reversal may be requested only by the client that initiated the original transfer.

Health and readiness endpoints do not require JWT. Metrics access is deployment-specific and should normally be restricted at the infrastructure level.

## 6. Domain model

### Client

A client is identified by the JWT `sub` claim. No local client table is required for the assessment because the service does not manage client profiles or credentials.

### Account

An account has:

* a unique account ID;
* one owner;
* one currency;
* a current balance;
* a monotonic version;
* creation and update timestamps.

A client may own multiple accounts, including multiple accounts in the same currency.

### Money

Balances and transfer amounts are stored as signed 64-bit integers in the smallest supported currency unit.

Examples:

* `10.25 PLN` is stored as `1025`;
* `100 JPY` is stored as `100`.

Binary floating-point types are not used for balances or transfer amounts.

Normal account creation requires an initial balance greater than or equal to zero. Normal transfer amounts must be greater than zero.

### Currency

Currency codes are normalized uppercase three-letter ASCII codes.

The mandatory implementation supports same-currency transfers. The data model remains compatible with a cross-currency extension.

### Transfer

A transfer is an immutable record of a completed balance movement.

A transfer stores:

* source and destination accounts;
* source and destination currencies;
* source and destination amounts;
* conversion fee;
* exchange rate, when applicable;
* initiating client;
* transfer type;
* reference to the original transfer for reversals;
* creation timestamp.

Failed transfer attempts do not create completed transfer records.

## 7. Data model

### accounts

```sql
CREATE TABLE accounts (
    id UUID PRIMARY KEY,
    owner_id TEXT NOT NULL,
    currency CHAR(3) NOT NULL,
    balance_minor BIGINT NOT NULL,
    version BIGINT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

The current balance is stored directly in the account row to provide constant-size balance reads. It is not recalculated from the full transfer history.

The transfer table remains the immutable audit trail, while the account row is the transactional current-state projection. Both are updated in the same database transaction.

### transfers

```sql
CREATE TYPE transfer_kind AS ENUM (
    'transfer',
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
    fee_currency CHAR(3),
    exchange_rate NUMERIC(30, 12),

    kind transfer_kind NOT NULL,
    reverses_transfer_id UUID REFERENCES transfers(id),

    initiated_by TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

A partial unique index prevents multiple reversals of one transfer:

```sql
CREATE UNIQUE INDEX transfers_single_reversal
ON transfers(reverses_transfer_id)
WHERE reverses_transfer_id IS NOT NULL;
```

History indexes are added for source account, destination account and chronological cursor pagination. Indexes are kept minimal because every additional index increases write cost.

## 8. Atomic transfer algorithm

A normal transfer is executed in one PostgreSQL transaction.

1. Validate the request format and positive amount.
2. Start a database transaction.
3. Lock both account rows using `SELECT ... FOR UPDATE`.
4. Acquire locks in deterministic account-ID order.
5. Verify that both accounts exist.
6. Verify that source and destination are distinct.
7. Verify that the authenticated client owns the source account.
8. Verify currency compatibility.
9. Verify sufficient source balance.
10. Update both balances.
11. Increment both account versions.
12. Insert the immutable transfer record.
13. Complete the idempotency record.
14. Commit.

Both account rows are selected and locked in one database round trip where practical.

Deterministic lock ordering reduces the probability of deadlocks for concurrent transfers such as `A → B` and `B → A`.

The transaction contains no external network calls, event publishing or expensive computation.

## 9. Transfer reversal

A reversal is a separate operation with different overdraft rules.

The reversal transaction:

1. locks the original transfer;
2. verifies that it exists and is reversible;
3. verifies authorization;
4. verifies that no reversal already exists;
5. locks both affected accounts in deterministic order;
6. debits the original destination;
7. credits the original source;
8. inserts an immutable reversal record;
9. commits.

The original destination may become negative during reversal, as explicitly required.

A reversal uses the exact amounts stored in the original transfer. It does not use current balances, current conversion rates or recalculated fees.

## 10. Idempotency

Transfer and reversal endpoints accept an `Idempotency-Key` header.

The key is scoped by the authenticated client:

```text
(client_id, idempotency_key)
```

The service stores:

* client ID;
* idempotency key;
* normalized request fingerprint;
* operation type;
* resulting transfer ID.

The idempotency record is created and completed in the same PostgreSQL transaction as the balance mutation.

Behaviour:

* a new key executes the operation;
* the same key with the same request returns the original result;
* the same key with a different request returns `409 Conflict`;
* concurrent requests with the same key result in one balance mutation.

A PostgreSQL unique constraint provides the final guarantee across multiple application instances.

## 11. Cross-currency extension

The mandatory implementation rejects transfers between different currencies.

The schema supports a later cross-currency implementation without changing transfer identity or account ownership.

A cross-currency transfer records an immutable snapshot of:

* source amount;
* source currency;
* destination amount;
* destination currency;
* applied decimal exchange rate;
* conversion fee;
* fee currency;
* total source debit.

Exchange rates use decimal arithmetic and are never represented using binary floating-point values.

Rates may be stored in PostgreSQL for the assessment. No external rate-provider request is performed while account rows are locked.

The fee is configurable, for example as basis points of the source amount.

A cross-currency reversal uses the original stored values:

* debit the original destination by the exact received amount;
* credit the original source by the exact original total debit;
* do not apply the current exchange rate.

This design treats reversal as the exact financial inverse of the original operation.

## 12. Consistency and concurrency

PostgreSQL is the only authoritative source for:

* account balances;
* completed transfers;
* reversals;
* idempotency state.

No cache participates in transfer validation or balance mutation.

This prevents inconsistencies caused by a database commit succeeding while a cache update fails, or a cache update succeeding while the database transaction rolls back.

Reads of a single account row by primary key are expected to be inexpensive and commonly served from PostgreSQL shared buffers.

Operations on independent accounts may execute concurrently. Operations contending on the same account are intentionally serialized by row-level locking to preserve strict balance correctness.

## 13. Performance strategy

The initial performance strategy is based on reducing work in the critical transaction path:

* stateless Tokio/Axum instances;
* SQLx connection pooling;
* short database transactions;
* deterministic row locking;
* minimal database round trips;
* primary-key balance reads;
* cursor-based history pagination;
* prepared statement reuse;
* minimal indexing;
* no remote calls inside transfer transactions;
* no authoritative distributed cache.

Connection-pool size is configurable. Horizontal scaling must account for the total number of PostgreSQL connections across all instances.

PgBouncer may be introduced in a larger deployment if connection scaling becomes a bottleneck.

Performance changes should be benchmark-driven rather than based on speculative caching.

## 14. API outline

```text
POST /v1/accounts
GET  /v1/accounts/{account_id}/balance

POST /v1/transfers
POST /v1/transfers/{transfer_id}/reversal

GET  /v1/accounts/{account_id}/transfers
GET  /v1/transfers?account_id_1=...&account_id_2=...

GET  /health
GET  /ready
GET  /metrics
```

Transfer-history endpoints use cursor pagination with a stable ordering based on `(created_at, id)`.

The complete request, response and error schemas are defined in the OpenAPI 3.1 specification.

## 15. Error handling

API errors use a stable structured representation:

```json
{
  "error": {
    "code": "insufficient_funds",
    "message": "The source account has insufficient funds"
  }
}
```

Expected error categories include:

* invalid request;
* unauthorized;
* forbidden;
* account not found;
* transfer not found;
* insufficient funds;
* currency mismatch;
* idempotency key reused;
* transfer already reversed;
* internal error.

Internal database details are logged but never returned to API clients.

## 16. Observability

The service uses structured tracing with request correlation IDs.

Logs and traces include:

* endpoint;
* request ID;
* authenticated client ID where appropriate;
* operation result;
* latency;
* database error category.

Sensitive values, JWTs and full financial request bodies are not logged.

Metrics may include:

* request count;
* request latency;
* transfer success and rejection counts;
* transaction duration;
* database pool utilization;
* deadlock and retry counts.

## 17. Testing strategy

Unit tests cover deterministic domain validation and value parsing.

Integration tests run against a real PostgreSQL instance and cover:

* account creation;
* balance reads;
* successful transfers;
* insufficient funds;
* currency mismatch;
* same-account rejection;
* authorization;
* successful reversal;
* reversal overdraft;
* duplicate reversal;
* idempotent replay;
* idempotency-key conflict;
* account history;
* account-pair history.

Concurrency tests verify:

* concurrent transfers cannot create an overdraft;
* total funds are conserved;
* opposing transfers do not corrupt balances;
* concurrent reversal attempts create only one reversal;
* concurrent requests with one idempotency key execute once.

Load tests report throughput and p50, p95 and p99 latency for representative account distributions, including independent accounts and intentionally contended hot accounts.

## 18. Scalability and future evolution

The service scales horizontally by adding stateless application instances against one PostgreSQL database.

The initial design can evolve through:

* read replicas for non-authoritative history queries;
* time-based transfer-table partitioning;
* PgBouncer;
* transactional outbox for event publishing;
* asynchronous read projections;
* externally managed exchange rates;
* asymmetric JWT verification;
* fine-grained account permissions.

These mechanisms are not introduced until measurements or requirements justify their operational complexity.

## 19. Key trade-offs

### PostgreSQL instead of authoritative cache

This favors correctness and operational simplicity. It avoids dual-write consistency problems while still providing fast indexed balance access.

### Row locking instead of optimistic retries

Row locking gives predictable correctness under contention. Optimistic concurrency may perform better under very low contention but can create repeated retries for hot accounts.

### Current balance plus immutable transfers

Storing the current balance avoids recalculating it from the complete journal for every request. Atomic updates keep the current state and audit trail consistent.

### Stateless application instances

Stateless instances simplify horizontal scaling and failure recovery. PostgreSQL remains the shared coordination point and eventual throughput boundary.

### Bonus-ready schema without bonus-first implementation

The schema preserves source and destination monetary values required for future currency conversion, while the mandatory same-currency path remains small and testable.
