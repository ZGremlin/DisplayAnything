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

say "Re-signing locally (ad hoc)"
codesign --force --deep --sign - "$APP" >/dev/null 2>&1 || echo "   (re-sign skipped; continuing)"

say "Clearing old permission grants (Accessibility, Input Monitoring)"
tccutil reset Accessibility "$BUNDLE_ID" >/dev/null 2>&1 || true
tccutil reset ListenEvent   "$BUNDLE_ID" >/dev/null 2>&1 || true

say "Opening DisplayAnything (it will ask for Accessibility)"
open "$APP"
sleep 2

say "Opening the Input Monitoring and Accessibility pages in System Settings"
open "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent" || true
sleep 1
open "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility" || true

BUILD="$(stat -f '%Sm' "$APP/Contents/MacOS/da-tray" 2>/dev/null || echo unknown)"
echo
echo "Installed build: $BUILD (the window also shows the build number at the bottom)"

cat <<'MSG'

Last step (macOS only lets you do this by hand):
  In System Settings, switch DisplayAnything ON under BOTH Accessibility and
  Input Monitoring (macOS asks for each; the keyboard needs both, the mouse
  works without them). If you can't find a page, type its name into the
  System Settings search box.
  Then quit DisplayAnything from its tray icon and open it again.

Done.
MSG
