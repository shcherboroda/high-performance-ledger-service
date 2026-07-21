use super::support::*;

#[tokio::test]
async fn authenticated_client_extracts_valid_rs256_subject() {
    let token = token(
        Some("client-123"),
        4_102_444_800,
        "https://issuer.example",
        "ledger",
    );
    let response = protected_response(Some(&format!("bEaReR {token}"))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(body.as_ref(), br#"{"client_id":"client-123"}"#);
}

#[tokio::test]
async fn authentication_failures_are_safe_unauthorized_responses() {
    let valid = token(
        Some("client-123"),
        4_102_444_800,
        "https://issuer.example",
        "ledger",
    );
    let expired = token(Some("client-123"), 1, "https://issuer.example", "ledger");
    let wrong_issuer = token(
        Some("client-123"),
        4_102_444_800,
        "https://other.example",
        "ledger",
    );
    let wrong_audience = token(
        Some("client-123"),
        4_102_444_800,
        "https://issuer.example",
        "other",
    );
    let missing_subject = token(None, 4_102_444_800, "https://issuer.example", "ledger");
    let blank_subject = token(
        Some("  "),
        4_102_444_800,
        "https://issuer.example",
        "ledger",
    );
    let hs256 = encode(
        &Header::new(Algorithm::HS256),
        &TestClaims {
            sub: Some("client-123".into()),
            exp: 4_102_444_800,
            iss: "https://issuer.example".into(),
            aud: "ledger".into(),
        },
        &EncodingKey::from_secret(b"not-an-rsa-key"),
    )
    .unwrap();
    let cases = [
        None,
        Some("not-a-bearer-header".to_owned()),
        Some("Basic value".to_owned()),
        Some("Bearer ".to_owned()),
        Some(format!("Bearer {valid}x")),
        Some(format!("Bearer {expired}")),
        Some(format!("Bearer {wrong_issuer}")),
        Some(format!("Bearer {wrong_audience}")),
        Some(format!("Bearer {missing_subject}")),
        Some(format!("Bearer {blank_subject}")),
        Some(format!("Bearer {hs256}")),
    ];
    for authorization in cases {
        let response = protected_response(authorization.as_deref()).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert!(body.contains("\"code\":\"unauthorized\""));
        assert!(!body.contains("client-123"));
        assert!(!body.contains("BEGIN"));
    }
}

#[tokio::test]
async fn duplicate_authorization_headers_are_rejected() {
    let token = token(
        Some("client-123"),
        4_102_444_800,
        "https://issuer.example",
        "ledger",
    );
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://postgres:postgres@127.0.0.1:1/ledger")
        .unwrap();
    let mut request = Request::get("/_test/authenticated")
        .body(Body::empty())
        .unwrap();
    request
        .headers_mut()
        .append("authorization", format!("Bearer {token}").parse().unwrap());
    request
        .headers_mut()
        .append("authorization", format!("Bearer {token}").parse().unwrap());

    let response = router(pool, test_auth()).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(
        std::str::from_utf8(&body).unwrap(),
        r#"{"error":{"code":"unauthorized","message":"Authentication is required","details":null,"request_id":null}}"#
    );
}

#[tokio::test]
async fn token_signed_by_another_rsa_key_is_rejected() {
    let token = token_with_key(
        Some("client-123"),
        4_102_444_800,
        "https://issuer.example",
        "ledger",
        EncodingKey::from_rsa_pem(OTHER_TEST_PRIVATE_KEY.as_bytes()).unwrap(),
    );
    let response = protected_response(Some(&format!("Bearer {token}"))).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(
        std::str::from_utf8(&body).unwrap(),
        r#"{"error":{"code":"unauthorized","message":"Authentication is required","details":null,"request_id":null}}"#
    );
}

#[tokio::test]
async fn shared_errors_have_stable_statuses_and_codes() {
    let cases = [
        (
            AppError::bad_request(Some(json!({"field": "amount"}))),
            StatusCode::BAD_REQUEST,
            "bad_request",
        ),
        (
            AppError::unauthorized(),
            StatusCode::UNAUTHORIZED,
            "unauthorized",
        ),
        (AppError::forbidden(), StatusCode::FORBIDDEN, "forbidden"),
        (AppError::not_found(), StatusCode::NOT_FOUND, "not_found"),
        (AppError::conflict(), StatusCode::CONFLICT, "conflict"),
        (
            AppError::service_unavailable(),
            StatusCode::SERVICE_UNAVAILABLE,
            "service_unavailable",
        ),
        (
            AppError::internal(anyhow::anyhow!("private database failure")),
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
        ),
    ];

    for (error, status, code) in cases {
        let response = error.into_response();
        assert_eq!(response.status(), status);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"]["code"], code);
        assert_eq!(body["error"]["request_id"], Value::Null);
    }
}

#[tokio::test]
async fn internal_errors_do_not_expose_their_source() {
    let response = AppError::internal(anyhow::anyhow!("postgres://user:password@localhost/ledger"))
        .into_response();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body = std::str::from_utf8(&body).unwrap();
    assert!(!body.contains("postgres://user:password"));
}
