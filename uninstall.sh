#!/bin/bash
# Removes omawe and gives the desktop back to the theme's own background.
# Leaves linux-wallpaperengine, Steam and ~/.config/omawe/config.toml alone.
set -euo pipefail

ID=org.pluseight.omawe
BIN="$HOME/.local/bin"

systemctl --user disable --now omawed.service 2>/dev/null || true
rm -f "$HOME/.config/systemd/user/omawed.service"
systemctl --user daemon-reload

[[ -x $BIN/omawe-integrate ]] && "$BIN/omawe-integrate" --remove
rm -f "$HOME/.config/hypr/omawe.lua"
omarchy plugin remove "$ID" --yes >/dev/null 2>&1 || rm -rf "$HOME/.config/omarchy/plugins/$ID"
rm -f "$BIN/omawed" "$BIN/omawe" "$BIN/omawe-pick" "$BIN/omawe-integrate"
rm -rf "${XDG_DATA_HOME:-$HOME/.local/share}/omawe" "${XDG_CACHE_HOME:-$HOME/.cache}/omawe"

# The background was one of our still frames; go back to one the theme ships.
omarchy theme bg next >/dev/null 2>&1 || true

echo "omawe удалён."
