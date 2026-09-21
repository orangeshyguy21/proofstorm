#!/usr/bin/env bash
# Explicit-image, disposable regtest compatibility. No existing runtime adoption.
set -Eeuo pipefail
# Parse the complete runner before executing; edits to the checkout cannot alter
# the tail of a long-running shell program after its cases have already passed.
main() {
umask 077
[[ $# == 3 ]] || { echo 'Usage: bitcoin-lightning.sh MATRIX.json linux/arm64|linux/amd64 NEW_WORK' >&2; exit 2; }
input=$1 platform=$2 work=$3
case "$platform" in linux/arm64|linux/amd64) ;; *) exit 2 ;; esac
[[ "$work" == /* && ! -e "$work" ]] || { echo 'Evidence directory must be new and absolute' >&2; exit 2; }
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)
helper="$root/target/check/debug/proofstorm-xtask"
[[ -x "$helper" ]] || { echo 'Build proofstorm-xtask first' >&2; exit 2; }
jq -e '.bitcoin|length>0' "$input" >/dev/null
jq -e '.lightning|length>0' "$input" >/dev/null
mkdir -p "$work"
cp "$input" "$work/input.json"
cp "${BASH_SOURCE[0]}" "$work/bitcoin-lightning-runner.sh"
cp "$root/tests/component-compat/nutshell.sh" "$work/nutshell-runner.sh"
cp "$root/tests/component-compat/cdk-wallet.sh" "$work/cdk-wallet-runner.sh"
cp "$root/crates/proofstorm-kube/tests/golden/nutshell.json" "$work/nutshell-render.json"
cp "$root/crates/proofstorm-kube/tests/golden/nutshell-cln-cell.json" "$work/nutshell-cln-render.json"
run_id="compat-$(date +%s)-$(openssl rand -hex 4)"
printf '%s\n' "$run_id" > "$work/run-id"
# Resolve tags once, check architecture, and thereafter execute only image IDs.
for kind in bitcoin lightning mints wallets; do
  while IFS= read -r entry; do
    ref=$(jq -r '.image' <<< "$entry")
    if [[ "$ref" == *@sha256:* ]] || ! docker image inspect "$ref" >/dev/null 2>&1; then docker pull --platform "$platform" "$ref" >/dev/null; fi
    metadata=$(docker image inspect "$ref")
    jq -e --arg arch "${platform#linux/}" '.[0].Os=="linux" and .[0].Architecture==$arch' <<< "$metadata" >/dev/null
    image_id=$(jq -er '.[0].Id' <<< "$metadata")
    jq -c --arg id "$image_id" '. + {image_id:$id}' <<< "$entry" >> "$work/$kind.jsonl"
  done < <(jq -c ".${kind}[]?" "$input")
done
if [[ -s "$work/mints.jsonl" ]]; then
  driver=$(jq -er '.driver' "$input")
  docker image inspect "$driver" > "$work/driver-inspect.json"
  jq -e --arg arch "${platform#linux/}" '.[0].Os=="linux" and .[0].Architecture==$arch' "$work/driver-inspect.json" >/dev/null
  driver_id=$(jq -er '.[0].Id' "$work/driver-inspect.json")
  "$helper" release-run 30 docker run --rm --label "dev.proofstorm.compat.run=$run_id" --platform "$platform" --network none --read-only --cap-drop ALL --entrypoint cat "$driver_id" /proofstorm-driver > "$work/driver"
  chmod 755 "$work/driver"
fi
cp "$root/crates/proofstorm-kube/tests/golden/bitcoin-core.json" "$work/bitcoin-render.json"
cp "$root/crates/proofstorm-kube/tests/golden/lnd.json" "$work/lnd-render.json"
cp "$root/crates/proofstorm-kube/tests/golden/cln.json" "$work/cln-render.json"

# ShellCheck 0.9 cannot follow exported calls into independent Bash cases.
# shellcheck disable=SC2317,SC2329
pair() (
  set -Eeuo pipefail
  btc=$1 ln=$2 index=$3
  implementation=$(jq -er '.implementation' <<< "$ln")
  case "$implementation" in lnd|cln) ;; *) echo 'Unknown Lightning implementation' >&2; exit 2 ;; esac
  directory="$work/case-$index"
  mkdir "$directory"
  jq -n --argjson bitcoin "$btc" --argjson lightning "$ln" --arg platform "$platform" '{bitcoin:$bitcoin,lightning:$lightning,platform:$platform}' > "$directory/selection.json"
  prefix="$run_id-$index" network='' containers=('') volumes=('') stage=setup
  btc_image=$(jq -r '.image_id' <<< "$btc")
  ln_image=$(jq -r '.image_id' <<< "$ln")
  chain="$prefix-chain" north="$prefix-north" south="$prefix-south"
  # shellcheck disable=SC2329 # Invoked by the EXIT trap in this case subprocess.
  cleanup() {
    result=$?; trap - EXIT INT TERM
    cleanup_ok=true
    for name in "${containers[@]}"; do
      [[ -n "$name" ]] || continue
      docker logs "$name" > "$directory/${name##*-}.log" 2>&1 || true
      owner=$(docker inspect --format '{{index .Config.Labels "dev.proofstorm.compat.run"}}' "$name" 2>/dev/null) || continue
      # Upstream LND/CLN images declare /root data volumes even though this
      # fixture mounts its named data elsewhere. Remove those anonymous volumes
      # with their owned container; named volumes are verified separately below.
      if [[ "$owner" == "$run_id" ]]; then docker rm --force --volumes "$name" >/dev/null || cleanup_ok=false; else cleanup_ok=false; fi
    done
    for name in "${volumes[@]}"; do
      [[ -n "$name" ]] || continue
      owner=$(docker volume inspect --format '{{index .Labels "dev.proofstorm.compat.run"}}' "$name" 2>/dev/null) || continue
      if [[ "$owner" == "$run_id" ]]; then docker volume rm "$name" >/dev/null || cleanup_ok=false; else cleanup_ok=false; fi
    done
    if [[ -n "$network" ]]; then
      owner=$(docker network inspect --format '{{index .Labels "dev.proofstorm.compat.run"}}' "$network" 2>/dev/null) || owner=''
      if [[ "$owner" == "$run_id" ]]; then docker network rm "$network" >/dev/null || cleanup_ok=false; else cleanup_ok=false; fi
    fi
    [[ "$cleanup_ok" == true ]] || result=1
    jq --arg stage "$stage" --argjson code "$result" --argjson cleanup "$cleanup_ok" '. + {passed:($code==0),exit_code:$code,last_stage:$stage,cleanup_verified:$cleanup}' "$directory/selection.json" > "$directory/result.json"
    exit "$result"
  }
  trap cleanup EXIT
  trap 'exit 130' INT TERM
  run() { "$helper" release-run 40 docker "$@"; }
  rpc() { run exec "$chain" bitcoin-cli -regtest -rpcuser=proofstorm -rpcpassword=proofstorm-regtest-only "$@"; }
  node() {
    local container=$1; shift
    if [[ "$implementation" == lnd ]]; then
      run exec "$container" lncli --lnddir=/home/lnd/.lnd --network=regtest "$@"
    else
      run exec "$container" lightning-cli --notifications=none --lightning-dir=/home/cln/.lightning --network=regtest "$@"
    fi
  }
  await_json() {
    local filter=$1; shift
    local end=$((SECONDS + 150))
    while ((SECONDS < end)); do
      if "$@" > "$directory/poll.json" 2> "$directory/poll.stderr" && jq -e "$filter" "$directory/poll.json" >/dev/null; then return 0; fi
      sleep 1
    done
    echo "Timed out at $stage: $filter" >&2; return 1
  }
  mine() { rpc -rpcwallet=miner generatetoaddress "$1" "$mining_address" > /dev/null; }
  network=$prefix
  docker network create --internal --label "dev.proofstorm.compat.run=$run_id" "$network" > /dev/null
  for component in chain north south; do
    volume="$prefix-$component-data"; volumes+=("$volume")
    docker volume create --label "dev.proofstorm.compat.run=$run_id" "$volume" >/dev/null
    docker run --rm --label "dev.proofstorm.compat.run=$run_id" --platform "$platform" --network none --user 0:0 --cap-drop ALL --cap-add CHOWN \
      --security-opt no-new-privileges --volume "$volume:/state" --entrypoint sh "$btc_image" -ec 'chown 1000:1000 /state'
  done
  args=()
  while IFS= read -r argument; do args+=("$argument"); done < <(jq -r '.resources.statefulSets[0].spec.template.spec.containers[0].args[]' "$work/bitcoin-render.json")
  containers+=("$chain")
  docker run -d --platform "$platform" --name "$chain" --hostname chain --network "$network" --network-alias chain \
    --label "dev.proofstorm.compat.run=$run_id" --user 1000:1000 --cap-drop ALL --security-opt no-new-privileges \
    --volume "$prefix-chain-data:/home/bitcoin/.bitcoin" --entrypoint bitcoind "$btc_image" "${args[@]}" >/dev/null
  stage=bitcoin-startup
  await_json '.chain=="regtest"' rpc getblockchaininfo
  rpc getnetworkinfo > "$directory/bitcoin-version.json"
  jq -e --arg version "$(jq -r '.version' <<< "$btc")" '.subversion==("/Satoshi:"+$version+".0/") or .subversion==("/Satoshi:"+$version+"/")' "$directory/bitcoin-version.json" >/dev/null
  rpc createwallet miner > /dev/null
  mining_address=$(rpc -rpcwallet=miner getnewaddress)
  mine 110
  stage=lightning-startup
  for component in north south; do
    args=()
    while IFS= read -r argument; do args+=("${argument//lightning/$component}"); done < <(jq -r '.resources.statefulSets[0].spec.template.spec.containers[0].args[]' "$work/$implementation-render.json")
    if [[ "$implementation" == lnd ]]; then entrypoint=lnd; data=/home/lnd/.lnd; else entrypoint=lightningd; data=/home/cln/.lightning; fi
    # Replace only rendered service names, preserving the native data path.
    if [[ "$implementation" == cln ]]; then args[0]='--lightning-dir=/home/cln/.lightning'; fi
    name="$prefix-$component"; containers+=("$name")
    docker run -d --platform "$platform" --name "$name" --hostname "$component" --network "$network" --network-alias "$component" \
      --label "dev.proofstorm.compat.run=$run_id" --user 1000:1000 --cap-drop ALL --security-opt no-new-privileges \
      --volume "$prefix-$component-data:$data" --entrypoint "$entrypoint" "$ln_image" "${args[@]}" >/dev/null
    if [[ "$implementation" == lnd ]]; then
      await_json '.synced_to_chain==true' node "$name" getinfo
    else
      await_json '.blockheight==110' node "$name" getinfo
    fi
    node "$name" getinfo > "$directory/$component-before.json"
    expected=$(jq -r '.version' <<< "$ln")
    if [[ "$implementation" == lnd ]]; then
      jq -e --arg expected "$expected" '.version|split(" ")[0]==$expected' "$directory/$component-before.json" >/dev/null
    else
      jq -e --arg expected "v$expected" '.version==$expected' "$directory/$component-before.json" >/dev/null
    fi
  done
  stage=funding
  if [[ "$implementation" == lnd ]]; then
    node "$north" newaddress p2wkh > "$directory/funding-address.json"
    address=$(jq -er '.address' "$directory/funding-address.json")
  else
    node "$north" newaddr bech32 > "$directory/funding-address.json"
    address=$(jq -er '.bech32' "$directory/funding-address.json")
  fi
  rpc -rpcwallet=miner sendtoaddress "$address" 1 > "$directory/funding-txid.txt"
  mine 6
  if [[ "$implementation" == lnd ]]; then
    await_json '.confirmed_balance|tonumber>=1000000' node "$north" walletbalance
    peer=$(jq -r '.identity_pubkey' "$directory/south-before.json")
    node "$north" connect "$peer@south:9735" >/dev/null
    node "$north" openchannel --node_key="$peer" --local_amt=1000000 > "$directory/channel-open.json"
  else
    await_json '[.outputs[]|select(.status=="confirmed")]|length>0' node "$north" listfunds
    peer=$(jq -r '.id' "$directory/south-before.json")
    node "$north" connect "$peer" south 9735 >/dev/null
    node "$north" fundchannel "$peer" 1000000 > "$directory/channel-open.json"
  fi
  mine 6
  stage=channel-ready
  if [[ "$implementation" == lnd ]]; then
    await_json '.channels|any(.active==true)' node "$north" listchannels
    await_json '.channels|any(.active==true)' node "$south" listchannels
    await_json '.edges|any(.node1_policy!=null and .node2_policy!=null)' node "$north" describegraph --include_unannounced
  else
    await_json '.channels|any(.state=="CHANNELD_NORMAL")' node "$north" listpeerchannels
    await_json '.channels|any(.state=="CHANNELD_NORMAL")' node "$south" listpeerchannels
  fi
  pay() {
    local payer=$1 recipient=$2 label=$3
    if [[ "$implementation" == lnd ]]; then
      node "$recipient" addinvoice --amt=1000 --memo="$label" > "$directory/$label-invoice.json"
      invoice=$(jq -r '.payment_request' "$directory/$label-invoice.json")
      node "$payer" sendpayment --force --json --timeout=30s --pay_req="$invoice" > "$directory/$label-payment.json"
      jq -e '.status=="SUCCEEDED" or (.payment_error=="" and (.payment_preimage|length>0))' "$directory/$label-payment.json" >/dev/null
      await_json '.invoices|any(.memo=="'"$label"'" and .settled==true and (.amt_paid_sat|tonumber)==1000)' node "$recipient" listinvoices
    else
      node "$recipient" invoice 1000000 "$label" compatibility > "$directory/$label-invoice.json"
      invoice=$(jq -r '.bolt11' "$directory/$label-invoice.json")
      node "$payer" "$label" "$invoice" > "$directory/$label-payment.json"
      await_json '.invoices|any(.status=="paid" and .amount_received_msat==1000000)' node "$recipient" listinvoices "$label"
    fi
    cp "$directory/poll.json" "$directory/$label-settlement.json"
  }
  stage=payment
  pay "$north" "$south" pay
  if [[ "$implementation" == cln ]]; then pay "$north" "$south" xpay; fi
  stage=restart
  height=$(rpc getblockcount)
  docker restart --time 30 "$chain" "$north" > /dev/null
  await_json ".blocks==$height" rpc getblockchaininfo
  await_json 'type=="object"' node "$north" getinfo
  node "$north" getinfo > "$directory/north-after.json"
  if [[ "$implementation" == lnd ]]; then
    test "$(jq -r '.identity_pubkey' "$directory/north-before.json")" = "$(jq -r '.identity_pubkey' "$directory/north-after.json")"
    node "$north" connect "$peer@south:9735" >/dev/null 2>&1 || true
    await_json '.channels|any(.active==true)' node "$north" listchannels
    pay "$north" "$south" after-restart
  else
    test "$(jq -r '.id' "$directory/north-before.json")" = "$(jq -r '.id' "$directory/north-after.json")"
    node "$north" connect "$peer" south 9735 >/dev/null 2>&1 || true
    await_json '.channels|any(.state=="CHANNELD_NORMAL" and .peer_connected==true)' node "$north" listpeerchannels
    # xpay is the Nutshell 0.21 payment contract; preserve its independent label.
    node "$south" invoice 1000000 restart compatibility > "$directory/restart-invoice.json"
    invoice=$(jq -r '.bolt11' "$directory/restart-invoice.json")
    node "$north" xpay "$invoice" > "$directory/restart-payment.json"
    await_json '.invoices|any(.status=="paid" and .amount_received_msat==1000000)' node "$south" listinvoices restart
    cp "$directory/poll.json" "$directory/restart-settlement.json"
  fi
  if [[ -s "$work/mints.jsonl" ]]; then
    stage=mint-wallet-compatibility
    bash "$work/nutshell-runner.sh" "$work" "$directory" "$prefix" "$implementation" "$platform" "$run_id" "$helper"
  fi
  stage=complete
)
index=0 failed=0
while IFS= read -r btc; do
  while IFS= read -r ln; do
    index=$((index+1))
    printf 'Compatibility %s: Bitcoin %s / %s %s\n' "$index" "$(jq -r '.version' <<< "$btc")" "$(jq -r '.implementation' <<< "$ln")" "$(jq -r '.version' <<< "$ln")"
    # A separate Bash process retains errexit inside the case function.
    export -f pair
    export work run_id platform helper root
    set +e
    bash -c 'set -Eeuo pipefail; pair "$@"' _ "$btc" "$ln" "$index" > "$work/case-$index.log" 2>&1
    result=$?
    set -e
    if [[ "$result" != 0 ]]; then failed=$((failed+1)); printf 'FAILED: %s/case-%s.log\n' "$work" "$index"; else printf 'Passed\n'; fi
  done < "$work/lightning.jsonl"
done < "$work/bitcoin.jsonl"
jq -s --arg platform "$platform" --argjson expected "$index" --argjson failures "$failed" \
  '{platform:$platform,expected_cases:$expected,completed_cases:length,cases:.,passed:($failures==0 and length==$expected and all(.passed and .cleanup_verified)),scope:"Explicit Bitcoin/Lightning pairings and optional Nutshell wallet contracts; not complete product qualification"}' \
  "$work"/case-*/result.json > "$work/result.json"
[[ "$failed" == 0 ]]
}
{ main "$@"; exit; }
