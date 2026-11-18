#!/usr/bin/env bash
# Проверка scripts/new-app.sh: из заготовки получается приложение со своим названием, портами и метками,
# без слов заготовки, со ссылками на kit коммитом.
set -euo pipefail

KIT="$(cd "$(dirname "$0")/.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
APP="$TMP/most"

# Файлы версий библиотек здесь не нужны: проверяется только текст.
export NEW_APP_NO_LOCKS=1
bash "$KIT/scripts/new-app.sh" "$APP" most Мост Мост Моста м 47530 MST > /dev/null 2>"$TMP/err" || { cat "$TMP/err" >&2; exit 1; }

fail() { echo "test-new-app: $*" >&2; exit 1; }
has() { grep -q -- "$2" "$APP/$1" || fail "в $1 нет «$2»"; }

REV="$(git -C "$KIT" rev-parse HEAD)"
[ -d "$APP/crates/most-core" ] || fail "нет crates/most-core"
has Cargo.toml 'crates/most-core'
has crates/most-core/Cargo.toml "rev = \"$REV\", features = \[\"db\"\]"
has app/src-tauri/Cargo.toml "kit-tauri = { git = \"https://github.com/primus-fixer/kit\", rev = \"$REV\" }"
has app/src-tauri/Cargo.toml 'most-core = { path = "../../crates/most-core" }'
has app/package.json "\"@primus-fixer/kit\": \"github:primus-fixer/kit#$REV\""
has .github/workflows/build.yml "android.yml@$REV"
has .github/workflows/build.yml 'title: Мост'
has crates/most-core/src/lib.rs 'id: "most", name: "Мост", name_acc: "Мост", name_gen: "Моста", feminine: false'
has crates/most-core/src/lan.rs 'tcp_port: 47530'
has crates/most-core/src/lan.rs 'discovery_port: 47531'
has crates/most-core/src/lan.rs 'magic: \*b"MSTL"'
has crates/most-core/src/lan.rs 'not_ours: "это не «Мост»"'
has crates/most-core/src/cloud.rs 'magic: \*b"MSTC"'
has crates/most-core/src/cloud.rs 'events_ext: ".mev"'
has crates/most-core/src/cloud.rs 'ext: ".mbl"'
has crates/most-core/src/backup.rs 'magic: b"MOSTXBAK"'
has crates/most-core/src/backup.rs 'common: &\["most", "мост"\]'
has app/src-tauri/src/names.rs 'dpapi_entropy: b"most-db-key-v1", dpapi_label: "Most", keystore_alias: "most-db-key"'
has app/src-tauri/src/main.rs 'most_app_lib::run()'
has app/src-tauri/tauri.conf.json '"identifier": "io.github.primusfixer.most"'
has app/src/screens/Settings.tsx "port: 47530"
has app/src/App.tsx "{ name: 'Мост', nameAcc: 'Мост' }"
has README.md '# Мост'
if grep -rIn -e starter -e Заготов -e ZGT -e STARTBAK -e 47520 -e 'feminine: true' "$APP"; then fail "остались слова заготовки"; fi
# Неверные настройки не принимаются.
for bad in "x Мост Мост Моста м 47530 MST" "most Мост Мост Моста м 47510 MST" "most Мост Мост Моста м 47530 PLK" "most Мост Мост Моста о 47530 MST"; do
  # shellcheck disable=SC2086
  if bash "$KIT/scripts/new-app.sh" "$TMP/bad" $bad > /dev/null 2>&1; then fail "приняты неверные настройки: $bad"; fi
done
echo "test-new-app: ok"
