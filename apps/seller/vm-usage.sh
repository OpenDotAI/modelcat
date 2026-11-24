#!/bin/sh
set -eu
dir="$HOME/.modelcat-monitor"
install -d -m 700 "$dir"
if [ -f "$dir/monitor.py" ] && printf '%s' "$1" | cmp -s "$dir/monitor.py" -; then
    systemctl is-active --quiet modelcat-monitor.service && exit 0
else
    tmp=$(mktemp "$dir/monitor.XXXXXXXX")
    printf '%s' "$1" > "$tmp"
    mv "$tmp" "$dir/monitor.py"
    sudo -n systemctl stop modelcat-monitor.service 2>/dev/null || true
fi
sudo -n systemd-run --quiet --collect --unit=modelcat-monitor --uid="$(id -u)" \
    --setenv="HOME=$HOME" --property=Restart=on-failure --property=RestartSec=5 \
    /usr/bin/python3 "$dir/monitor.py" collect
