#!/bin/sh
set -eu

UID_VALUE=$(id -u)
PLIST="$HOME/Library/LaunchAgents/com.rovr.daemon.plist"
launchctl bootout "gui/$UID_VALUE/com.rovr.daemon" >/dev/null 2>&1 || true
rm -f "$PLIST" "$HOME/.local/bin/rovr" "$HOME/.local/bin/rovrd"
echo "Removed Rovr development service and symlinks. Configuration was preserved."
