#!/bin/sh
# A cell-local service: http://scripts:8080/cgi-bin/respond
set -eu
mkdir -p "$PROOFSTORM_OUTPUT/www/cgi-bin"
cat > "$PROOFSTORM_OUTPUT/www/cgi-bin/respond" <<'CGI'
#!/bin/sh
sleep "${RESPONSE_DELAY_SECONDS:-2}"
printf 'Status: 503 Service Unavailable\r\nContent-Type: text/plain\r\n\r\nSimulated outage\n'
CGI
chmod 700 "$PROOFSTORM_OUTPUT/www/cgi-bin/respond"
exec httpd -f -p 8080 -h "$PROOFSTORM_OUTPUT/www"
