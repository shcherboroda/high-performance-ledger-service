use axum::{
    extract::FromRequestParts,
    http::{header::AUTHORIZATION, request::Parts},
};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode};
use serde::Deserialize;

use crate::{api_error::AppError, app::AppState, config::AuthConfig};

#[derive(Clone)]
pub struct AuthVerifier {
    decoding_key: DecodingKey,
    validation: Validation,
}

impl AuthVerifier {
    pub fn new(config: &AuthConfig) -> anyhow::Result<Self> {
        let decoding_key = DecodingKey::from_rsa_pem(config.public_key_pem().as_bytes())?;
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_issuer(&[&config.issuer]);
        validation.set_audience(&[&config.audience]);
        validation.required_spec_claims.insert("exp".to_owned());
        Ok(Self {
            decoding_key,
            validation,
        })
    }

    fn authenticate(&self, token: &str) -> Result<AuthenticatedClient, AppError> {
        let claims = decode::<Claims>(token, &self.decoding_key, &self.validation)
            .map_err(|_| AppError::unauthorized())?
            .claims;
        let client_id = claims.sub.ok_or_else(AppError::unauthorized)?;
        if client_id.trim().is_empty() {
            return Err(AppError::unauthorized());
        }
        Ok(AuthenticatedClient { client_id })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthenticatedClient {
    pub client_id: String,
}

#[derive(Deserialize)]
struct Claims {
    sub: Option<String>,
}

impl FromRequestParts<AppState> for AuthenticatedClient {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let authorization = parts
            .headers
            .get_all(AUTHORIZATION)
            .iter()
            .collect::<Vec<_>>();
        if authorization.len() != 1 {
            return Err(AppError::unauthorized());
        }
        let value = authorization[0]
            .to_str()
            .map_err(|_| AppError::unauthorized())?;
        let mut components = value.split_whitespace();
        let scheme = components.next().ok_or_else(AppError::unauthorized)?;
        let token = components.next().ok_or_else(AppError::unauthorized)?;
        if !scheme.eq_ignore_ascii_case("Bearer") || token.is_empty() || components.next().is_some()
        {
            return Err(AppError::unauthorized());
        }
        state.auth.authenticate(token)
    }
}
