# omawe — фаза 0: разведка

Дата: 2026-11-11. Всё ниже проверено на живой системе, если не помечено «не проверено».

## 0. Главное: ТЗ расходится с системой

ТЗ описывает старый Omarchy (Waybar, Walker, mako, hyprlock, hypridle, `~/.local/share/omarchy`).
На машине стоит **Omarchy 4.5.0 («Quattro»)**, где этого стека нет.

| В ТЗ | На системе | Что делаем |
|---|---|---|
| `~/.local/share/omarchy/` | нет; Omarchy — пакет в `/usr/share/omarchy/` (только чтение) | не трогаем, читаем как справку |
| Waybar-модуль | Waybar не установлен; бар — плагин Quickshell (`omarchy-shell`) | bar-widget на QML |
| Walker (dmenu) | не установлен; меню — Quickshell, расширяется через `~/.config/omarchy/extensions/omarchy-menu.jsonc` | пункт меню в JSONC + панель-браузер на QML |
| mako / `notify-send` | уведомления рисует shell; есть `omarchy-notification-send` | используем его |
| hyprlock | нет; экран блокировки — плагин shell (`WlSessionLock`), у пользователя включён клон `io.github.sirjul1337.lock-explorer` | локскрин берёт размытый штатный фон → см. «статичный двойник» |
| hypridle | нет; idle — сервис shell, таймеры в `shell.json` (`idle.lock`, `idle.screensaver`) | lock ловим через `omarchy-shell lock isLocked`/logind (уточнить в MVP) |
| бинды через `source` в `.conf` | Hyprland 0.56.2, конфиг на **Lua**: `require("hypr.bindings")`, хелперы `o.bind`, `o.launch_on_start` | свой модуль `~/.config/hypr/omawe.lua` + одна строка `require` |
| «GUI на стеке Omarchy» | Quickshell 0.3.2 + QML, кит `qs.Ui` (`KeyboardPanel`, `PopupCard`), темы из `Commons` | основной UI — плагин shell, не опция |
| Магазин плагинов | plugins.omarchy.org: публичный GitHub-репозиторий, `manifest.json` **в корне**, README, LICENSE, безопасные install/remove; заявка через issue-форму | корень репозитория = плагин shell |

## 1. Как Omarchy рисует фон

- Плагин shell `omarchy.background` (`/usr/share/omarchy/shell/plugins/background/Background.qml`): layer-shell,
  namespace `omarchy-background`, слой **Background**. Рисует только статичные картинки.
- Видеофоны рисует штатный демон **`owe` 0.2.14** (пакет `owe`, github.com/omacom/owe, написан на C):
  `owed.service` (user unit, `PartOf=graphical-session.target`), сокет `$XDG_RUNTIME_DIR/owe/owed.sock`, CLI `owe`.
  Сам умеет паузу при fullscreen/батарее/локе/DPMS. Сейчас простаивает (`engine: shell`, `render_alive: false`), RSS 692 КБ.
  Имя `owe` занято — наш `omawe` с ним не пересекается.
- Текущий фон: симлинк `~/.local/state/omarchy/current/background`. Меняется командой `omarchy theme bg set <файл>`
  (симлинк + `omarchy-shell -q background set`). Тема: `~/.local/state/omarchy/current/theme/` (`colors.toml`, `theme.name`).
- Хуки: `~/.config/omarchy/hooks/{theme-set,post-update,post-boot,battery-low,font-set,pre-refresh-pacman}.d/`,
  установка `omarchy hook install <тип> <файл>`. `theme-set` получает slug темы в `$1`.
- Смена темы сбрасывает фон на картинку темы → нужен хук `theme-set`.
- Шрифт: JetBrainsMono Nerd Font (`omarchy font current`). Цвета: `colors.toml` (accent, background, foreground, 16 цветов).

## 2. Что уже стоит и конфликтует

**Плагин `io.github.dkgamer02ai.wallpaper-engine` 1.0.0** (12★, в магазине) — уже установлен и работает:
bash-CLI `omarchy-we` (484 строки) + QML-пикер. Он оставил:
- `~/.local/bin/omarchy-we`, строку `o.launch_on_start("omarchy-we launch")` в `autostart.lua`;
- хук `theme-set.d/50-wallpaper-engine`, пункт меню `style.wallpaper-engine`;
- фон Omarchy = `transparent.png` (чтобы не закрывать обои);
- `stop_we` делает `pkill -f "linux-wallpaperengine.* --screen-root"` — **убьёт и наши процессы**.

Вывод: два плагина одновременно жить не могут. `install.sh` должен обнаружить его и предложить снять
(штатным `uninstall.sh` плагина + `omarchy plugin remove`), `omawe doctor` — сообщать о конфликте.

Ещё три репозитория на GitHub с тем же назначением (Sebas200702, ryguy0601, Azteriisk; 0–1★) — не установлены, не изучал.

## 3. Бэкенд `linux-wallpaperengine-git r643.a0ddce58`

Установлен в `/opt/linux-wallpaperengine`, обёртка `/usr/bin/linux-wallpaperengine` (делает `cd /opt/...`).
Реальные флаги из `--help`:

| Группа | Флаги |
|---|---|
| Вывод | `--screen-root <mon>` (повторяемый), `--screen-span a,b`, `--window`, `--bg`, `--layer background\|bottom\|top\|overlay` (по умолчанию `bottom`) |
| Картинка | `--scaling`, `--clamp` (на каждый монитор) |
| Производительность | `--fps` (по умолчанию 30), `--hwdec auto\|nvdec-copy\|no`, `--no-fullscreen-pause`, `--fullscreen-pause-only-active`, `--fullscreen-pause-ignore-appid` |
| Звук | `--volume` (15), `--silent`, `--noautomute`, `--no-audio-processing` |
| Свойства | `--list-properties`, `--set-property k=v` (повторяемый) |
| Кадр | `--screenshot <png>`, `--screenshot-delay <кадры>` |
| Прочее | `--playlist` (из `config.json` Wallpaper Engine), `--assets-dir`, `--disable-particles/-mouse/-parallax` |

Факты:
- **Один процесс обслуживает несколько мониторов** с разными обоями — так сейчас и работает (PID 8479: 2 монитора, RSS 42 МБ, 0.4% CPU, 16 ч аптайма).
- **IPC и управления на лету нет.** Любое изменение (обои, свойство, FPS, громкость) = перезапуск процесса.
- Пауза при fullscreen встроена (Wayland). Остальные паузы — только SIGSTOP/SIGCONT снаружи.
- `--screenshot` работает: первый кадр scene-обоев через **755 мс** от запуска, PNG не чёрный. Режима headless нет — открывается окно.
- `--list-properties` работает и печатает тип/значение.
- `--playlist` бесполезен: в `config.json` пользователя плейлистов нет → плейлисты делает демон.
- В логе есть `Failed to initialize GLEW: No GLX display` — безвредно на Wayland.

Следствия для ТЗ:
- «Живое применение свойств» = перезапуск с бесшовной подменой (~0.8 с), не мгновенно. Пометить в UI.
- «Переключение < 500 мс» при старте процесса 755 мс недостижимо холодным запуском. Реальная цель — **без чёрного кадра**, ~0.8–1 с.
- VA-API: `intel-media-driver 26.3.5` и `mesa 26.2.5` стоят, `--hwdec auto` есть; факт аппаратного декодирования **не проверен** (нет `vainfo`/`intel_gpu_top`).

## 4. Лучший способ бесшовной смены (найден здесь)

Фон Omarchy лежит на слое Background, бэкенд — на слое Bottom, то есть **над** ним. Поэтому:

1. Для каждых обоев храним кадр-двойник (`--screenshot`).
2. Штатным фоном Omarchy ставим **двойник**, а не `transparent.png`.
3. Смена обоев: стартует новый процесс → через ~1 с гасим старый. Под ними всегда лежит кадр, чёрного экрана быть не может.

Это же бесплатно закрывает ещё четыре пункта ТЗ: эко-режим (убили процесс — остался кадр, 0% GPU, VRAM свободна),
локскрин (он размывает штатный фон), выход из сессии, падение бэкенда. Отдельного кода для них не нужно.
Не проверено: порядок двух поверхностей на одном слое Bottom (новая сверху или снизу) — проверить первым делом в MVP;
если новая окажется снизу, гасим старый сразу после первого кадра нового — под ними всё равно лежит двойник.

## 5. Steam и библиотека

- Steam нативный: `~/.local/share/Steam` (`~/.steam/steam` → туда же). Flatpak-Steam не установлен (путь `~/.var/app/com.valvesoftware.Steam` поддержим, но проверить негде).
- `steamapps/libraryfolders.vdf`: одна библиотека. Wallpaper Engine (431960) установлен: `steamapps/common/wallpaper_engine/{assets,projects,config.json}`.
- Workshop: `steamapps/workshop/content/431960/` — **285 элементов**.
- `project.json`: `type`, `title`, `file`, `preview`, `tags[]`, `contentrating`, `general.properties{ключ:{type,text,value,order,…}}`.
  Бывает UTF-8 BOM. До 368 свойств у одного проекта.

Состав библиотеки пользователя:

| Тип | Кол-во | Статус в бэкенде |
|---|---|---|
| scene | ~170 | работает (качество зависит от шейдеров/скриптов конкретных обоев) |
| video | 76 | работает (libmpv) |
| web | ~19 | работает через встроенный CEF (тяжело: отдельный Chromium) |
| пресет (нет `type`, есть `dependency` + `preset`) | 20 | запускаем базовые обои из `dependency` с `--set-property` из `preset` |
| application | 0 | **не поддерживается**, показываем бейдж «недоступно» |

Это априорная матрица. Фактическая строится автотестом (скриншот → проверка на чёрный кадр/краш), ~1 с на обои.

## 6. Железо и измерения

- HONOR ZQC-P, Core Ultra X7 358H, Arc B390, батарея `BAT0`, адаптер `ADP1`.
- Сейчас: встроенный `eDP-1` отключён, работают два внешних 1920×1080@180 (`HDMI-A-1`, `DP-1`), питание от сети.
- Для приёмки не хватает пакетов: `intel-gpu-tools`, `powertop`, `libva-utils`, `chafa`, `qt6-declarative` (qmllint). Поставлю с вашего разрешения.
- Rust/cargo, ffmpeg, ImageMagick, sqlite3, jq, socat — есть.

## 7. План интеграции

**Раскладка репозитория** (магазин требует `manifest.json` в корне):

```
manifest.json, BarWidget.qml, Panel.qml, Service.qml, i18n/   ← плагин shell (id: org.pluseight.omawe)
crates/omawed, crates/omawe-cli, crates/omawe-tui
integration/{hypr/omawe.lua, menu.jsonc, hooks/, systemd/omawed.service}
install.sh, uninstall.sh, PKGBUILD, README.md, docs/
```

`omarchy plugin add <url>` только клонирует репозиторий и скрипты не запускает (проверено по исходнику).
Значит демон ставится отдельно: `install.sh` (cargo build → `~/.local/bin`) или пакет из `PKGBUILD`.
Плагин без демона показывает одну кнопку «установить демон».

**Что меняется относительно ТЗ**

- Waybar-модуль → bar-widget QML (ЛКМ панель, ПКМ пауза, колесо — следующие). Обновление по событию от демона, без интервала.
- Walker → пункт в `omarchy-menu.jsonc` + QML-панель-браузер с превью (поиск, фильтры, избранное, бейджи, редактор свойств).
- Бинды → `~/.config/hypr/omawe.lua` и строка `require("hypr.omawe")` в `hyprland.lua` (пользовательский файл, не upstream).
- Автозапуск → `omawed.service` (`Type=notify`, `Restart=on-failure`, `PartOf=graphical-session.target`), бэкенд — дочерний процесс демона.
- Уведомления → `omarchy-notification-send`.
- Хуки: `theme-set.d/50-omawe` (вернуть двойник фоном + палитра), `post-update.d/omawe` (doctor --fix).
- События без поллинга: Hyprland `socket2` (fullscreen, workspace, monitor, окна `steam_app_*` = «игра запущена»), UPower по D-Bus, inotify на workshop.
  Lock и DPMS: источник события выбрать в MVP (logind `Lock`/`Unlock` и socket2; **не проверено**, что shell их шлёт).

**Что предлагаю урезать или отложить**

- TUI на ratatui: при родной QML-панели это второй интерфейс к тому же. Предлагаю последним этапом, после полировки.
- Подписка/отписка «через клиент»: без ключа API доступно только открыть страницу (`steam://url/CommunityFilePage/<id>`), кнопку жмёт пользователь. Так и напишу в README.
- SQLite, как в ТЗ, оставляю (rusqlite bundled), хотя на 285 записей хватило бы JSON.

**Этапы**

1. MVP: парсеры vdf/project.json + индекс, демон (процесс, пауза, двойник, socket2, UPower), CLI, пункт меню, bar-widget, install/uninstall, снятие конфликта с плагином dkgamer02ai. Замеры: RSS/CPU демона, время смены, GPU под fullscreen.
2. Свойства, пресеты, плейлисты, аудио, per-monitor.
3. Палитра → тема `omawe-live`, профили питания, workspace-обои, автотест совместимости, Workshop-метаданные, doctor.
4. Полировка, i18n (ru/en), PKGBUILD, заявка в магазин, TUI.

## 8. Вопросы к вам

1. Снять установленный плагин `io.github.dkgamer02ai.wallpaper-engine` при установке omawe? (Иначе они убивают процессы друг друга.)
2. ID плагина `org.pluseight.omawe` — подходит?
3. Поставить `intel-gpu-tools powertop libva-utils chafa qt6-declarative` для замеров?
4. TUI — последним этапом или вообще убрать?
