# omawe — Wallpaper Engine для Omarchy

Живые обои из Steam Wallpaper Engine на рабочем столе Omarchy (Hyprland + Quickshell).
Рендерит [linux-wallpaperengine](https://github.com/Almamu/linux-wallpaperengine); omawe управляет им:
библиотека, применение по мониторам, пауза, экономия батареи.

Нужна легальная копия Wallpaper Engine в Steam. omawe ничего из неё не копирует и не распространяет:
он только читает `project.json` и передаёт пути рендереру.

> Состояние: **этап 1 (MVP)**. Что готово и что нет — ниже.

## Требования

- Omarchy 4.5+ (оболочка на Quickshell)
- `linux-wallpaperengine` — `omarchy pkg aur add linux-wallpaperengine-git`
- Steam с установленным Wallpaper Engine (appid 431960) и подписками Workshop
- Rust (cargo) для сборки

## Установка

```bash
git clone https://github.com/pluseight8/wallpaper-engine-for-omarchy && cd wallpaper-engine-for-omarchy && ./install.sh
```

`install.sh` можно запускать повторно. Он:

- собирает и кладёт в `~/.local/bin`: `omawed`, `omawe`, `omawe-pick`, `omawe-integrate`;
- ставит user-юнит `omawed.service` и включает его;
- копирует плагин оболочки в `~/.config/omarchy/plugins/org.pluseight.omawe` и включает виджет в баре;
- добавляет меню > Style > Wallpaper Engine, бинды (`~/.config/hypr/omawe.lua`), хуки `theme-set` и `post-update`;
- **удаляет плагин `io.github.dkgamer02ai.wallpaper-engine`**, если он стоит: тот убивает любые процессы рендерера, включая наши.

Файлы Omarchy в `/usr/share/omarchy` не меняются. Удаление: `./uninstall.sh` — возвращает фон темы.

## Использование

| Действие | Как |
|---|---|
| Выбрать обои (превью, поиск) | `Super+Alt+W`, ЛКМ по значку в баре, меню > Style > Wallpaper Engine |
| Следующие / предыдущие | `Super+Alt+]` / `Super+Alt+[`, колесо на значке |
| Пауза | `Super+Alt+\`, ПКМ по значку |
| Случайные | СКМ по значку, `omawe random` |

```
omawe apply <id|название> [--monitor DP-1] [--scaling fill|fit|stretch|default] [--clamp clamp|border|repeat]
omawe next | prev | random [--type video] [--tag Nature] [--fav]
omawe pause | resume | toggle
omawe stop                 # убрать живые обои, оставить статичный кадр
omawe list [--type T] [--tag T] [--rating R] [--search S] [--fav] [--sort recent] [--json]
omawe fav <id|название>    # избранное
omawe pick [фильтры list]  # пикер с превью
omawe status | watch | rescan | config
```

Без `--monitor` обои ставятся на все мониторы, с ним — только на указанный.

## Настройки

`~/.config/omawe/config.toml`, применяется сразу после сохранения. Все ключи необязательны, ниже значения по умолчанию:

```toml
fps_ac = 60
fps_battery = 30
battery = "fps"          # на батарее: "fps" — снизить FPS, "pause" — пауза, "static" — остановить рендерер
pause_fullscreen = true  # пауза, когда все мониторы закрыты полноэкранными окнами
pause_maximized = true   # считать развёрнутое окно полноэкранным
stop_on_game = true      # остановить рендерер, пока открыта игра Steam (освобождает VRAM)
silent = true
volume = 15
automute = true
audio_processing = true
scaling = "fill"
clamp = "clamp"
sync_omarchy_bg = true   # держать штатным фоном Omarchy кадр текущих обоев
notify = true
```

## Как это устроено

- `omawed` — демон на Rust (tokio, один поток). Просыпается только по событиям: запрос CLI, событие Hyprland (`socket2`),
  смена питания (UPower по D-Bus), изменение папки Workshop или конфига (inotify), завершение рендерера.
- `omawe` — тонкий клиент, JSON по сокету `$XDG_RUNTIME_DIR/omawe.sock`.
- Индекс библиотеки — SQLite в `~/.local/share/omawe/library.db`. При изменениях разбираются только те `project.json`, у которых сменился mtime.
- **Статичный двойник.** Первый кадр каждых обоев сохраняется в `~/.cache/omawe/twins/` и ставится штатным фоном Omarchy.
  Он лежит слоем ниже рендерера, поэтому при смене обоев, паузе, остановке на время игры или падении рендерера чёрного экрана нет.
  Экран блокировки берёт этот же кадр.
- Смена обоев: новый рендерер стартует поверх старого, старый гасится после первого кадра нового.
- Пауза — `SIGSTOP`/`SIGCONT`, процесс не перезапускается.

## Замеры (этап 1)

Core Ultra X7 358H, Arc B390, два монитора 1920×1080@180, питание от сети, scene-обои.

| Что | Результат | Цель из ТЗ |
|---|---|---|
| Демон в простое, 20 с | 0 тиков CPU, RSS 7.9 МБ, 1 поток | < 0.3% CPU, < 15 МБ |
| Вызов `omawe status` | 1.6–1.9 мс | < 20 мс |
| GPU на паузе (`SIGSTOP`) | 0.00% render-движка | ~0% |
| GPU в работе, 60 FPS, 2 монитора | 24.5% render-движка | — |
| Первый кадр после `apply` | 880 мс | — |
| Смена обоев целиком | ~1.1 с, без чёрного кадра | < 500 мс — **не достигнуто** |

GPU считан из `/proc/<pid>/fdinfo` (драйвер `xe`), без `intel_gpu_top`.
500 мс недостижимы: сам рендерер отдаёт первый кадр через 0.7–0.9 с, а управления «на лету» у него нет.
Не измерено: расход батареи, аппаратное декодирование видео, холодный старт сессии.

## Ограничения

- **Обои типа application не работают** — `linux-wallpaperengine` их не поддерживает. Они помечены в списке как `unsupported`, применить их нельзя.
- У рендерера нет IPC. Любое изменение (обои, FPS, громкость, масштабирование) — это перезапуск с бесшовной подменой, ~1 с.
- Пауза по fullscreen срабатывает, когда закрыты **все** мониторы: процесс рендерера один на все экраны.
- При разных обоях на мониторах штатный фон Omarchy один, поэтому двойник берётся из общего кадра рендерера.
- Совместимость (`ok` / `crash`) пока отмечается по факту запуска, автотеста всей библиотеки нет.
- Новая библиотека Steam, добавленная после старта демона, подхватывается после `systemctl --user restart omawed`.

## План

1. ✅ MVP: библиотека, применение, пауза, питание, пикер, виджет, установка.
2. Свойства обоев и пресеты, плейлисты, аудио, своя панель-браузер с фильтрами и редактором.
3. Тема из палитры обоев, профили питания, обои по воркспейсам, автотест совместимости, метаданные Workshop, `omawe doctor`, пауза при блокировке и DPMS.
4. TUI, `PKGBUILD`, заявка в plugins.omarchy.org.

Факты о системе и обоснование решений: [docs/DISCOVERY.md](docs/DISCOVERY.md).

## Разработка

```bash
cargo test
```

```bash
cargo clippy --all-targets -- -D warnings
```

Интеграционный тест поднимает настоящий демон с поддельной библиотекой Steam и скриптом вместо рендерера.

## Лицензия

MIT
