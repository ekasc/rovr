#!/bin/sh
set -eu

if [ "$(uname -s)" != "Darwin" ]; then
  echo "install-dev.sh must run on macOS" >&2
  exit 1
fi

command -v cargo >/dev/null 2>&1 || {
  echo "cargo is required" >&2
  exit 1
}

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
BIN_DIR="$HOME/.local/bin"
CONFIG_DIR="$HOME/.config/rovr"
AGENT_DIR="$HOME/Library/LaunchAgents"
PLIST="$AGENT_DIR/com.rovr.daemon.plist"
UID_VALUE=$(id -u)
USER_VALUE=$(id -un)

cd "$ROOT"
cargo build --release -p rovr-daemon -p rovr-cli

mkdir -p "$BIN_DIR" "$CONFIG_DIR" "$AGENT_DIR"
ln -sf "$ROOT/target/release/rovr" "$BIN_DIR/rovr"
ln -sf "$ROOT/target/release/rovrd" "$BIN_DIR/rovrd"

if [ ! -f "$CONFIG_DIR/rovr.toml" ]; then
  cp "$ROOT/config/rovr.example.toml" "$CONFIG_DIR/rovr.toml"
fi

cat > "$PLIST" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>com.rovr.daemon</string>
  <key>ProgramArguments</key>
  <array>
    <string>$ROOT/target/release/rovrd</string>
    <string>--foreground</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>EnvironmentVariables</key>
  <dict>
    <key>HOME</key>
    <string>$HOME</string>
    <key>USER</key>
    <string>$USER_VALUE</string>
    <key>UID</key>
    <string>$UID_VALUE</string>
    <key>RUST_LOG</key>
    <string>rovr=info</string>
  </dict>
  <key>StandardOutPath</key>
  <string>/tmp/rovr.log</string>
  <key>StandardErrorPath</key>
  <string>/tmp/rovr.err.log</string>
</dict>
</plist>
PLIST

launchctl bootout "gui/$UID_VALUE/com.rovr.daemon" >/dev/null 2>&1 || true
launchctl bootstrap "gui/$UID_VALUE" "$PLIST"

echo "Installed rovr and rovrd into $BIN_DIR"
echo "LaunchAgent: $PLIST"
echo "Run: rovr doctor"
