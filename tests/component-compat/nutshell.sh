#!/usr/bin/env bash
# Called by bitcoin-lightning.sh while its owned, funded regtest pair is alive.
set -Eeuo pipefail
main() {
umask 077
[[ $# == 7 ]] || exit 2
work=$1 pair_directory=$2 prefix=$3 implementation=$4 platform=$5 run_id=$6 helper=$7
run() { "$helper" release-run 60 docker "$@"; }
node() {
  local container=$1; shift
  if [[ "$implementation" == lnd ]]; then run exec "$container" lncli --lnddir=/home/lnd/.lnd --network=regtest "$@"
  else run exec "$container" lightning-cli --notifications=none --lightning-dir=/home/cln/.lightning --network=regtest "$@"; fi
}
# Give the independent payer inbound liquidity for real mint issuance.
if [[ "$implementation" == lnd ]]; then
  node "$prefix-south" addinvoice --amt=100000 --memo=mint-liquidity > "$pair_directory/liquidity-invoice.json"
  invoice=$(jq -er '.payment_request' "$pair_directory/liquidity-invoice.json")
  node "$prefix-north" sendpayment --force --json --timeout=30s --pay_req="$invoice" > "$pair_directory/liquidity-payment.json"
  jq -e '.status=="SUCCEEDED"' "$pair_directory/liquidity-payment.json" >/dev/null
else
  node "$prefix-south" invoice 100000000 mint-liquidity compatibility > "$pair_directory/liquidity-invoice.json"
  invoice=$(jq -er '.bolt11' "$pair_directory/liquidity-invoice.json")
  node "$prefix-north" pay "$invoice" > "$pair_directory/liquidity-payment.json"
  jq -e '.status=="complete"' "$pair_directory/liquidity-payment.json" >/dev/null
fi
# ShellCheck 0.9 cannot follow exported calls into independent Bash cases.
# shellcheck disable=SC2317,SC2329
mint_case() (
  set -Eeuo pipefail
  entry=$1 index=$2
  [[ $(jq -r '.implementation' <<< "$entry") == nutshell ]] || { echo 'Unsupported mint matrix entry' >&2; exit 2; }
  version=$(jq -er '.version' <<< "$entry")
  case "$version" in 0.20.3|0.21.0) ;; *) exit 2 ;; esac
  mint_image=$(jq -er '.image_id' <<< "$entry")
  directory="$pair_directory/mint-$index"; mkdir "$directory"
  jq -n --argjson mint "$entry" --slurpfile pair "$pair_directory/selection.json" '{mint:$mint,pair:$pair[0]}' > "$directory/selection.json"
  name="$prefix-mint-$index"; containers=('') volumes=('') stage=setup
  # shellcheck disable=SC2329 # EXIT trap retains logs and removes only owned resources.
  cleanup() {
    code=$?; trap - EXIT INT TERM; clean=true
    for container in "${containers[@]}"; do
      [[ -n "$container" ]] || continue
      docker logs "$container" > "$directory/$container.log" 2>&1 || true
      owner=$(docker inspect --format '{{index .Config.Labels "dev.proofstorm.compat.run"}}' "$container" 2>/dev/null) || continue
      if [[ "$owner" == "$run_id" ]]; then docker rm -f "$container" >/dev/null || clean=false; else clean=false; fi
    done
    for volume in "${volumes[@]}"; do
      [[ -n "$volume" ]] || continue
      owner=$(docker volume inspect --format '{{index .Labels "dev.proofstorm.compat.run"}}' "$volume" 2>/dev/null) || continue
      if [[ "$owner" == "$run_id" ]]; then docker volume rm "$volume" >/dev/null || clean=false; else clean=false; fi
    done
    [[ "$clean" == true ]] || code=1
    jq --arg stage "$stage" --argjson code "$code" --argjson clean "$clean" '. + {passed:($code==0),last_stage:$stage,exit_code:$code,cleanup_verified:$clean}' "$directory/selection.json" > "$directory/result.json"
    exit "$code"
  }
  trap cleanup EXIT
  trap 'exit 130' INT TERM
  volume_new() {
    local volume=$1; volumes+=("$volume")
    docker volume create --label "dev.proofstorm.compat.run=$run_id" "$volume" >/dev/null
    run run --rm --platform "$platform" --network none --label "dev.proofstorm.compat.run=$run_id" --user 0:0 --cap-drop ALL --cap-add CHOWN \
      --volume "$volume:/state" --entrypoint sh "$mint_image" -ec 'chown 1000:1000 /state'
  }
  await_ready() {
    local attempt
    for ((attempt=0; attempt<90; attempt++)); do
      if run exec "$name" /opt/proofstorm/driver ready nutshell http://127.0.0.1:3338/v1/info >/dev/null 2>&1; then return; fi
      sleep 1
    done
    return 1
  }
  # Disposable keys: host parent evidence directories remain private (0700).
  mkdir -p "$directory/tls/server" "$directory/tls/client"
  tls="$directory/tls"
  openssl req -x509 -newkey rsa:2048 -nodes -sha256 -days 1 -subj /CN=compat-ca -keyout "$tls/ca.key" -out "$tls/ca.pem" >/dev/null 2>&1
  for role in server client; do
    openssl req -new -newkey rsa:2048 -nodes -subj "/CN=$role" -keyout "$tls/$role/$role.key" -out "$tls/$role.csr" >/dev/null 2>&1
    if [[ "$role" == server ]]; then printf 'subjectAltName=IP:127.0.0.1\nextendedKeyUsage=serverAuth\n' > "$tls/extensions"
    else printf 'extendedKeyUsage=clientAuth\n' > "$tls/extensions"; fi
    openssl x509 -req -in "$tls/$role.csr" -CA "$tls/ca.pem" -CAkey "$tls/ca.key" -CAcreateserial -days 1 -sha256 -extfile "$tls/extensions" -out "$tls/$role/$role.pem" >/dev/null 2>&1
    cp "$tls/ca.pem" "$tls/$role/ca.pem"
    chmod 755 "$tls/$role"; chmod 644 "$tls/$role/"*
  done
  if [[ "$implementation" == lnd ]]; then
    jq '.resources.configMaps[]|select(.metadata.name=="mint-config")|.data' "$work/nutshell-render.json" > "$directory/rendered-env.json"
  else
    jq '.resources.configMaps[]|select(.metadata.name=="mint-config")|.data' "$work/nutshell-cln-render.json" > "$directory/rendered-env.json"
  fi
  # Alias and contract marker are the same transformations covered by renderer tests.
  jq -r --arg version "$version" --arg implementation "$implementation" '
    if $implementation=="lnd" then .MINT_LND_REST_ENDPOINT="https://north:8080" else .MINT_CLNREST_URL="http://north:3010" end |
    if $version=="0.21.0" then .PROOFSTORM_NUTSHELL_VERSION=$version | if $implementation=="cln" then .MINT_CLNREST_RUNE="/app/data/.proofstorm/cln-xpay.rune" else . end
    else del(.PROOFSTORM_NUTSHELL_VERSION) | if $implementation=="cln" then .MINT_CLNREST_RUNE="/app/data/.proofstorm/cln.rune" else . end end |
    to_entries[]|"\(.key)=\(.value)"' "$directory/rendered-env.json" > "$directory/mint.env"
  printf 'MINT_PRIVATE_KEY=disposable-%s\n' "$name" >> "$directory/mint.env"
  volume_new "$name-data"
  command='exec mint'
  if [[ "$implementation" == cln ]]; then
    if [[ "$version" == 0.21.0 ]]; then command='/opt/proofstorm/driver cln-mint-rune xpay; exec mint'
    else command='/opt/proofstorm/driver cln-mint-rune; exec mint'; fi
    data=/cln
  else data=/lnd; fi
  containers+=("$name")
  run run -d --platform "$platform" --name "$name" --network "$prefix" --network-alias "$name" \
    --label "dev.proofstorm.compat.run=$run_id" --user 1000:1000 --cap-drop ALL --security-opt no-new-privileges \
    --env-file "$directory/mint.env" --volume "$name-data:/app/data" --volume "$prefix-north-data:$data:ro" \
    --volume "$work/driver:/opt/proofstorm/driver:ro" \
    --volume "$tls/server:/management-server/tls:ro" --volume "$tls/client:/management-client/tls:ro" \
    --entrypoint sh "$mint_image" -ec "$command" > /dev/null
  stage=mint-startup
  [[ $(run exec "$name" mint --version) == "Nutshell, version $version" ]]
  await_ready
  run exec "$name" /opt/proofstorm/driver nutshell settings > "$directory/settings.json"
  for identity in server missing; do
    if [[ "$identity" == server ]]; then identity_directory=/management-server/tls; else identity_directory=/management-client/tls; fi
    if run exec "$name" /opt/proofstorm/driver management "$identity" https://127.0.0.1:8086 "$identity_directory" > "$directory/auth-$identity.json" 2>&1; then exit 1; fi
  done
  if run exec "$name" /opt/proofstorm/driver management plaintext http://127.0.0.1:8086 /management-client/tls > "$directory/auth-plaintext.json" 2>&1; then exit 1; fi
  if [[ "$implementation" == cln ]]; then
    run exec "$name" /opt/proofstorm/driver nutshell rune-probe > "$directory/rune-before.json"
    jq -e '.length>=32 and .mode=="0o600" and (.allowed==200 or .allowed==201) and (.forbidden==401 or .forbidden==403)' "$directory/rune-before.json" >/dev/null
  fi
  wallet_index=0
  while IFS= read -r wallet; do
    wallet_index=$((wallet_index+1))
    wallet_implementation=$(jq -er '.implementation' <<< "$wallet")
    case "$wallet_implementation" in nutshell-wallet|cdk-cli-wallet) ;; *) exit 2 ;; esac
    wallet_name="$name-wallet-$wallet_index"
    wallet_image=$(jq -er '.image_id' <<< "$wallet")
    wallet_directory="$directory/wallet-$wallet_index"; mkdir "$wallet_directory"
    printf '%s\n' "$wallet" > "$wallet_directory/selection.json"
    volume_new "$wallet_name-data"
    containers+=("$wallet_name")
    run run -d --name "$wallet_name" --platform "$platform" --network "$prefix" --label "dev.proofstorm.compat.run=$run_id" \
      --user 1000:1000 --cap-drop ALL --security-opt no-new-privileges --volume "$wallet_name-data:/wallet" \
      --volume "$work/driver:/opt/proofstorm/driver:ro" --env TOR=false --env LOG_LEVEL=ERROR \
      --entrypoint sh "$wallet_image" -ec 'mkdir -p /wallet/alice /wallet/bob; trap "exit 0" TERM INT; while :; do sleep 3600 & wait $!; done' > /dev/null
    if [[ "$wallet_implementation" == cdk-cli-wallet ]]; then
      stage="wallet-$wallet_index-cdk-contract"
      bash "$work/cdk-wallet-runner.sh" "$wallet_name" "$name" "$wallet_directory" "$(jq -er '.version' <<< "$wallet")" "$prefix" "$implementation" "$helper"
      continue
    fi
    # cashu has no --version; both entrypoints come from the same pinned package.
    [[ $(run exec "$wallet_name" mint --version) == "Nutshell, version $(jq -er '.version' <<< "$wallet")" ]]
    cli() {
      local user=$1; shift
      run exec --env "HOME=/wallet/$user" --env "CASHU_DIR=/wallet/$user/.cashu" "$wallet_name" \
        cashu -h "http://$name:3338" -u sat -w wallet -t -y "$@"
    }
    balance() {
      cli "$1" balance > "$wallet_directory/$1-balance.log" 2>&1
      sed -n 's/^Balance: \([0-9][0-9]*\) sat$/\1/p' "$wallet_directory/$1-balance.log"
    }
    stage="wallet-$wallet_index-mint"
    cli alice invoice 10000 --no-check > "$wallet_directory/quote.log" 2>&1
    quote=$(sed -n 's/.*--id \([^[:space:]]*\).*/\1/p' "$wallet_directory/quote.log")
    [[ -n "$quote" && "$quote" != *$'\n'* ]]
    invoice=$(grep -Eo 'lnbcrt[0-9a-z]+' "$wallet_directory/quote.log")
    [[ -n "$invoice" && "$invoice" != *$'\n'* ]]
    if [[ "$implementation" == lnd ]]; then
      node "$prefix-south" sendpayment --force --json --timeout=30s --pay_req="$invoice" > "$wallet_directory/funding.json"
      jq -e '.status=="SUCCEEDED"' "$wallet_directory/funding.json" >/dev/null
    else
      node "$prefix-south" pay "$invoice" > "$wallet_directory/funding.json"
      jq -e '.status=="complete"' "$wallet_directory/funding.json" >/dev/null
    fi
    cli alice invoice 10000 --id "$quote" > "$wallet_directory/claim-native.log" 2>&1
    run exec --env HOME=/wallet/alice --env CASHU_DIR=/wallet/alice/.cashu --env PROOFSTORM_WALLET=alice --env PROOFSTORM_MINT=mint \
      --env PROOFSTORM_OBSERVATION_ROLE=claim_receive \
      --env "PROOFSTORM_EXPECTED_MINT_URL=http://$name:3338" --env "PROOFSTORM_MINT_QUOTE_ID=$quote" "$wallet_name" \
      /opt/proofstorm/driver quote observe-receive > "$wallet_directory/claim.json"
    jq -e '.state=="ISSUED" and .amount_sat==10000 and .wallet_id=="alice"' "$wallet_directory/claim.json" >/dev/null
    [[ $(balance alice) == 10000 && $(balance bob) == 0 ]]
    stage="wallet-$wallet_index-transfer"
    cli alice send 1000 > "$wallet_directory/send.log" 2>&1
    token=$(grep -Eo 'cashu[AB][A-Za-z0-9_=-]+' "$wallet_directory/send.log")
    [[ -n "$token" && "$token" != *$'\n'* ]]
    cli bob receive "$token" > "$wallet_directory/receive.log" 2>&1
    bob_balance=$(balance bob)
    # The rendered 100-ppk input fee charges one sat for this receive.
    [[ "$bob_balance" == 999 ]]
    stage="wallet-$wallet_index-melt"
    label="melt-$index-$wallet_index"
    if [[ "$implementation" == lnd ]]; then
      node "$prefix-south" addinvoice --amt=500 --memo="$label" > "$wallet_directory/melt-invoice.json"
      invoice=$(jq -er '.payment_request' "$wallet_directory/melt-invoice.json")
    else
      node "$prefix-south" invoice 500000 "$label" compatibility > "$wallet_directory/melt-invoice.json"
      invoice=$(jq -er '.bolt11' "$wallet_directory/melt-invoice.json")
    fi
    cli alice pay "$invoice" > "$wallet_directory/melt.log" 2>&1
    if [[ "$implementation" == lnd ]]; then
      node "$prefix-south" listinvoices > "$wallet_directory/settlement.json"
      jq -e --arg label "$label" '.invoices|any(.memo==$label and .settled==true and (.amt_paid_sat|tonumber)==500)' "$wallet_directory/settlement.json" >/dev/null
    else
      node "$prefix-south" listinvoices "$label" > "$wallet_directory/settlement.json"
      jq -e '.invoices|any(.status=="paid" and .amount_received_msat==500000)' "$wallet_directory/settlement.json" >/dev/null
    fi
    before=$(balance alice)
    [[ "$before" == 8499 ]]
    stage="wallet-$wallet_index-restart"
    run restart --time 30 "$name" "$wallet_name" >/dev/null
    await_ready
    [[ $(balance alice) == "$before" && $(balance bob) == "$bob_balance" ]]
    run exec "$name" /opt/proofstorm/driver nutshell settings > "$wallet_directory/settings-after-restart.json"
    if [[ "$implementation" == cln ]]; then
      run exec "$name" /opt/proofstorm/driver nutshell rune-probe > "$wallet_directory/rune-after.json"
      cmp "$directory/rune-before.json" "$wallet_directory/rune-after.json"
    fi
    jq -n --argjson wallet "$wallet" --argjson alice "$before" --argjson bob "$bob_balance" \
      '{passed:true,wallet:$wallet,minted_sat:10000,invoice_settled_sat:500,alice_balance_sat:$alice,bob_balance_sat:$bob,input_fees_sat:(10000-500-$alice-$bob),restart_preserved_balances:true}' > "$wallet_directory/result.json"
  done < "$work/wallets.jsonl"
  [[ "$wallet_index" -gt 0 ]]
  stage=complete
)
export -f run node mint_case
export work pair_directory prefix implementation platform run_id helper
index=0 failed=0
while IFS= read -r entry; do
  index=$((index+1))
  set +e
  bash -c 'set -Eeuo pipefail; mint_case "$@"' _ "$entry" "$index" > "$pair_directory/mint-$index.log" 2>&1
  code=$?
  set -e
  if [[ "$code" != 0 ]]; then failed=$((failed+1)); fi
done < "$work/mints.jsonl"
[[ "$failed" == 0 ]]
}
{ main "$@"; exit; }
