use std::time::Duration;

use axum::{
    Router,
    routing::{get, post},
};
use sqlx::PgPool;

use crate::auth::AuthVerifier;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub auth: AuthVerifier,
    pub idempotency_retention: Duration,
}

pub fn router(pool: PgPool, auth: AuthVerifier) -> Router {
    router_with_idempotency_retention(pool, auth, Duration::from_secs(24 * 60 * 60))
}

pub fn router_with_idempotency_retention(
    pool: PgPool,
    auth: AuthVerifier,
    idempotency_retention: Duration,
) -> Router {
    let router = Router::new()
        .route("/health", get(crate::api::health::health))
        .route("/ready", get(crate::api::health::ready))
        .route("/openapi.json", get(crate::openapi::openapi))
        .route("/accounts", post(crate::api::accounts::create_account))
        .route(
            "/accounts/{account_id}/balance",
            get(crate::api::accounts::get_balance),
        )
        .route("/transfers", post(crate::api::transfers::create_transfer))
        .route(
            "/transfers/{transfer_id}/reversal",
            post(crate::api::reversals::reverse_transfer),
        );

    #[cfg(test)]
    let router = router.route("/_test/authenticated", get(test_authenticated));
    router.with_state(AppState {
        pool,
        auth,
        idempotency_retention,
    })
}

#[cfg(test)]
async fn test_authenticated(
    crate::auth::AuthenticatedClient { client_id }: crate::auth::AuthenticatedClient,
) -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({ "client_id": client_id }))
}

#[cfg(test)]
mod tests;
