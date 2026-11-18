# __NAME__

__DESCRIPTION__.

Приложение на общем модуле [kit](https://github.com/primus-fixer/kit) __KIT_VERSION__: Tauri 2, React и Rust,
Windows и Android. Из kit — вход по PIN-коду, отпечатку и Windows Hello, зашифрованная база с журналом
записей, обмен по Wi‑Fi и через облако (Яндекс Диск, свой сервер WebDAV), общий интерфейс и сборки.

## Устройство

| Папка | Что там |
|---|---|
| `crates/__ID__-core` | Ядро: записи и чем «__NAME__» отличается от других приложений на kit — название, порты, метки, имена файлов (`src/lib.rs`) |
| `app/src-tauri` | Приложение Tauri: открытие базы, вход, обмен, команды экранов (`src/lib.rs`), строки ключей (`src/names.rs`) |
| `app/src` | Экраны (React): главный, настройки с разделами «Вход», «Обмен по Wi‑Fi», «Облако» |

Записи лежат в хранилище kit (`kit_core::store`): `store.put(вид, номер, &значение)`, `get`, `list`,
`delete`. Каждое изменение попадает в журнал и само уходит на другие устройства — по Wi‑Fi и через облако.
Свои таблицы, которым не нужен обмен, приложение заводит в той же базе через `store.conn()`.

## Строки, которые нельзя менять после выпуска

Название латиницей (`__ID__`), строки ключей в `app/src-tauri/src/names.rs`, порты, метки и имена файлов
в `crates/__ID__-core/src/lib.rs`, `identifier` в `app/src-tauri/tauri.conf.json`. С другими строками
устройства не откроют свою базу, перестанут узнавать друг друга, облако и копии — открываться. Это
закреплено проверками `strings_are_kept` и `key_names_are_kept`.

## Разработка

```
cd app
npm install
npm run tauri dev      # приложение
npm run dev            # демо в браузере, без Tauri: данные в памяти
```

Проверки:

```
cargo test
cd app && npm run build
```

## Сборки

Установщик для Windows и APK для Android собираются после слияния в `main` и по кнопке (Actions →
Сборки → Run workflow) общими сценариями kit. Чтобы APK ставился поверх прежнего, в секретах репозитория
нужны `ANDROID_KEYSTORE` (файл ключа в base64), `ANDROID_KEYSTORE_PASSWORD` и, если псевдоним ключа не
`__ID__`, `ANDROID_KEY_ALIAS`; без них собирается отладочный APK. Свою донастройку Android-проекта
(«Поделиться», виджеты, службы) приложение кладёт в `scripts/android-patch.sh` — её вызовет сборка.

## Иконка

Иконка — заготовка kit. Своя: заменить `app/app-icon.svg` и выполнить в `app/`
`npx tauri icon app-icon.svg -o src-tauri/icons`.

## Откуда взялось

Создано заготовкой kit (`scripts/new-app.py`), kit __KIT_VERSION__.
