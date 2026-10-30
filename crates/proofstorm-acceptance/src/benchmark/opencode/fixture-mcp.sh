#!/usr/bin/env bash
# Opt-in real-CLI contract fixture: a stdio MCP server with one tool, no accounts.
set -euo pipefail
work=${1:?usage: fixture-mcp.sh WORK}
while IFS= read -r line; do
  # Notifications carry no id and get no reply.
  jq -e 'has("id")' <<< "$line" > /dev/null || continue
  case $(jq -r '.method // ""' <<< "$line") in
    initialize)
      printf '{}' > "$work/proxy-ready.json"
      result='{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}}' ;;
    tools/list)
      result='{"tools":[{"name":"catalog_list","description":"Fixture tool","inputSchema":{"type":"object","properties":{},"additionalProperties":false}}]}' ;;
    tools/call)
      jq -c '{kind: "start", id: 1, tool: .params.name, arguments: (.params.arguments // {})}' <<< "$line" >> "$work/events.jsonl"
      printf '%s\n' '{"kind":"end","id":1,"success":true,"elapsed_ms":1}' >> "$work/events.jsonl"
      result='{"content":[{"type":"text","text":"fixture ok"}],"isError":false}' ;;
    *) result='{}' ;;
  esac
  jq -cn --argjson request "$line" --argjson result "$result" '{jsonrpc: "2.0", id: $request.id, result: $result}'
done
