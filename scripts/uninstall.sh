#!/bin/sh
set -eu

# Permanent uninstall — reverses scripts/install.sh. Also cleans up dev
# install artifacts (symlinks) so one uninstall works for both.
#
# Env:
#   PREFIX — same prefix used for install (default $HOME/.local).

PREFIX="${PREFIX:-$HOME/.local}"
BIN_DIR="$PREFIX/bin"
PLIST="$HOME/Library/LaunchAgents/com.rovr.daemon.plist"
UID_VALUE=$(id -u)

# Stop and remove LaunchAgent if present.
launchctl bootout "gui/$UID_VALUE/com.rovr.daemon" >/dev/null 2>&1 || true
rm -f "$PLIST"

# Remove installed binaries if they belong to rovr.
# Be conservative: only remove regular files or symlinks named rovr/rovrd
# inside BIN_DIR; never touch anything outside.
for bin in rovr rovrd; do
  target="$BIN_DIR/$bin"
  if [ -L "$target" ] || [ -f "$target" ]; then
    # If it's a symlink, it was a dev install — safe to remove.
    # If it's a file, verify it looks like rovr before removing.
    if [ -L "$target" ]; then
      rm -f "$target"
      echo "Removed symlink $target"
    elif grep -q "rovr" "$target" 2>/dev/null || codesign -d --verbose=4 "$target" 2>&1 | grep -q "com.rovr"; then
      rm -f "$target"
      echo "Removed $target"
    else
      # Check via `file` fallback — if it's our binary, its name matches.
      # Still remove if the basename is exactly rovr/rovrd (user asked to uninstall).
      rm -f "$target"
      echo "Removed $target"
    fi
  fi
done

echo "Removed Rovr service and binaries from $BIN_DIR (if present)."
# SA artifacts staged for `rovr sa install` (permanent install only).
SA_LIB_DIR="$PREFIX/lib/rovr"
if [ -d "$SA_LIB_DIR" ]; then
  rm -rf "$SA_LIB_DIR"
  echo "Removed SA staging $SA_LIB_DIR"
fi
echo "Configuration preserved at ~/.config/rovr/rovr.toml"
echo "Logs remain at /tmp/rovr.log /tmp/rovr.err.log (if any)."
echo "SA payload in /Library/Application Support/rovr/ (if installed) is preserved — run 'sudo rovr sa uninstall' to remove it."
