#!/bin/sh
set -eu

if [ "$(uname -s)" != "Darwin" ]; then
  echo "install.sh must run on macOS" >&2
  exit 1
fi

command -v cargo >/dev/null 2>&1 || {
  echo "cargo is required" >&2
  exit 1
}

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
# PREFIX controls the install root; BIN_DIR = $PREFIX/bin.
# Default: $HOME/.local (user-local, no sudo). For system-wide: PREFIX=/usr/local ./scripts/install.sh
PREFIX="${PREFIX:-$HOME/.local}"
BIN_DIR="$PREFIX/bin"
CONFIG_DIR="$HOME/.config/rovr"
AGENT_DIR="$HOME/Library/LaunchAgents"
PLIST="$AGENT_DIR/com.rovr.daemon.plist"
UID_VALUE=$(id -u)
USER_VALUE=$(id -un)

cd "$ROOT"

# If caller wants to skip the build (e.g. already built), set SKIP_BUILD=1.
if [ "${SKIP_BUILD:-0}" != "1" ]; then
  echo "Building rovr (release)..."
  cargo build --release -p rovr-daemon -p rovr-cli
else
  echo "SKIP_BUILD=1 — using existing target/release binaries"
  if [ ! -f "$ROOT/target/release/rovrd" ] || [ ! -f "$ROOT/target/release/rovr" ]; then
    echo "target/release/rovr{,d} not found — cannot skip build" >&2
    exit 1
  fi
fi

# --- Stable code signature (TCC permanence) --------------------------------
# See install-dev.sh for rationale. We sign BOTH the build outputs and the
# installed copies so the TCC Designated Requirement survives clean/copy.
sign_binaries() {
  _src_rovrd="$1"
  _src_rovr="$2"
  SIGN_IDENTITY=$(security find-identity -v -p codesigning 2>/dev/null \
    | awk -F'"' '/rovr-dev/{print $2; exit}')
  if [ -z "$SIGN_IDENTITY" ]; then
    SIGN_IDENTITY=$(security find-identity -v -p codesigning 2>/dev/null \
      | awk -F'"' '/Apple Development/{print $2; exit}')
  fi
  if [ -n "$SIGN_IDENTITY" ]; then
    TEAM_ID=$(security find-certificate -c "$SIGN_IDENTITY" -p 2>/dev/null \
      | openssl x509 -noout -subject 2>/dev/null \
      | sed -n 's/.*OU *= *\([A-Z0-9]*\).*/\1/p')
    if [ -n "$TEAM_ID" ]; then
      REQ="designated => identifier \"com.rovr.rovrd\" and anchor apple generic and certificate leaf[subject.OU] = \"$TEAM_ID\""
      codesign --force --sign "$SIGN_IDENTITY" \
        --identifier com.rovr.rovrd -r="$REQ" "$_src_rovrd"
      REQ_CLI=$(echo "$REQ" | sed 's/com\.rovr\.rovrd/com.rovr.rovr/')
      codesign --force --sign "$SIGN_IDENTITY" \
        --identifier com.rovr.rovr -r="$REQ_CLI" "$_src_rovr"
    else
      codesign --force --sign "$SIGN_IDENTITY" \
        --identifier com.rovr.rovrd "$_src_rovrd"
      codesign --force --sign "$SIGN_IDENTITY" \
        --identifier com.rovr.rovr "$_src_rovr"
    fi
    echo "Signed with identity: $SIGN_IDENTITY"
  else
    echo "WARNING: no codesigning identity found; binaries left unsigned." >&2
    echo "         The Accessibility grant will break on every rebuild." >&2
    echo "         Create one: Keychain Access > Certificate Assistant >" >&2
    echo "         Create Certificate (name: rovr-dev, type: Code Signing)" >&2
  fi
}

sign_binaries "$ROOT/target/release/rovrd" "$ROOT/target/release/rovr"

mkdir -p "$BIN_DIR" "$CONFIG_DIR" "$AGENT_DIR"

# Replace any existing symlinks (dev install) or stale copies with real files.
rm -f "$BIN_DIR/rovr" "$BIN_DIR/rovrd"
# Use `install` so permissions/mtime are correct and the copy is independent
# of the build directory (survives `cargo clean` and repo moves).
install -m 755 "$ROOT/target/release/rovr" "$BIN_DIR/rovr"
install -m 755 "$ROOT/target/release/rovrd" "$BIN_DIR/rovrd"

# Re-sign the installed copies as well so TCC sees the stable prefix path.
# Signing the build outputs already covers the bytes, but this is idempotent
# and ensures the installed path has the correct identifier/DR even if the
# build-output signing was skipped.
sign_binaries "$BIN_DIR/rovrd" "$BIN_DIR/rovr"

# --- SA artifacts (for `rovr sa install` without env vars) -------------------
# Build and stage SA payload/loader/helper alongside the permanent binary.
# `find_sa_artifacts()` looks next to the binary and in $PREFIX/lib/rovr first,
# so a copied install doesn't need to hunt in target/build.
if [ "${SKIP_SA:-0}" != "1" ]; then
  echo "Building SA artifacts (release)..."
  if cargo build --release -p rovr-sa-payload -p rovr-sa-loader -p rovr-sa-helper 2>&1 | tail -n 5; then
    SA_LIB_DIR="$PREFIX/lib/rovr"
    mkdir -p "$SA_LIB_DIR"
    find_sa_artifact() {
      find "$ROOT/target/release/build" -path "*$1*/out/$2" -type f 2>/dev/null | sort | tail -n 1
    }
    SA_DYLIB=$(find_sa_artifact "rovr-sa-payload" "librovr_sa_payload.dylib" || true)
    SA_LOADER=$(find_sa_artifact "rovr-sa-loader" "rovr-sa-loader" || true)
    SA_HELPER=$(find_sa_artifact "rovr-sa-helper" "rovr-sa-helper" || true)
    if [ -n "$SA_DYLIB" ] && [ -n "$SA_LOADER" ] && [ -n "$SA_HELPER" ]; then
      install -m 644 "$SA_DYLIB" "$SA_LIB_DIR/librovr_sa_payload.dylib"
      install -m 755 "$SA_LOADER" "$SA_LIB_DIR/rovr-sa-loader"
      install -m 755 "$SA_HELPER" "$SA_LIB_DIR/rovr-sa-helper"
      echo "Staged SA artifacts into $SA_LIB_DIR"
    else
      echo "WARNING: SA artifacts not found after build — 'rovr sa install' will need ROVR_SA_* env vars" >&2
    fi
  else
    echo "WARNING: SA build failed — 'rovr sa install' will need manual cargo build" >&2
  fi
fi

if [ ! -f "$CONFIG_DIR/rovr.toml" ]; then
  cp "$ROOT/config/rovr.example.toml" "$CONFIG_DIR/rovr.toml"
  echo "Created default config at $CONFIG_DIR/rovr.toml"
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
    <string>$BIN_DIR/rovrd</string>
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

# (Re)load the LaunchAgent. bootout is safe if not loaded.
launchctl bootout "gui/$UID_VALUE/com.rovr.daemon" >/dev/null 2>&1 || true
sleep 1
if ! launchctl bootstrap "gui/$UID_VALUE" "$PLIST" 2>&1; then
  echo "bootstrap retry in 1s..." >&2
  sleep 1
  launchctl bootstrap "gui/$UID_VALUE" "$PLIST"
fi

echo "Installed rovr and rovrd into $BIN_DIR (permanent, copied — not symlinked)"
echo "LaunchAgent: $PLIST → $BIN_DIR/rovrd"
if ! echo ":$PATH:" | grep -q ":$BIN_DIR:"; then
  echo "NOTE: $BIN_DIR is not in PATH — add it to your shell rc:"
  echo "  export PATH=\"$BIN_DIR:\$PATH\""
fi
echo "Run: rovr doctor"
echo "To uninstall: ./scripts/uninstall.sh (or PREFIX=$PREFIX ./scripts/uninstall.sh)"
