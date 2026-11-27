#!/bin/bash
# DisplayAnything macOS setup: run after installing or updating a build.
#
#   bash DisplayAnythingInstall.sh                      # fix the copy in /Applications
#   bash DisplayAnythingInstall.sh ~/Downloads/DisplayAnything-macos.zip   # install from a zip first
#   bash DisplayAnythingInstall.sh ~/Downloads/DisplayAnything.app         # install from an .app first
#   bash DisplayAnythingInstall.sh --reset-permissions [zip or app]         # also clear the permissions
#
# What it does:
#   1. Quits DisplayAnything if it is running.
#   2. Installs the given .zip or .app into /Applications (optional).
#   3. Removes the "downloaded from the internet" quarantine flag, so macOS opens it.
#   4. Re-signs it locally. If this Mac has an Apple code-signing certificate
#      in its keychain ("Developer ID Application" or "Apple Development"), it
#      signs with that, so every build is the same app to macOS and keeps its
#      Accessibility, Input Monitoring and Screen Recording permissions.
#      Without one it signs ad hoc, and each build counts as a new app.
#   5. Clears the old permission grants only when they can't carry over: when
#      signing ad hoc, the first time a certificate is used, or with
#      --reset-permissions. (A stale grant looks switched on but does nothing.)
#   6. Opens DisplayAnything, and the Settings pages if permissions were cleared.
#
# Set DA_SIGN_IDENTITY to choose a certificate (its name or SHA-1), or to "-"
# to force ad hoc signing.
set -euo pipefail

APP="/Applications/DisplayAnything.app"
BUNDLE_ID="com.displayanything.app"
RESET=0
if [[ "${1:-}" == "--reset-permissions" ]]; then
  RESET=1
  shift
fi
SRC="${1:-}"
# Remembers which certificate signed the last install, so the permissions
# are cleared once when it changes and kept after that.
STATE_DIR="$HOME/Library/Application Support/DisplayAnything Installer"
STATE_FILE="$STATE_DIR/signed-with"

say() { printf '\n==> %s\n' "$*"; }

say "Quitting DisplayAnything if it is running"
osascript -e "tell application id \"$BUNDLE_ID\" to quit" >/dev/null 2>&1 || true
sleep 1
pkill -x da-tray >/dev/null 2>&1 || true

# Find DisplayAnything.app inside a folder, unpacking any zips on the way
# (the GitHub download is a zip that contains another zip).
find_app() {
  local dir="$1" depth=0 found
  while (( depth < 3 )); do
    found="$(find "$dir" -maxdepth 4 -name 'DisplayAnything.app' -type d -prune | head -n1)"
    [[ -n "$found" ]] && { echo "$found"; return 0; }
    local zips
    zips="$(find "$dir" -maxdepth 4 -name '*.zip' -type f)"
    [[ -z "$zips" ]] && return 1
    while IFS= read -r z; do
      ditto -x -k "$z" "$(dirname "$z")" && rm -f "$z"
    done <<< "$zips"
    depth=$((depth + 1))
  done
  return 1
}

if [[ -z "$SRC" ]]; then
  # No argument: use the newest DisplayAnything download, if there is one.
  SRC="$(ls -td "$HOME"/Downloads/DisplayAnything-macOS* 2>/dev/null | head -n1 || true)"
  [[ -n "$SRC" ]] && echo "Using the newest download: $SRC"
fi

if [[ -n "$SRC" ]]; then
  SRC="${SRC/#\~/$HOME}"
  [[ -e "$SRC" ]] || { echo "Not found: $SRC"; exit 1; }
  say "Installing from $SRC"
  TMP="$(mktemp -d)"
  if [[ -d "$SRC" && "$SRC" == *.app ]]; then
    ditto "$SRC" "$TMP/DisplayAnything.app"
  elif [[ -d "$SRC" ]]; then
    ditto "$SRC" "$TMP/src"
  else
    ditto -x -k "$SRC" "$TMP" || { echo "Could not unzip $SRC"; exit 1; }
  fi
  FOUND="$(find_app "$TMP")" || { echo "No DisplayAnything.app found in $SRC"; rm -rf "$TMP"; exit 1; }
  rm -rf "$APP"
  ditto "$FOUND" "$APP"
  rm -rf "$TMP"
fi

[[ -d "$APP" ]] || { echo "Can't find $APP. Pass the downloaded .zip as the first argument."; exit 1; }

say "Removing the quarantine flag"
xattr -cr "$APP"

# A stable certificate makes every build the same app to macOS. Prefer a
# Developer ID, then an Apple Development certificate; pick it by SHA-1 so
# two certificates with similar names can't be confused.
pick_identity() {
  if [[ -n "${DA_SIGN_IDENTITY:-}" ]]; then
    echo "$DA_SIGN_IDENTITY"
    return
  fi
  local ids kind line
  ids="$(security find-identity -v -p codesigning 2>/dev/null || true)"
  for kind in "Developer ID Application" "Apple Development"; do
    line="$(grep -m1 "\"$kind: " <<< "$ids" || true)"
    if [[ -n "$line" ]]; then
      awk '{print $2}' <<< "$line"
      return
    fi
  done
  echo "-"
}

IDENTITY="$(pick_identity)"
SIGNED_AS="-"
if [[ "$IDENTITY" != "-" ]]; then
  NAME="$(security find-identity -v -p codesigning 2>/dev/null | grep -m1 "$IDENTITY" | sed -E 's/^[^"]*"(.*)"$/\1/' || true)"
  say "Signing with your certificate: ${NAME:-$IDENTITY}"
  echo "   (macOS may ask to use the key in your keychain: enter your Mac password and click Always Allow)"
  if codesign --force --deep --timestamp=none --sign "$IDENTITY" "$APP"; then
    SIGNED_AS="${NAME:-$IDENTITY}"
  else
    echo "   Signing with the certificate failed; signing ad hoc instead."
  fi
fi
if [[ "$SIGNED_AS" == "-" ]]; then
  say "Re-signing locally (ad hoc)"
  codesign --force --deep --sign - "$APP" >/dev/null 2>&1 || echo "   (re-sign skipped; continuing)"
fi

PREVIOUS="$(cat "$STATE_FILE" 2>/dev/null || true)"
CLEARED=0
if [[ "$RESET" == 1 || "$SIGNED_AS" == "-" || "$SIGNED_AS" != "$PREVIOUS" ]]; then
  say "Clearing old permission grants (Accessibility, Input Monitoring, Screen Recording)"
  tccutil reset Accessibility "$BUNDLE_ID" >/dev/null 2>&1 || true
  tccutil reset ListenEvent   "$BUNDLE_ID" >/dev/null 2>&1 || true
  tccutil reset ScreenCapture "$BUNDLE_ID" >/dev/null 2>&1 || true
  CLEARED=1
else
  say "Keeping your permissions (same certificate as last time)"
fi
mkdir -p "$STATE_DIR"
echo "$SIGNED_AS" > "$STATE_FILE"

say "Opening DisplayAnything"
open "$APP"
sleep 2

if [[ "$CLEARED" == 1 ]]; then
  say "Opening the Input Monitoring, Accessibility and Screen Recording pages in System Settings"
  open "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent" || true
  sleep 1
  open "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility" || true
  sleep 1
  open "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture" || true
fi

BUILD="$(stat -f '%Sm' "$APP/Contents/MacOS/da-tray" 2>/dev/null || echo unknown)"
echo
echo "Installed build: $BUILD (the window also shows the build number at the bottom)"

if [[ "$CLEARED" == 1 ]]; then
  cat <<'MSG'

Last step (macOS only lets you do this by hand):
  In System Settings, switch DisplayAnything ON under BOTH Accessibility and
  Input Monitoring (macOS asks for each; the keyboard needs both, the mouse
  works without them). If you can't find a page, type its name into the
  System Settings search box.
  To let another computer see this Mac's screen (remote control), also switch
  DisplayAnything ON under Screen Recording.
  Then quit DisplayAnything from its tray icon and open it again.
MSG
  if [[ "$SIGNED_AS" != "-" ]]; then
    echo
    echo "  This build is signed with your certificate, so future builds keep these"
    echo "  permissions: this should be the last time you switch them on."
  fi
else
  cat <<'MSG'

Your permissions carried over. If keyboard, mouse or screen sharing stops
working anyway, run this script again with --reset-permissions.
MSG
fi

echo
echo "Done."
