use anyhow::Error;
use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use serde_json::Value;
use tracing::error;
use utoipa::ToSchema;

/// The stable JSON envelope returned when an API operation cannot succeed.
#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorEnvelope {
    pub error: ApiErrorBody,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ApiErrorBody {
    /// A stable, machine-readable error identifier.
    pub code: &'static str,
    /// A safe description intended for API clients.
    pub message: &'static str,
    /// Optional structured context, for example future validation errors.
    pub details: Option<Value>,
    /// Reserved for request correlation once request IDs are introduced.
    pub request_id: Option<String>,
}

impl ErrorEnvelope {
    fn new(code: &'static str, message: &'static str, details: Option<Value>) -> Self {
        Self {
            error: ApiErrorBody {
                code,
                message,
                details,
                request_id: None,
            },
        }
    }
}

/// Transport-level failures shared by HTTP handlers.
#[derive(Debug)]
pub enum AppError {
    BadRequest { details: Option<Value> },
    Unauthorized,
    Forbidden,
    NotFound,
    Conflict,
    ServiceUnavailable,
    Internal { source: Error },
}

impl AppError {
    pub fn bad_request(details: Option<Value>) -> Self {
        Self::BadRequest { details }
    }

    pub fn unauthorized() -> Self {
        Self::Unauthorized
    }
    pub fn forbidden() -> Self {
        Self::Forbidden
    }
    pub fn not_found() -> Self {
        Self::NotFound
    }
    pub fn conflict() -> Self {
        Self::Conflict
    }
    pub fn service_unavailable() -> Self {
        Self::ServiceUnavailable
    }

    pub fn internal(source: impl Into<Error>) -> Self {
        Self::Internal {
            source: source.into(),
        }
    }

    fn response_parts(&self) -> (StatusCode, ErrorEnvelope) {
        match self {
            Self::BadRequest { details } => (
                StatusCode::BAD_REQUEST,
                ErrorEnvelope::new("bad_request", "The request is invalid", details.clone()),
            ),
            Self::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                ErrorEnvelope::new("unauthorized", "Authentication is required", None),
            ),
            Self::Forbidden => (
                StatusCode::FORBIDDEN,
                ErrorEnvelope::new(
                    "forbidden",
                    "You are not allowed to perform this action",
                    None,
                ),
            ),
            Self::NotFound => (
                StatusCode::NOT_FOUND,
                ErrorEnvelope::new("not_found", "The requested resource was not found", None),
            ),
            Self::Conflict => (
                StatusCode::CONFLICT,
                ErrorEnvelope::new("conflict", "The request conflicts with current state", None),
            ),
            Self::ServiceUnavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                ErrorEnvelope::new(
                    "service_unavailable",
                    "Service is temporarily unavailable",
                    None,
                ),
            ),
            Self::Internal { .. } => (
                StatusCode::INTERNAL_SERVER_ERROR,
                ErrorEnvelope::new("internal_error", "An internal error occurred", None),
            ),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        if let Self::Internal { source } = &self {
            error!(error = %source, "internal API error");
        }
        let (status, body) = self.response_parts();
        (status, Json(body)).into_response()
    }
}
