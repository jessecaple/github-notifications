#!/bin/sh
# Installs GitHub Notifications for the current user, starts it at login,
# and (re)starts it now. Re-run after pulling changes to upgrade.
# `./install.sh --uninstall` removes everything this script and the app add.
set -eu

bin="$HOME/.local/bin/github-notifications"
data="${XDG_DATA_HOME:-$HOME/.local/share}"
config="${XDG_CONFIG_HOME:-$HOME/.config}"
entry="$data/applications/github-notifications.desktop"
autostart="$config/autostart/github-notifications.desktop"
icons="$data/icons/hicolor/scalable/status"

stop_apps() {
    # Linux process names are limited to 15 characters; match the full command.
    pkill -f -x "$bin" 2>/dev/null || true
    pkill -x tbgn 2>/dev/null || true
    # Wait for the old process to release its single-instance D-Bus name.
    attempts=0
    while pgrep -f -x "$bin" >/dev/null || pgrep -x tbgn >/dev/null; do
        attempts=$((attempts + 1))
        if [ "$attempts" -ge 50 ]; then
            echo "GitHub Notifications did not exit; try quitting it from the tray." >&2
            exit 1
        fi
        sleep 0.1
    done
}

remove_legacy_install() {
    rm -f "$HOME/.local/bin/tbgn" "$data/applications/tbgn.desktop" \
        "$config/autostart/tbgn.desktop" "$icons/tbgn-symbolic.svg" \
        "$icons/tbgn-unread-symbolic.svg"
}

if [ "${1:-}" = "--uninstall" ]; then
    stop_apps
    remove_legacy_install
    rm -f "$bin" "$entry" "$autostart" \
        "$icons/github-notifications-symbolic.svg" "$icons/github-notifications-unread.svg"
    echo "Uninstalled GitHub Notifications."
    exit 0
fi

cd "$(dirname "$0")"
cargo build --release --locked

# Leave the installed app running until the replacement has built successfully.
stop_apps
mkdir -p "$(dirname "$bin")" "$(dirname "$entry")" "$(dirname "$autostart")"
install -m 755 target/release/github-notifications "$bin"

desktop="[Desktop Entry]
Type=Application
Name=GitHub Notifications
GenericName=GitHub Notifications
Comment=GitHub inbox notifications in the top bar
Exec=$bin
Icon=github-notifications-symbolic
Terminal=false
Categories=Network;
X-GNOME-UsesNotifications=true
X-GNOME-Autostart-enabled=true"
printf '%s\n' "$desktop" > "$entry"
printf '%s\n' "$desktop" > "$autostart"
remove_legacy_install

nohup setsid -f "$bin" >/dev/null 2>&1 &
echo "Installed $bin and started it."
