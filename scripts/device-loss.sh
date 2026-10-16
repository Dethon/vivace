#!/bin/bash
# Checks that audio survives its output device going away (Linux, needs the
# `pulseaudio` binary). Runs the device_loss example against a private
# PulseAudio server with one null sink, which the example has restarted to
# stand for a device being replaced. Plays nothing audible.
#
#   scripts/device-loss.sh [paused|playing]...    (default: both)
set -u

if [ "${1:-}" = restart ]; then
    # Called by the example: kill the server and start a new one on the same socket.
    dir=$2
    pkill -f "pulseaudio.*$dir" 2>/dev/null
    for _ in $(seq 50); do pgrep -f "pulseaudio.*$dir" >/dev/null || break; sleep 0.1; done
    [ "${3:-}" = stop ] && exit 0
    rm -f "$dir/socket"
    HOME=$dir XDG_RUNTIME_DIR=$dir PULSE_RUNTIME_PATH=$dir setsid pulseaudio -n -F "$dir/server.pa" \
        --exit-idle-time=-1 --daemonize=no --use-pid-file=no </dev/null >/dev/null 2>&1 &
    for _ in $(seq 50); do [ -S "$dir/socket" ] && break; sleep 0.1; done
    exit 0
fi

root=$(cd "$(dirname "$0")/.." && pwd)
script=$root/scripts/device-loss.sh
cargo build --release --quiet --manifest-path "$root/Cargo.toml" -p vivace-core --example device_loss || exit 1
dir=$(mktemp -d)
trap '"$script" restart "$dir" stop; rm -rf "$dir"' EXIT
cat >"$dir/server.pa" <<EOF
load-module module-native-protocol-unix socket=$dir/socket auth-anonymous=1
load-module module-null-sink sink_name=device_loss
set-default-sink device_loss
EOF

status=0
for scenario in "${@:-paused playing}"; do
    for each in $scenario; do
        "$script" restart "$dir"
        PULSE_SERVER=unix:$dir/socket DEVICE_LOSS_REPLACE="'$script' restart '$dir'" \
            timeout 30 "$root/target/release/examples/device_loss" "$each" 2>/dev/null || status=1
    done
done
exit $status
