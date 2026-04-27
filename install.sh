#!/bin/bash

# --- root check ---
if [ "$EUID" -ne 0 ]; then
    echo "please run this script as root"
    exit 1
fi

echo "building inertia"
cargo build --release

echo "listing input devices"
DEVICE_NAMES=()
while IFS= read -r line; do
    name=$(echo "$line" | sed 's/N: Name="//; s/"$//')
    DEVICE_NAMES+=("$name")
done < <(grep '^N: Name=' /proc/bus/input/devices)

echo "available devices:"
for i in "${!DEVICE_NAMES[@]}"; do
    echo "[$i] ${DEVICE_NAMES[$i]}"
done

echo -n "select device index: "
read INDEX

SELECTED_DEVICE="${DEVICE_NAMES[$INDEX]}"

if [ -z "$SELECTED_DEVICE" ]; then
    echo "invalid selection"
    exit 1
fi

echo "selected device: $SELECTED_DEVICE"

echo "copying inertia to /usr/local/bin/"
cp target/release/inertia /usr/local/bin/

echo "copying config to /etc/inertia/"
mkdir -p /etc/inertia
cp config.toml /etc/inertia/

echo "writing device into config"
sed -i "s|^device_name = .*|device_name = \"$SELECTED_DEVICE\"|" /etc/inertia/config.toml

echo "copying systemd service file to /etc/systemd/system/"
cp inertia.service /etc/systemd/system/

echo "enabling and starting inertia"
systemctl daemon-reexec
systemctl daemon-reload
systemctl enable inertia
systemctl start inertia

echo "checking if inertia is running"
sleep 1

if systemctl is-active --quiet inertia; then
    echo "inertia is running"
else
    echo "inertia failed to start"
    systemctl status inertia --no-pager
    exit 1
fi

echo "installer finished"
