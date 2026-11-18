# Заготовка

Приложение на общем модуле [kit](https://github.com/primus-fixer/kit) (Tauri 2, React и TypeScript, Rust) для Windows
и Android. Сделано из заготовки kit (`scripts/new-app.sh`): всё общее — вход по PIN-коду и отпечатку, зашифрованная
база, обмен по Wi-Fi и через облако, свой вид и полоса окна — уже работает. Своё приложение начинается с записей
и экранов.

## Что уже есть

- **Вход:** PIN-код, отпечаток на телефоне, Windows Hello; блокировка после 5 минут в фоне (kit: `kit_tauri::guard`).
- **База:** SQLCipher, ключ — в хранилище системы (DPAPI на Windows, Android Keystore). Записи «вид, номер,
  содержимое» с журналом событий: новое поле или вид записи не требуют новой версии базы.
- **Обмен по Wi-Fi:** связь по коду или QR-коду, обмен со связанными устройствами в фоне.
- **Облако:** Яндекс Диск по паролю приложения или свой сервер WebDAV; всё шифруется паролем копий.
- **Вид:** общие стили и элементы kit, пять расцветок, светлая и тёмная тема, своя полоса окна на компьютере.
- **Сборки:** установщик для Windows и APK для Android — общими сценариями kit, после слияния в `main`
  и по кнопке (Actions → Сборки → Run workflow).

Для примера записи — заметки (`crates/starter-core/src/model.rs`, `app/src/screens/Notes.tsx`).

## С чего начать

```
cd app
npm install
npm run tauri dev
```

Проверки: `cargo test` в корне, `npm run build` в `app/`.

## Что поменять под своё приложение

1. **Записи** — `crates/starter-core/src/model.rs`: свои виды записей вместо заметок (`kind` — строкой; у выпущенного
   приложения не менять). Снимок записи — поле `image`.
2. **Команды** — `app/src-tauri/src/lib.rs`: свои команды вместо `notes`, `note_save`, `note_delete`; не забудьте
   вписать их в `generate_handler!`.
3. **Экраны** — `app/src/screens/`, разделы — `TABS` в `app/src/App.tsx`, значки — `app/src/icons.ts`.
4. **Слова** — в `app/src/screens/Settings.tsx` (`WIFI_WORDS`, `CLOUD_WORDS`): что едет вместе с записями.
5. **Название в боковом меню** — размер в `app/src/styles.css` (`.wordmark`).
6. **Иконка** — `app/src-tauri/icons/` (в заготовке — простая картинка): `npm run tauri icon <картинка 1024×1024>`.
7. **Подпись APK** — секреты репозитория `ANDROID_KEYSTORE`, `ANDROID_KEYSTORE_PASSWORD`, `ANDROID_KEY_ALIAS`
   (как у «Полки» и «Кармана»).

## Что менять нельзя

Когда приложение выпущено, эти настройки остаются навсегда, иначе устройства перестанут узнавать друг друга,
а база, облако и копии — открываться:

- строки ключей — `app/src-tauri/src/names.rs`;
- порты и метки связи по Wi-Fi — `crates/starter-core/src/lan.rs`;
- файл ключа, метка и расширения облака — `crates/starter-core/src/cloud.rs`;
- метка и расширение копий — `crates/starter-core/src/backup.rs`;
- ключи в `meta` (`cloud.password`, `backup.key`) — `app/src-tauri/src/cloud.rs`;
- виды записей (`kind`).

## Что добавить позже

Резервные копии в файл (формат уже общий — `kit_core::backup`, экран и команды — как у «Полки»), коннектор
для приложения Claude (`kit_core::mcp`, `kit_core::link`) — когда они понадобятся приложению.

## Общий модуль

Приложение ссылается на kit коммитом (`Cargo.toml` крейтов, `app/package.json`, `.github/workflows/build.yml`).
Перейти на новую версию kit — заменить коммит во всех трёх местах и прогнать проверки.
