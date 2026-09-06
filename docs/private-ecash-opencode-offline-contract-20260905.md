# Installed OpenCode private-transfer contract check

**Passed offline.** The replacement method-specific schema survives the inspected installed-client conversion, and complete synthetic preparation arguments reach local MCP unchanged. This is separate evidence from the failed funded agent checkpoint; its score remains failed.

## Exact client and source

The installed `/Users/admin/.opencode/bin/opencode` reports **1.18.28**, matching run02's log. Its SHA-256 is `d099d7025feb3663e6ad50513d764d9a6868d42d5e2d88eae01a7b2aeafeea9f`. Source was extracted directly from the executable's embedded Bun JavaScript, not substituted from a newer checkout. `source-provenance.json` records each embedded module, exact byte offsets and function hash. The bundled Anthropic SDK reports **3.0.111**; the embedded `@ai-sdk/anthropic` loader resolves to its `createAnthropic` export.

Inspected and replayed functions:

- `McpCatalog.convertTool`: copies the MCP input schema, wraps it as a dynamic tool and forwards `arguments` to `callTool`.
- `ProviderTransform.schema`: preserves nested union branches for the examined K3/Kimi paths; Kimi normalization removes siblings of `$ref`, not the referenced definitions or their `oneOf` branches.
- AI SDK `jsonSchema`, `asSchema`, `safeValidateTypes` and `dynamicTool`: these JSON-schema wrappers have no supplied validator, so the inspected validation path returns the supplied argument object unchanged.
- Anthropic `prepareTools`: passes the resulting schema to `input_schema`, including `$defs` and the nested reference.

## Checks and results

The coordinator's exact generated schema was copied from `private-ecash-contract-20260905/private-transfer-tool-schema.json`. Its `transfer` property references `#/$defs/PrivateTransferInput`; that definition has four `oneOf` branches. The `prepare` branch requires both `destinationComponent` and `maximumBytes`.

| Offline check | Result |
| --- | --- |
| Replacement schema through K3 and Kimi-ID normalization | Four branches and required preparation fields retained |
| Anthropic tool-schema preparation | Full transformed schema preserved in `input_schema` |
| Complete synthetic prepare through conversion/forwarding | Destination and capacity reached the fake MCP bridge unchanged |
| Same complete request with run02's old schema | Also forwarded unchanged |
| Replacement schema through extracted adapter to real debug MCP stdio | Complete request reached nonexistent-instance `not_found`, JSON-RPC code -32002 |
| Missing destination | Immediate `result.isError` naming `destinationComponent` |
| Missing capacity | Immediate `result.isError` naming `maximumBytes` |
| Isolated local store after checks | Zero instances, operations and actions |

The missing-field refusals are MCP tool errors, not JSON-RPC -32602 responses. Exact envelopes are retained. The complete request's nonexistent-instance error establishes successful parameter decoding and progression to instance lookup; it does not exercise live admission or custody.

## Scope and reproducibility

Evidence: `dev/agent-usability-runs/private-ecash-opencode-offline-20260905/`. It includes extracted functions, installed-source provenance, the exact input schema, transformed provider schemas, argument captures, local stdio responses, debug MCP checksum, zero-action store audit and artifact hashes. Run the retained `replay.mjs` from the repository root with Node to reproduce against a matching debug MCP build and a clean isolated output database.

This executes exact installed conversion functions in an isolated JavaScript context, first with a fake MCP bridge and then a local MCP stdio endpoint. Cache-control was supplied as a no-cache dependency. It does **not** boot a full OpenCode session, execute plugin hooks, invoke model inference, send a provider HTTP request, load Kubernetes configuration or access the cluster. No product files or deployed pins were changed.

The check demonstrates that offline validation of the actual installed conversion functions is feasible and that the replacement schema works through those functions. It does not prove the full inference/client stack's behavior, nor recover run02's unavailable raw provider arguments. In particular, the successful replay with the old schema prevents attributing the earlier omissions to these conversion functions from the available evidence alone. Provider emission, model omission and uninspected hooks remain unlocalized possibilities, not established causes.

Next: retain these results alongside the coordinator's real stdio regressions. A future actual-session check should preserve synthetic argument evidence on both sides of tool validation before another funded dispatch. The existing failed model result must not be relabeled as passed.
