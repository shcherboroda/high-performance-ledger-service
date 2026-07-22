use super::support::*;

fn assert_local_schema_references_resolve(value: &Value, schemas: &Map<String, Value>) {
    match value {
        Value::Object(object) => {
            if let Some(reference) = object.get("$ref").and_then(Value::as_str)
                && let Some(name) = reference.strip_prefix("#/components/schemas/")
            {
                assert!(
                    schemas.contains_key(name),
                    "unresolved schema reference: {reference}"
                );
            }
            for value in object.values() {
                assert_local_schema_references_resolve(value, schemas);
            }
        }
        Value::Array(values) => {
            for value in values {
                assert_local_schema_references_resolve(value, schemas);
            }
        }
        _ => {}
    }
}

#[tokio::test]
async fn openapi_serves_documented_api_contract() {
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://postgres:postgres@127.0.0.1:1/ledger")
        .unwrap();
    let response = router(pool, test_auth())
        .oneshot(Request::get("/openapi.json").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("application/json")
    );
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let document: Value = serde_json::from_slice(&body).unwrap();
    assert!(document["openapi"].as_str().unwrap().starts_with("3.1"));
    let paths = document["paths"].as_object().unwrap();
    assert_eq!(paths.len(), 8);
    assert!(paths.contains_key("/health"));
    assert!(paths.contains_key("/ready"));
    assert!(paths.contains_key("/accounts"));
    assert!(paths.contains_key("/accounts/{account_id}/balance"));
    assert!(paths.contains_key("/accounts/{account_id}/entries"));
    assert!(paths.contains_key("/transfers"));
    assert!(!paths.contains_key("/fx-transfers"));
    assert!(paths.contains_key("/transfers/{transfer_id}"));
    assert!(paths.contains_key("/transfers/{transfer_id}/reversal"));
    assert!(paths["/ready"]["get"]["responses"].get("200").is_some());
    assert!(paths["/ready"]["get"]["responses"].get("503").is_some());
    assert!(paths["/health"]["get"].get("security").is_none());
    assert!(paths["/ready"]["get"].get("security").is_none());
    assert_eq!(
        paths["/accounts"]["post"]["security"][0]["bearerAuth"],
        json!([])
    );
    assert_eq!(
        paths["/accounts/{account_id}/balance"]["get"]["security"][0]["bearerAuth"],
        json!([])
    );
    assert_eq!(
        paths["/transfers"]["post"]["security"][0]["bearerAuth"],
        json!([])
    );
    assert_eq!(
        paths["/accounts/{account_id}/entries"]["get"]["security"][0]["bearerAuth"],
        json!([])
    );
    assert_eq!(
        paths["/transfers/{transfer_id}"]["get"]["security"][0]["bearerAuth"],
        json!([])
    );
    for path in ["/accounts/{account_id}/entries", "/transfers/{transfer_id}"] {
        let operation = &paths[path]["get"];
        assert_eq!(operation["security"][0]["bearerAuth"], json!([]));
        for status in ["200", "400", "401", "404", "500"] {
            assert!(
                operation["responses"].get(status).is_some(),
                "{path} is missing {status}"
            );
        }
    }
    let history = &paths["/accounts/{account_id}/entries"]["get"];
    let parameters = history["parameters"].as_array().unwrap();
    let parameter = |name: &str| {
        parameters
            .iter()
            .find(|value| value["name"] == name)
            .unwrap()
    };
    assert_eq!(parameter("account_id")["in"], "path");
    assert_eq!(parameter("counterparty_account_id")["in"], "query");
    assert_eq!(parameter("limit")["in"], "query");
    assert_eq!(parameter("cursor")["in"], "query");
    assert_eq!(parameter("limit")["schema"]["default"], 50);
    assert_eq!(parameter("limit")["schema"]["minimum"], 1);
    assert_eq!(parameter("limit")["schema"]["maximum"], 100);
    assert_eq!(
        paths["/transfers/{transfer_id}/reversal"]["post"]["security"][0]["bearerAuth"],
        json!([])
    );
    let reversal = &paths["/transfers/{transfer_id}/reversal"]["post"];
    assert_eq!(reversal["parameters"][0]["name"], "transfer_id");
    assert_eq!(reversal["parameters"][0]["in"], "path");
    assert_eq!(reversal["parameters"][0]["required"], true);
    assert_eq!(reversal["parameters"][1]["name"], "Idempotency-Key");
    assert_eq!(reversal["parameters"][1]["in"], "header");
    assert_eq!(reversal["parameters"][1]["required"], true);
    for status in ["201", "400", "401", "404", "409", "422", "500"] {
        assert!(
            reversal["responses"].get(status).is_some(),
            "missing {status}"
        );
    }
    assert_eq!(
        reversal["responses"]["201"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/ReversalCreatedResponse"
    );
    let security_schemes = document["components"]["securitySchemes"]
        .as_object()
        .unwrap();
    assert_eq!(security_schemes["bearerAuth"]["type"], "http");
    assert_eq!(security_schemes["bearerAuth"]["scheme"], "bearer");
    assert_eq!(security_schemes["bearerAuth"]["bearerFormat"], "JWT");
    let schemas = document["components"]["schemas"].as_object().unwrap();
    assert!(schemas.contains_key("ErrorEnvelope"));
    assert!(schemas.contains_key("ApiErrorBody"));
    assert!(schemas.contains_key("CreateAccountRequest"));
    assert!(schemas.contains_key("ReversalCreatedResponse"));
    assert!(schemas.contains_key("AccountCreatedResponse"));
    assert!(schemas.contains_key("AccountBalanceResponse"));
    assert!(schemas.contains_key("CreateTransferRequest"));
    assert!(schemas.contains_key("TransferCreatedResponse"));
    assert!(schemas.contains_key("TransferDetailsResponse"));
    assert!(schemas.contains_key("AccountEntryResponse"));
    assert!(schemas.contains_key("AccountHistoryResponse"));
    let transfer_response = &schemas["TransferCreatedResponse"];
    let variants = transfer_response["oneOf"].as_array().unwrap();
    assert_eq!(variants.len(), 2);
    let variant = |kind: &str| {
        variants
            .iter()
            .find(|schema| schema["properties"]["kind"]["enum"] == json!([kind]))
            .unwrap()
    };
    let ordinary = variant("transfer");
    let ordinary_properties = ordinary["properties"].as_object().unwrap();
    let ordinary_required = ordinary["required"].as_array().unwrap();
    for name in [
        "id",
        "kind",
        "status",
        "source_account_id",
        "destination_account_id",
        "currency",
        "amount",
        "created_at",
    ] {
        assert!(
            ordinary_properties.contains_key(name),
            "ordinary missing {name}"
        );
        assert!(ordinary_required.iter().any(|field| field == name));
    }
    for absent in [
        "source_currency",
        "source_amount",
        "destination_currency",
        "destination_amount",
        "fee_amount",
        "total_source_debit",
        "resulting_source_balance",
        "resulting_destination_balance",
    ] {
        assert!(
            !ordinary_properties.contains_key(absent),
            "ordinary contains {absent}"
        );
        assert!(!ordinary_required.iter().any(|field| field == absent));
    }
    let fx = variant("fx_transfer");
    let fx_properties = fx["properties"].as_object().unwrap();
    let fx_required = fx["required"].as_array().unwrap();
    for name in [
        "id",
        "kind",
        "status",
        "source_account_id",
        "destination_account_id",
        "source_currency",
        "source_amount",
        "destination_currency",
        "destination_amount",
        "fee_amount",
        "total_source_debit",
        "created_at",
    ] {
        assert!(fx_properties.contains_key(name), "FX missing {name}");
        assert!(fx_required.iter().any(|field| field == name));
        assert!(!fx_properties[name].to_string().contains("null"));
    }
    for absent in [
        "currency",
        "amount",
        "resulting_source_balance",
        "resulting_destination_balance",
    ] {
        assert!(!fx_properties.contains_key(absent), "FX contains {absent}");
        assert!(!fx_required.iter().any(|field| field == absent));
    }
    let transfer_request = &schemas["CreateTransferRequest"];
    assert_eq!(
        transfer_request["required"],
        json!(["source_account_id", "destination_account_id", "amount"])
    );
    assert_eq!(
        paths["/transfers"]["post"]["responses"]["201"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/TransferCreatedResponse"
    );
    for status in ["400", "401", "404", "409", "422", "500"] {
        assert!(
            paths["/transfers"]["post"]["responses"]
                .get(status)
                .is_some()
        );
    }
    assert_local_schema_references_resolve(&document, schemas);
    assert!(!std::str::from_utf8(&body).unwrap().contains("postgres://"));
}
