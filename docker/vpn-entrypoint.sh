#!/usr/bin/env bash
# S6 long-lived service: waits for NordVPN, then runs microsocks.

SUBNET="${WHITELIST_SUBNET:-172.20.0.0/16}"
SOCKS_PORT="${SOCKS_PORT:-1080}"

echo "houdinny: waiting for NordVPN to connect..."
for i in $(seq 1 60); do
    if nordvpn status 2>/dev/null | grep -qi "status: connected"; then
        echo "houdinny: VPN connected!"
        sleep 2
        break
    fi
    sleep 2
done

if nordvpn status 2>/dev/null | grep -qi "status: connected"; then
    for attempt in 1 2 3; do
        nordvpn whitelist add subnet "$SUBNET" 2>/dev/null && break
        sleep 2
    done
    echo "houdinny: whitelisted subnet $SUBNET"
    echo "houdinny: starting SOCKS5 proxy on :$SOCKS_PORT"
    # exec replaces this process — S6 monitors it
    exec microsocks -p "$SOCKS_PORT" -b 0.0.0.0
else
    echo "houdinny: WARNING — VPN did not connect"
    # Sleep forever so S6 doesn't restart the service in a loop
    exec sleep infinity
fi
