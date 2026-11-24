#!/bin/bash
# DisplayAnything macOS setup: run after installing or updating a build.
#
#   bash macos-setup.sh                      # fix the copy in /Applications
#   bash macos-setup.sh ~/Downloads/DisplayAnything-macos.zip   # install from a zip first
#   bash macos-setup.sh ~/Downloads/DisplayAnything.app         # install from an .app first
#
# What it does:
#   1. Quits DisplayAnything if it is running.
#   2. Installs the given .zip or .app into /Applications (optional).
#   3. Removes the "downloaded from the internet" quarantine flag, so macOS opens it.
#   4. Re-signs it locally (ad hoc) so macOS treats it as one consistent app.
#   5. Clears the old Accessibility and Input Monitoring grants. Each new build
#      counts as a new app, so the old grants silently stop working.
#   6. Opens DisplayAnything, which asks for Accessibility again, and opens the
#      Settings page where you switch it on.
set -euo pipefail

APP="/Applications/DisplayAnything.app"
BUNDLE_ID="com.displayanything.app"
SRC="${1:-}"

say() { printf '\n==> %s\n' "$*"; }

say "Quitting DisplayAnything if it is running"
osascript -e "tell application id \"$BUNDLE_ID\" to quit" >/dev/null 2>&1 || true
sleep 1
pkill -x da-tray >/dev/null 2>&1 || true

if [[ -n "$SRC" ]]; then
  SRC="${SRC/#\~/$HOME}"
  case "$SRC" in
    *.zip)
      say "Installing from $SRC"
      TMP="$(mktemp -d)"
      ditto -x -k "$SRC" "$TMP"
      FOUND="$(find "$TMP" -maxdepth 3 -name 'DisplayAnything.app' -type d | head -n1)"
      [[ -n "$FOUND" ]] || { echo "No DisplayAnything.app inside $SRC"; exit 1; }
      rm -rf "$APP"
      ditto "$FOUND" "$APP"
      rm -rf "$TMP"
      ;;
    *.app|*.app/)
      say "Installing from $SRC"
      rm -rf "$APP"
      ditto "$SRC" "$APP"
      ;;
    *) echo "Expected a .zip or .app, got: $SRC"; exit 1 ;;
  esac
fi

[[ -d "$APP" ]] || { echo "Can't find $APP. Pass the downloaded .zip as the first argument."; exit 1; }

say "Removing the quarantine flag"
xattr -cr "$APP"

say "Re-signing locally (ad hoc)"
codesign --force --deep --sign - "$APP" >/dev/null 2>&1 || echo "   (re-sign skipped; continuing)"

say "Clearing old permission grants (Accessibility, Input Monitoring)"
tccutil reset Accessibility "$BUNDLE_ID" >/dev/null 2>&1 || true
tccutil reset ListenEvent   "$BUNDLE_ID" >/dev/null 2>&1 || true

say "Opening DisplayAnything (it will ask for Accessibility)"
open "$APP"
sleep 2

say "Opening the Accessibility page in System Settings"
open "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility" || true

cat <<'MSG'

Last step (macOS only lets you do this by hand):
  In the System Settings window that just opened, switch DisplayAnything ON.
  If macOS also asks about Input Monitoring, allow that too.
  Then quit DisplayAnything from its tray icon and open it again.

Done.
MSG
