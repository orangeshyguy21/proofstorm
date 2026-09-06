use proofstorm_core::native::{NativeCommand, NativeOutput, OutputMode, project_invoice};
use serde_json::{Value, json};

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/invoice-relay.json")).unwrap()
}

#[test]
fn both_native_formats_produce_the_same_validated_invoice() {
    let f = fixture();
    let request = f["payment_request"].as_str().unwrap();
    let expected = project_invoice(request.as_bytes(), OutputMode::Bolt11).unwrap();
    assert_eq!(expected["payment_hash"], f["r_hash"]);
    assert_eq!(expected["amount_msat"], 700_000);
    assert_eq!(expected["currency"], "bcrt");
    assert!(expected["expires_at_unix"].as_u64().is_some());
    let mut response = f;
    response["payment_preimage"] = json!("private-canary");
    response["error"] = json!({"mnemonic":"private-canary"});
    let projected = project_invoice(
        &serde_json::to_vec(&response).unwrap(),
        OutputMode::LndInvoice,
    )
    .unwrap();
    assert_eq!(projected, expected);
    assert!(!projected.to_string().contains("private-canary"));
}

#[test]
fn rejects_ambiguous_malformed_and_mismatched_responses() {
    let f = fixture();
    let request = f["payment_request"].as_str().unwrap();
    let hash = f["r_hash"].as_str().unwrap();
    let valid = json!({"payment_request":request,"r_hash":hash}).to_string();
    for text in [
        format!("{valid}\n{valid}"),
        format!("[{valid}]"),
        json!([request, hash]).to_string(),
        format!(
            "{{\"payment_request\":\"bad\",\"payment_request\":\"{request}\",\"r_hash\":\"{hash}\"}}"
        ),
        format!(
            "{{\"payment_request\":\"{request}\",\"r_hash\":\"{hash}\",\"r_hash\":\"{hash}\"}}"
        ),
        json!({"payment_request":request}).to_string(),
        json!({"payment_request":[request],"r_hash":hash}).to_string(),
        json!({"payment_request":request,"r_hash":"00".repeat(32)}).to_string(),
        json!({"payment_request":request,"r_hash":hash.to_uppercase()}).to_string(),
        json!({"payment_request":"private-canary","r_hash":hash}).to_string(),
        "private-canary".into(),
    ] {
        let error = project_invoice(text.as_bytes(), OutputMode::LndInvoice).unwrap_err();
        assert!(!error.contains("private-canary"));
    }
    for text in [
        format!("invoice: {request}"),
        format!("{request}\n{request}"),
        format!("{request}x"),
        format!("{request}\nprivate-canary"),
    ] {
        assert!(project_invoice(text.as_bytes(), OutputMode::Bolt11).is_err());
    }
    assert!(project_invoice(&[0xff], OutputMode::Bolt11).is_err());
}

#[test]
fn enforces_bounded_explicit_output_contract() {
    assert_eq!(
        project_invoice(&vec![b'a'; 65537], OutputMode::LndInvoice),
        Err("invoice_response_too_large")
    );
    assert_eq!(
        project_invoice(&vec![b'a'; 4097], OutputMode::Bolt11),
        Err("invoice_too_large")
    );
    assert_eq!(
        project_invoice(b"anything", OutputMode::Private),
        Err("invoice_mode_invalid")
    );
    for mode in [OutputMode::Bolt11, OutputMode::LndInvoice] {
        let mut command = NativeCommand {
            private_io: None,
            script: String::new(),
            argv: vec!["native".into()],
            timeout_seconds: 30,
            output: NativeOutput {
                mode,
                fields: vec![],
            },
        };
        command.validate().unwrap();
        command.output.fields.push("payment_preimage".into());
        assert!(command.validate().is_err());
    }
}
