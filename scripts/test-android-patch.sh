#!/usr/bin/env bash
# Проверка общей донастройки Android (`android-patch.sh`) на образце проекта `tauri android init`.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PATCH="$ROOT/scripts/android-patch.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

fail() { echo "НЕ ПРОШЛО: $1"; exit 1; }

cp -r "$ROOT/tests/android-template" "$WORK/once"
mkdir -p "$WORK/icons/mipmap-hdpi"
printf 'png' > "$WORK/icons/mipmap-hdpi/ic_launcher.png"
bash "$PATCH" "$WORK/once" "$WORK/icons" "Проба Kit" > /dev/null

MAIN="$WORK/once/app/src/main"
MANIFEST="$MAIN/AndroidManifest.xml"
grep -q '<string name="app_name">Проба Kit</string>' "$MAIN/res/values/strings.xml" || fail "название под иконкой"
grep -q '<string name="main_activity_title">Проба Kit</string>' "$MAIN/res/values/strings.xml" || fail "название окна"
[ -f "$MAIN/res/mipmap-hdpi/ic_launcher.png" ] || fail "иконки"
for p in INTERNET ACCESS_NETWORK_STATE ACCESS_WIFI_STATE CHANGE_WIFI_MULTICAST_STATE ACCESS_LOCAL_NETWORK; do
  [ "$(grep -c "android.permission.$p\"" "$MANIFEST")" = 1 ] || fail "разрешение $p — ровно одно"
done
grep -q 'android:allowBackup="false"' "$MANIFEST" || fail "копия Android выключена"
grep -q 'android:dataExtractionRules="@xml/data_extraction_rules"' "$MANIFEST" || fail "правила переноса"
[ "$(grep -c '<exclude domain="database" path="." />' "$MAIN/res/xml/data_extraction_rules.xml")" = 2 ] || fail "база не уходит ни в облако, ни на новый телефон"
grep -q 'targetSdk = 36' "$WORK/once/app/build.gradle.kts" || fail "targetSdk 36"
# Разрешения стоят до <application>, а не внутри него.
awk '/<application/ { inside = 1 } /uses-permission/ && inside { bad = 1 } END { exit bad }' "$MANIFEST" || fail "разрешения внутри <application>"

# Повторный запуск ничего не меняет.
cp -r "$WORK/once" "$WORK/twice"
bash "$PATCH" "$WORK/twice" "$WORK/icons" "Проба Kit" > /dev/null
diff -r "$WORK/once" "$WORK/twice" > /dev/null || fail "повторный запуск изменил проект"

# Опасные знаки в названии — отказ, проект не тронут.
for bad in 'a&b' 'a/b' 'a#b' 'a<b' 'a"b' "a'b" 'a\b'; do
  cp -r "$ROOT/tests/android-template" "$WORK/bad"
  if bash "$PATCH" "$WORK/bad" "" "$bad" > /dev/null 2>&1; then fail "название «$bad» принято"; fi
  diff -r "$ROOT/tests/android-template" "$WORK/bad" > /dev/null || fail "проект изменён при отказе («$bad»)"
  rm -rf "$WORK/bad"
done
if bash "$PATCH" "$WORK/nothing" "" "Проба" > /dev/null 2>&1; then fail "папка без проекта принята"; fi

echo "Донастройка Android: все проверки прошли"
