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

# --- Stable code signature (TCC permanence) --------------------------------
# macOS Accessibility (TCC) grants are keyed to a binary's code signature.
# Unsigned/adhoc binaries change cdhash on EVERY rebuild, silently voiding
# the grant. Signing with a stable identity makes the Designated Requirement
# cert-based, so one grant survives all future rebuilds.
#   Preferred: a dedicated self-signed "rovr-dev" codesigning identity.
#   Fallback:  any Apple Development identity already in the keychain.
SIGN_IDENTITY=$(security find-identity -v -p codesigning 2>/dev/null \
  | awk -F'"' '/rovr-dev/{print $2; exit}')
if [ -z "$SIGN_IDENTITY" ]; then
  SIGN_IDENTITY=$(security find-identity -v -p codesigning 2>/dev/null \
    | awk -F'"' '/Apple Development/{print $2; exit}')
fi
if [ -n "$SIGN_IDENTITY" ]; then
  # Explicit cert-anchored Designated Requirement: TCC matches on identifier
  # + Apple anchor + team OU, NOT the per-build cdhash, so ONE Accessibility
  # grant survives every rebuild. (Plain `codesign --sign` pins the DR to
  # the cdhash, which would void the grant on each rebuild — verified.)
  TEAM_ID=$(security find-certificate -c "$SIGN_IDENTITY" -p 2>/dev/null \
    | openssl x509 -noout -subject 2>/dev/null \
    | sed -n 's/.*OU *= *\([A-Z0-9]*\).*/\1/p')
  if [ -n "$TEAM_ID" ]; then
    REQ="designated => identifier \"com.rovr.rovrd\" and anchor apple generic and certificate leaf[subject.OU] = \"$TEAM_ID\""
    codesign --force --sign "$SIGN_IDENTITY" \
      --identifier com.rovr.rovrd -r="$REQ" target/release/rovrd
    REQ_CLI=$(echo "$REQ" | sed 's/com\.rovr\.rovrd/com.rovr.rovr/')
    codesign --force --sign "$SIGN_IDENTITY" \
      --identifier com.rovr.rovr -r="$REQ_CLI" target/release/rovr
  else
    codesign --force --sign "$SIGN_IDENTITY" \
      --identifier com.rovr.rovrd target/release/rovrd
    codesign --force --sign "$SIGN_IDENTITY" \
      --identifier com.rovr.rovr target/release/rovr
  fi
else
  echo "WARNING: no codesigning identity found; binaries left unsigned." >&2
  echo "         The Accessibility grant will break on every rebuild." >&2
  echo "         Create one: Keychain Access > Certificate Assistant >" >&2
  echo "         Create Certificate (name: rovr-dev, type: Code Signing)" >&2
fi

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
