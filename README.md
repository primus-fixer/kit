# kit — общий модуль приложений

Общая часть приложений «Полка», «Карман» и «Мост» (Tauri 2, React и Rust, Windows и Android):
то, что в них было одинаковым и правилось в каждом отдельно. В приложении остаются его записи,
экраны и инструменты для Claude.

## Что здесь есть

| Часть | Где | С версии |
|---|---|---|
| Ключ базы в хранилище системы (DPAPI, Android Keystore), Windows Hello | `crates/kit-secure` | 0.1.0 |
| PIN-код: хеш и паузы после ошибок; название приложения для сообщений | `crates/kit-core` | 0.1.0 |
| Вход: PIN-код, отпечаток, Windows Hello, автоблокировка — команды для экрана | `crates/kit-tauri` | 0.1.0 |
| Правила дизайна: кнопки, отступы, шрифты, экраны | [docs/design.md](docs/design.md) | 0.1.0 |
| Сборки: установщик для Windows, APK для Android, общая донастройка Android | `.github/workflows/windows.yml`, `android.yml`, `scripts/android-patch.sh` | 0.2.0 |
| Интерфейс: цвета и темы, шрифты, общие стили, элементы экрана, своя полоса окна, экран входа | `ui/` (пакет npm `@primus-fixer/kit`) | 0.3.0; экран входа отдельным файлом стилей — 0.5.0 |
| Коннектор для Claude: протокол MCP, доказательство ключа связи, файл расширения `.mcpb` | `crates/kit-core` (`mcp`) | 0.4.0 |
| Связь коннектора: посредник `--mcp`, приём запросов в запущенном приложении, запуск приложения из посредника | `crates/kit-core` (`link`) | 0.6.0 |

Что переедет дальше и в каком порядке — [docs/plan.md](docs/plan.md).

## Как подключить

В `Cargo.toml` приложения — зависимость от нужной версии по метке:

```toml
kit-core = { git = "https://github.com/primus-fixer/kit", tag = "v0.1.0" }
kit-secure = { git = "https://github.com/primus-fixer/kit", tag = "v0.1.0" }
kit-tauri = { git = "https://github.com/primus-fixer/kit", tag = "v0.1.0" }
```

Метку версии ставит владелец репозитория на GitHub (Releases → новый выпуск с меткой `vX.Y.Z`
на `main`). Пока метки нет, приложение ссылается на коммит: `rev = "<номер коммита>"` вместо `tag`.

Чем приложение отличается от других, оно описывает один раз — своим названием и строками ключей:

```rust
use kit_core::AppNames;
use kit_secure::KeyNames;

pub const APP: AppNames = AppNames { id: "polka", name: "Полка", name_acc: "Полку", name_gen: "Полки" };
pub const KEYS: KeyNames = KeyNames { dpapi_entropy: b"polka-db-key-v1", dpapi_label: "Polka", keystore_alias: "polka-db-key" };
```

**Строки ключей у выпущенного приложения менять нельзя:** с другими строками система не расшифрует
ключ базы, который уже лежит на устройстве.

Вход подключается в три шага (пример — `app/src-tauri/src/lib.rs` «Полки»):

1. при запуске создать `kit_tauri::guard::Lock` и отдать его Tauri: `app.manage(lock.clone())`;
2. вписать команды `kit_tauri::guard::*` в `tauri::generate_handler!`;
3. сообщать о фокусе окна: `kit_tauri::guard::on_focus(app, focused)`.

Перед работой с данными приложение спрашивает `lock.check()`: пока оно заблокировано, ответ — `locked`.

Коннектор для Claude (`kit_core::mcp`): приложение описывает себя — `Server` (имя, подсказка для Claude,
строка ключа связи), `Extension` (как расширение называется в приложении Claude) и список `Tool` — и
отдаёт строки запросов в `mcp::handle`. **Строку ключа связи у выпущенного приложения менять нельзя.**

Связь посредника с запущенным приложением (`kit_core::link`) описывает `Connector`: `Server`, `Extension`
(из него — переменная с папкой данных), название `AppNames` и идентификатор приложения (имя папки данных).
Запущенное приложение после открытия базы вызывает `link::listen(&CONNECTOR, папка, версия, |инструмент,
аргументы| …)`; `<программа> --mcp` — это `link::relay(&CONNECTOR, &инструменты, версия)`. Посредник прежней
версии и новое приложение понимают друг друга.

## Интерфейс

Пакет npm `@primus-fixer/kit` — прямо из этого репозитория, без сборки: исходники TypeScript собирает
Vite приложения. В `app/package.json`:

```json
"@primus-fixer/kit": "git+https://github.com/primus-fixer/kit.git#<версия kit>"
```

В `main.tsx` — шрифты и общие стили, затем свои стили приложения:

```ts
import '@primus-fixer/kit/ui/fonts';
import '@primus-fixer/kit/ui/styles.css';
import './styles.css';
```

Приложение, которое ещё не перешло на стили kit целиком, может взять только экран входа: вместо
`styles.css` — `@primus-fixer/kit/ui/lock.css` (он самостоятельный: нужны только переменные цветов
из `applyTheme`). Так сейчас сделано в «Кармане».

Что есть в `@primus-fixer/kit/ui`:

- `SCHEMES`, `applyTheme` — пять цветовых схем, светлая и тёмная тема;
- `Icon`, `addIcons` — значки; свои приложение добавляет к значкам kit;
- `Seg`, `Cut`, `Row`, `Header`, `Bar` — переключатель, раздел под катом, строка, верхняя строка, полоса;
- `TitleBar`, `customTitleBar`, `setTitleBarColors` — своя полоса окна на компьютере;
- `LockScreen`, `PinFlow`, `bioLabel`, `lockCommands` — экран входа и экраны PIN-кода. Название
  приложения (`names`) и команды входа (`api`) передаёт приложение: в самом приложении это
  `lockCommands()` — команды `kit-tauri`, в демо в браузере — свои;
- `stored`, `store` — настройки вида на этом устройстве.

Как ими пользоваться и какие у элементов классы — [docs/design.md](docs/design.md).

## Сборки

Сценарий сборок приложения вызывает общие сценарии kit. Приложение лежит в папке `app/` (Tauri 2, npm);
имя программы, название и версия берутся из `app/src-tauri/tauri.conf.json`.

```yaml
jobs:
  windows:
    uses: primus-fixer/kit/.github/workflows/windows.yml@<версия kit>
    with:
      mcp-tool: add_title            # проверить, что «polka.exe --mcp» знает этот инструмент; можно не задавать
  android:
    uses: primus-fixer/kit/.github/workflows/android.yml@<версия kit>
    with:
      title: Полка                   # название под иконкой
      key-alias: polka               # псевдоним ключа подписи, если нет секрета ANDROID_KEY_ALIAS
      kit-ref: <та же версия kit>
    secrets:
      ANDROID_KEYSTORE: ${{ secrets.ANDROID_KEYSTORE }}
      ANDROID_KEYSTORE_PASSWORD: ${{ secrets.ANDROID_KEYSTORE_PASSWORD }}
      ANDROID_KEY_ALIAS: ${{ secrets.ANDROID_KEY_ALIAS }}
```

- **Windows:** установщик `.exe` и `.msi` в артефакте `<программа>-windows`. Перед сборкой можно
  проверить пакеты Rust на Windows (`tests: -p …`), после — ответ коннектора для Claude (`mcp-tool`).
- **Android:** `<Название>-<версия>.apk` в артефакте `<программа>-android`. С секретами
  `ANDROID_KEYSTORE` и `ANDROID_KEYSTORE_PASSWORD` APK подписан постоянным ключом и ставится поверх
  прежнего, без них — отладочная сборка.
- **Донастройка Android:** сначала общий `scripts/android-patch.sh` kit — название под иконкой, иконки,
  разрешения сети, запрет копии Android, targetSdk; затем свой `scripts/android-patch.sh` приложения,
  если он есть, — главное окно, «Поделиться», виджеты.

Когда запускать сборки, решает приложение (`on:` в своём сценарии). Общий скрипт проверяется
на образце проекта `tauri android init` (`scripts/test-android-patch.sh`).

## Правила

- **Форматы не меняются.** Базы, резервные копии, файлы в облаке и связь устройств, сделанные
  прежними версиями приложений, должны работать. Это закреплено проверками на образцах — файлах,
  которые сделали выпущенные версии.
- **Версии** — метки `vX.Y.Z`. Приложение само решает, когда перейти на новую.
- **Приложения различаются настройками**, а не правками в модуле: название, строки ключей, порты,
  имена файлов.

## Проверки

```
cargo test
bash scripts/test-android-patch.sh
npm ci && npm run check      # типы интерфейса
```

На Windows дополнительно проверяется, что ключ, сохранённый «Полкой» и «Карманом» до модуля,
открывается модулем.
