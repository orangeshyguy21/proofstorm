use cashu::nuts::KeysResponse;

fn main() {
    let response = r#"{
      "keysets": [{
        "id": "009a1f293253e41e",
        "unit": "auth",
        "active": true,
        "keys": {
          "1": "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798"
        },
        "input_fee_ppk": null,
        "final_expiry": 1788376139
      }]
    }"#;

    let parsed: KeysResponse = serde_json::from_str(response).unwrap();
    println!("parsed keysets: {}", parsed.keysets.len());
    assert_eq!(parsed.keysets.len(), 1, "valid keyset must not be dropped");
}
