#!/bin/bash
# Installs omawe for the current user. Idempotent: run it again to update.
set -euo pipefail
cd "$(dirname "$(readlink -f "$0")")"

ID=org.pluseight.omawe
OLD=io.github.dkgamer02ai.wallpaper-engine
PLUGINS="$HOME/.config/omarchy/plugins"
SHARE="${XDG_DATA_HOME:-$HOME/.local/share}/omawe/integration"
BIN="$HOME/.local/bin"

say() { printf '\033[1;34m::\033[0m %s\n' "$*"; }

command -v linux-wallpaperengine >/dev/null ||
  { echo "linux-wallpaperengine не найден: omarchy pkg aur add linux-wallpaperengine-git" >&2; exit 1; }
command -v cargo >/dev/null || { echo "нужен cargo (пакет rust или rustup)" >&2; exit 1; }

# The other Wallpaper Engine plugin kills every renderer it finds, ours included.
if [[ -d $PLUGINS/$OLD ]]; then
  say "Удаляю конфликтующий плагин $OLD"
  bash "$PLUGINS/$OLD/uninstall.sh" >/dev/null 2>&1 || true
  omarchy plugin remove "$OLD" --yes >/dev/null 2>&1 || rm -rf "${PLUGINS:?}/$OLD"
fi
pkill -f 'linux-wallpaperengine[^ ]* .*--screen-root' 2>/dev/null || true

say "Сборка"
cargo build --release --locked

say "Установка"
systemctl --user stop omawed.service 2>/dev/null || true
install -Dm755 target/release/omawed target/release/omawe target/release/omawe-tui integration/bin/omawe-pick integration/bin/omawe-integrate integration/bin/omawe-doctor -t "$BIN"
rm -rf "$SHARE"
mkdir -p "$SHARE"
cp -r integration/hypr integration/hooks "$SHARE/"
install -Dm644 integration/systemd/omawed.service -t "$HOME/.config/systemd/user"
systemctl --user daemon-reload

# `omarchy plugin add` already cloned this repository into the plugin folder.
FILES=(manifest.json BarWidget.qml Panel.qml i18n LICENSE)
if [[ $PWD != "$PLUGINS/$ID" ]]; then
  changed=false
  for f in "${FILES[@]}"; do diff -rq "$f" "$PLUGINS/$ID/$f" >/dev/null 2>&1 || changed=true; done
  if [[ $changed == true ]]; then
    had_plugin=$([[ -d $PLUGINS/$ID ]] && echo true || echo false)
    rm -rf "${PLUGINS:?}/$ID"
    mkdir -p "$PLUGINS/$ID"
    cp -r "${FILES[@]}" "$PLUGINS/$ID/"
    # The shell keeps the previous version of an already loaded widget; only a restart picks up an update.
    [[ $had_plugin == true ]] && { say "Перезапуск оболочки Omarchy"; omarchy restart shell >/dev/null 2>&1 || true; }
  fi
fi

"$BIN/omawe-integrate"
systemctl --user restart omawed.service
omarchy plugin enable "$ID" >/dev/null 2>&1 || omarchy-shell -q shell rescanPlugins || true

say "Готово. Выбор обоев: Super+Alt+W, меню > Style > Wallpaper Engine или значок в баре."
