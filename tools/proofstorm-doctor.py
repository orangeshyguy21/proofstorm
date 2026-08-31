#!/usr/bin/env python3
import json
import os
import selectors
import subprocess
import sys
import tempfile


def fail(message, process=None):
    detail = ""
    if process is not None and process.poll() is not None:
        detail = process.stderr.read().strip()
    raise RuntimeError(f"{message}{': ' + detail if detail else ''}")


def read_response(process, selector, expected_id):
    if not selector.select(timeout=15):
        fail(f"MCP response {expected_id} timed out", process)
    line = process.stdout.readline()
    if not line:
        fail(f"MCP server closed before response {expected_id}", process)
    response = json.loads(line)
    if response.get("id") != expected_id or "error" in response:
        fail(f"unexpected MCP response: {response}", process)
    return response["result"]


def send(process, message):
    process.stdin.write(json.dumps(message, separators=(",", ":")) + "\n")
    process.stdin.flush()


def main():
    binary, config_path = sys.argv[1:3]
    with open(config_path, encoding="utf-8") as config_file:
        server = json.load(config_file)["mcp"]["proofstorm"]
    environment = os.environ.copy()
    environment.update(server["environment"])

    with tempfile.TemporaryDirectory(prefix="proofstorm-doctor-") as directory:
        environment["PROOFSTORM_DB"] = os.path.join(directory, "doctor.sqlite3")
        process = subprocess.Popen(
            [binary],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            env=environment,
        )
        selector = selectors.DefaultSelector()
        selector.register(process.stdout, selectors.EVENT_READ)
        try:
            send(
                process,
                {
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {
                        "protocolVersion": "2025-11-25",
                        "capabilities": {},
                        "clientInfo": {"name": "proofstorm-doctor", "version": "v1alpha1"},
                    },
                },
            )
            read_response(process, selector, 1)
            send(process, {"jsonrpc": "2.0", "method": "notifications/initialized"})
            send(
                process,
                {"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}},
            )
            tools = read_response(process, selector, 2)["tools"]
            names = {tool["name"] for tool in tools}
            expected = {
                "proofstorm_catalog_list",
                "proofstorm_component_add",
                "proofstorm_lab_materialize",
                "proofstorm_node_start",
                "proofstorm_node_stop",
                "proofstorm_node_restart",
                "proofstorm_peer_connect",
                "proofstorm_peer_disconnect",
                "proofstorm_channel_open",
                "proofstorm_channel_close",
                "proofstorm_channel_force_close",
                "proofstorm_channel_rebalance",
                "proofstorm_network_capabilities",
                "proofstorm_network_delay",
                "proofstorm_network_loss",
                "proofstorm_network_partition",
                "proofstorm_network_heal",
                "proofstorm_wallet_initialize",
                "proofstorm_wallet_balance",
                "proofstorm_wallet_fund",
                "proofstorm_wallet_invoice",
                "proofstorm_wallet_pay",
                "proofstorm_wallet_quote_status",
                "proofstorm_wallet_quote_list",
                "proofstorm_conservation_oracle",
                "proofstorm_reachability_oracle",
                "proofstorm_artifact_export",
                "proofstorm_lab_close",
            }
            missing = sorted(expected - names)
            if missing:
                fail(f"MCP capability configuration hides required tools: {missing}", process)
        finally:
            selector.close()
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)

    print(f"MCP stdio handshake passed with {len(names)} capability-filtered tools")


if __name__ == "__main__":
    main()
