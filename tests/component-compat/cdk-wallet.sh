#!/usr/bin/env bash
# A native CDK CLI contract inside a wallet owned by the Nutshell matrix runner.
set -Eeuo pipefail
main() {
umask 077
[[ $# == 7 ]] || exit 2
wallet_name=$1 mint_name=$2 directory=$3 version=$4 prefix=$5 implementation=$6 helper=$7
case "$version" in 0.18.0) ;; *) exit 2 ;; esac
run() { "$helper" release-run 60 docker "$@"; }
node() {
  if [[ "$implementation" == lnd ]]; then run exec "$prefix-south" lncli --lnddir=/home/lnd/.lnd --network=regtest "$@"
  else run exec "$prefix-south" lightning-cli --notifications=none --lightning-dir=/home/cln/.lightning --network=regtest "$@"; fi
}
cli() {
  local user=$1; shift
  run exec "$wallet_name" cdk-cli --work-dir "/wallet/$user/cdk" --unit sat --non-interactive "$@"
}
balance() {
  cli "$1" balance > "$directory/$1-balance.log" 2>&1
  awk -v url="http://$mint_name:3338" '$2==url && $4=="sat" {print $3; found=1} END {if (!found) print 0}' "$directory/$1-balance.log"
}
run exec "$wallet_name" cdk-cli --version > "$directory/version.txt"
grep -Fx "cdk-cli $version" "$directory/version.txt" >/dev/null
[[ $(balance alice) == 0 && $(balance bob) == 0 ]]
run exec "$wallet_name" sha256sum /wallet/alice/cdk/seed > "$directory/identity-before.txt"
# Let the real unpaid quote persist and the bounded native wait expire. Its exact
# identity is checked independently before funding, then resumed explicitly.
cli alice mint "http://$mint_name:3338" 10000 --wait-duration 1 > "$directory/quote.log" 2>&1 || true
run exec "$wallet_name" /opt/proofstorm/driver cdk-quote await UNPAID /wallet/alice/cdk/cdk-cli.sqlite "http://$mint_name:3338" 10000 >/dev/null
invoice=$(run exec "$wallet_name" /opt/proofstorm/driver cdk-quote invoice UNPAID /wallet/alice/cdk/cdk-cli.sqlite "http://$mint_name:3338" 10000)
quote=$(run exec "$wallet_name" /opt/proofstorm/driver cdk-quote id UNPAID /wallet/alice/cdk/cdk-cli.sqlite "http://$mint_name:3338" 10000)
if [[ "$implementation" == lnd ]]; then
  node sendpayment --force --json --timeout=30s --pay_req="$invoice" > "$directory/funding.json"
  jq -e '.status=="SUCCEEDED"' "$directory/funding.json" >/dev/null
else
  node pay "$invoice" > "$directory/funding.json"
  jq -e '.status=="complete"' "$directory/funding.json" >/dev/null
fi
cli alice mint "http://$mint_name:3338" --quote-id "$quote" --wait-duration 10 > "$directory/claim.log" 2>&1
[[ $(balance alice) == 10000 && $(balance bob) == 0 ]]
cli alice send --mint-url "http://$mint_name:3338" --amount 1000 > "$directory/send.log" 2>&1
token=$(grep -Eo 'cashu[AB][A-Za-z0-9_=-]+' "$directory/send.log")
[[ -n "$token" && "$token" != *$'\n'* ]]
cli bob receive --allow-untrusted "$token" > "$directory/receive.log" 2>&1
bob_balance=$(balance bob)
[[ "$bob_balance" == 999 ]]
label="cdk-melt-$wallet_name"
if [[ "$implementation" == lnd ]]; then
  node addinvoice --amt=500 --memo="$label" > "$directory/melt-invoice.json"
  invoice=$(jq -er '.payment_request' "$directory/melt-invoice.json")
else
  node invoice 500000 "$label" compatibility > "$directory/melt-invoice.json"
  invoice=$(jq -er '.bolt11' "$directory/melt-invoice.json")
fi
# Keep the native output in private container storage for the real receipt parser.
# shellcheck disable=SC2016 # Positional arguments expand only inside the container.
run exec "$wallet_name" sh -ec 'cdk-cli --work-dir /wallet/alice/cdk --unit sat --non-interactive melt --mint-url "$1" --invoice "$2" > /wallet/melt.log 2>&1' _ "http://$mint_name:3338" "$invoice"
run exec "$wallet_name" /opt/proofstorm/driver cdk-melt-receipt /wallet/melt.log > "$directory/melt-receipt.json"
jq -e '.state=="PAID" and .amount_sat==500' "$directory/melt-receipt.json" >/dev/null
if [[ "$implementation" == lnd ]]; then
  node listinvoices > "$directory/settlement.json"
  jq -e --arg label "$label" '.invoices|any(.memo==$label and .settled==true and (.amt_paid_sat|tonumber)==500)' "$directory/settlement.json" >/dev/null
else
  node listinvoices "$label" > "$directory/settlement.json"
  jq -e '.invoices|any(.status=="paid" and .amount_received_msat==500000)' "$directory/settlement.json" >/dev/null
fi
before=$(balance alice)
# CDK's preparation swap adds one sat to the Nutshell wallet's total input fees.
[[ "$before" == 8498 ]]
run restart --time 30 "$wallet_name" "$mint_name" >/dev/null
for ((attempt=0; attempt<90; attempt++)); do
  if run exec "$mint_name" /opt/proofstorm/driver ready nutshell http://127.0.0.1:3338/v1/info >/dev/null 2>&1; then break; fi
  sleep 1
done
run exec "$mint_name" /opt/proofstorm/driver ready nutshell http://127.0.0.1:3338/v1/info
[[ $(balance alice) == "$before" && $(balance bob) == "$bob_balance" ]]
run exec "$wallet_name" sha256sum /wallet/alice/cdk/seed > "$directory/identity-after.txt"
cmp "$directory/identity-before.txt" "$directory/identity-after.txt"
jq -n --slurpfile wallet "$directory/selection.json" --argjson alice "$before" --argjson bob "$bob_balance" \
  '{passed:true,wallet:$wallet[0],minted_sat:10000,invoice_settled_sat:500,alice_balance_sat:$alice,bob_balance_sat:$bob,input_fees_sat:(10000-500-$alice-$bob),restart_preserved_balances:true,restart_preserved_identity:true}' > "$directory/result.json"
}
{ main "$@"; exit; }
