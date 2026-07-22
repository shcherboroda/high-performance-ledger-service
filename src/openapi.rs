use std::sync::LazyLock;

use axum::Json;

use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};

use utoipa::{Modify, OpenApi};

use crate::api_error::{ApiErrorBody, ErrorEnvelope};

pub const SERVICE_TITLE: &str = "FJX High-Performance Ledger Service";

pub const SERVICE_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(OpenApi)]
#[openapi(
    info(title = SERVICE_TITLE, version = SERVICE_VERSION),
    paths(crate::api::health::health, crate::api::health::ready, crate::api::accounts::create_account, crate::api::accounts::get_balance, crate::api::history::get_account_history, crate::api::transfers::create_transfer, crate::api::transfers::get_transfer, crate::api::reversals::reverse_transfer),
    components(schemas(crate::api::health::StatusResponse, crate::api::accounts::CreateAccountRequest, crate::api::accounts::AccountCreatedResponse, crate::api::accounts::AccountBalanceResponse, crate::api::history::AccountEntryResponse, crate::api::history::AccountHistoryResponse, crate::api::transfers::CreateTransferRequest, crate::api::transfers::TransferCreatedResponse, crate::api::transfers::TransferDetailsResponse, crate::api::reversals::ReversalCreatedResponse, ErrorEnvelope, ApiErrorBody)),
    modifiers(&SecuritySchemeAddon)
)]
struct ApiDoc;

struct SecuritySchemeAddon;

impl Modify for SecuritySchemeAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        openapi
            .components
            .as_mut()
            .expect("OpenAPI components are generated from schemas")
            .add_security_scheme(
                "bearerAuth",
                SecurityScheme::Http(
                    HttpBuilder::new()
                        .scheme(HttpAuthScheme::Bearer)
                        .bearer_format("JWT")
                        .build(),
                ),
            );
    }
}

static OPENAPI: LazyLock<utoipa::openapi::OpenApi> = LazyLock::new(ApiDoc::openapi);

pub(crate) async fn openapi() -> Json<utoipa::openapi::OpenApi> {
    Json(OPENAPI.clone())
}
