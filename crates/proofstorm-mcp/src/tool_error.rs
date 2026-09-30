//! Preserve actionable tool failures for clients that discard JSON-RPC data.
use rmcp::{ErrorData, model::CallToolResult};
use serde_json::json;

pub(super) fn result(error: ErrorData) -> CallToolResult {
    let value = json!(error);
    let result = CallToolResult::structured_error(value);
    if crate::serialized_size(&result).is_ok_and(|size| size <= crate::MAX_AGENT_RESPONSE_BYTES) {
        return result;
    }

    // Error details can contain large validation lists or caller input. Keep
    // the original classification and an explicit omission marker, with a
    // bounded message; never turn a rejected call into a successful response.
    let mut message = error.message.into_owned();
    let message_truncated = message.len() > 2048;
    if message_truncated {
        let mut end = 2048;
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        message.truncate(end);
    }
    let code = error
        .data
        .as_ref()
        .and_then(|data| data.get("code"))
        .and_then(serde_json::Value::as_str)
        .filter(|code| code.len() <= 128);
    CallToolResult::structured_error(json!({
        "code": error.code,
        "message": message,
        "data": {
            "code": code,
            "details_omitted": true,
            "message_truncated": message_truncated,
            "maximum_response_bytes": crate::MAX_AGENT_RESPONSE_BYTES,
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::ServiceExt;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};

    fn payload(result: &CallToolResult) -> serde_json::Value {
        assert_eq!(result.is_error, Some(true));
        let wire = json!(result);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(wire["content"][0]["text"].as_str().unwrap())
                .unwrap(),
            wire["structuredContent"]
        );
        assert!(serde_json::to_vec(&wire).unwrap().len() <= crate::MAX_AGENT_RESPONSE_BYTES);
        wire["structuredContent"].clone()
    }

    #[test]
    fn text_and_structured_errors_preserve_recovery_details_and_classification() {
        for error in [
            ErrorData::invalid_request(
                "Cell failed preflight; nothing accepted",
                Some(json!({
                    "code":"cell_plan_invalid", "validation":{"issues":[{
                        "path":"/api_version", "code":"unsupported_api_version",
                        "message":"expected proofstorm/v1alpha1"
                    }]}
                })),
            ),
            ErrorData::resource_not_found("missing cell", Some(json!({"code":"not_found"}))),
            ErrorData::internal_error(
                "runtime unavailable",
                Some(json!({"code":"runtime_failure"})),
            ),
            ErrorData::invalid_params("missing field name", None),
        ] {
            let expected = json!(error);
            assert_eq!(payload(&result(error)), expected);
        }
    }

    #[test]
    fn oversized_details_remain_bounded_and_explicitly_failed() {
        for message in [
            "preflight failed".to_owned(),
            "🦀\n\"".repeat(10_000),
            "\0".repeat(10_000),
        ] {
            let error = ErrorData::invalid_request(
                message,
                Some(json!({
                    "code":"cell_plan_invalid", "issues":"x".repeat(40_000)
                })),
            );
            let value = payload(&result(error));
            assert_eq!(value["code"], -32600);
            assert_eq!(value["data"]["code"], "cell_plan_invalid");
            assert_eq!(value["data"]["details_omitted"], true);
        }
        let error = ErrorData::invalid_request(
            "\0".repeat(10_000),
            Some(json!({
                "code":"\0".repeat(128), "issues":"x".repeat(40_000)
            })),
        );
        payload(&result(error));
    }

    async fn exchange(
        reader: &mut BufReader<ReadHalf<DuplexStream>>,
        writer: &mut WriteHalf<DuplexStream>,
        id: u64,
        method: &str,
        params: serde_json::Value,
    ) -> serde_json::Value {
        writer
            .write_all(
                format!(
                    "{}\n",
                    json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        loop {
            let mut line = String::new();
            assert!(reader.read_line(&mut line).await.unwrap() > 0);
            let reply: serde_json::Value = serde_json::from_str(&line).unwrap();
            if reply["id"] == id {
                return reply;
            }
        }
    }

    #[tokio::test]
    async fn wire_errors_preserve_details_authority_and_protocol_distinction() {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let store = crate::tests::seeded_store();
            let mcp = crate::ProofstormMcp::new(store.clone(), "alpha", "designer").unwrap();
            let (client, server) = tokio::io::duplex(128 * 1024);
            let serving = tokio::spawn(async move {
                mcp.serve(server).await.unwrap().waiting().await.unwrap();
            });
            let (read, mut write) = tokio::io::split(client);
            let mut read = BufReader::new(read);
            let init = exchange(&mut read, &mut write, 1, "initialize", json!({
                "protocolVersion":"2025-11-25", "capabilities":{},
                "clientInfo":{"name":"error-regression","version":"1"}
            })).await;
            assert!(init.get("result").is_some());
            write.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n").await.unwrap();

            let mut cell: serde_json::Value = serde_json::from_str(include_str!("../../../examples/developer-cell.json")).unwrap();
            cell["api_version"] = json!("unsupported");
            let rejected = exchange(&mut read, &mut write, 2, "tools/call", json!({
                "name":"cell_plan", "arguments":{"name":"invalid","request_id":"invalid","cell":cell}
            })).await;
            assert!(rejected.get("error").is_none(), "{rejected}");
            let result: CallToolResult = serde_json::from_value(rejected["result"].clone()).unwrap();
            let value = payload(&result);
            assert_eq!(value["data"]["code"], "cell_plan_invalid");
            assert!(value["data"]["validation"]["issues"].as_array().unwrap().iter().any(|issue| issue["code"] == "unsupported_api_version" && issue["message"].as_str().unwrap().contains("proofstorm/v1alpha1")));
            assert!(store.resolve_cell("alpha", "designer", "invalid").is_err());

            let success = exchange(&mut read, &mut write, 7, "tools/call", json!({"name":"catalog_list","arguments":{"scan":true}})).await;
            assert!(success.get("error").is_none(), "{success}");
            assert_ne!(success["result"]["isError"], true, "{success}");

            let malformed = exchange(&mut read, &mut write, 3, "tools/call", json!({"name":"catalog_list","arguments":{"limit":"wrong-type"}})).await;
            assert_eq!(malformed["result"]["isError"], true, "{malformed}");
            let unknown = exchange(&mut read, &mut write, 4, "tools/call", json!({"name":"unknown_tool","arguments":{}})).await;
            assert!(unknown.get("error").is_some(), "{unknown}");
            let invalid_envelope = exchange(&mut read, &mut write, 5, "tools/call", json!({"arguments":{}})).await;
            assert!(invalid_envelope.get("error").is_some(), "{invalid_envelope}");

            // Revocation after discovery must refuse before entering a handler.
            store.replace_grants("alpha", "designer", []).unwrap();
            let denied = exchange(&mut read, &mut write, 6, "tools/call", json!({"name":"catalog_list","arguments":{}})).await;
            assert_eq!(denied["result"]["isError"], true);
            assert_eq!(denied["result"]["structuredContent"]["data"]["code"], "access_denied");
            drop(read);
            drop(write);
            serving.await.unwrap();
        }).await.expect("wire test timed out");
    }
}
