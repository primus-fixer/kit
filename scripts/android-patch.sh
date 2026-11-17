#!/usr/bin/env bash
# Общая донастройка Android-проекта, который создаёт `tauri android init`, — для всех приложений на kit:
# название под иконкой, иконки, разрешения сети, запрет копии Android и targetSdk.
#
#   bash android-patch.sh <gen/android> <иконки> <название>
#
# Своё (главное окно, «Поделиться», виджеты) приложение донастраивает своим скриптом после этого.
# Повторный запуск ничего не меняет.
set -euo pipefail

PROJECT="${1:?путь к gen/android}"
ICONS="${2:-}"
NAME="${3:?название приложения под иконкой}"
MAIN="$PROJECT/app/src/main"
MANIFEST="$MAIN/AndroidManifest.xml"

[ -f "$MANIFEST" ] || { echo "Нет $MANIFEST — это не проект tauri android init"; exit 1; }
# Название идёт в sed и в XML: знаки, которые там что-то значат, не годятся.
case "$NAME" in
  *[\#\&\\/\<\>\"\']* | *$'\n'*)
    echo "Название «$NAME»: нельзя знаки # & \\ / < > \" ' и перевод строки"
    exit 1
    ;;
esac

# Название под иконкой.
STRINGS="$MAIN/res/values/strings.xml"
if [ -f "$STRINGS" ]; then
  sed -i -E "s#(<string name=\"(app_name|main_activity_title)\">)[^<]*(</string>)#\\1$NAME\\3#" "$STRINGS"
fi

# Иконки, которые сделал `tauri icon`.
if [ -n "$ICONS" ] && [ -d "$ICONS" ]; then
  cp -r "$ICONS"/. "$MAIN/res/"
fi

# Разрешения: состояние сети и Wi-Fi, широковещательный поиск устройств в сети и доступ к локальной сети.
# INTERNET уже есть в шаблоне, CAMERA добавляет модуль сканера штрихкодов и QR-кодов.
add_permission() {
  local name="$1"
  grep -q "android:name=\"$name\"" "$MANIFEST" && return 0
  sed -i "s#<application#<uses-permission android:name=\"$name\" />\n    <application#" "$MANIFEST"
}
add_permission android.permission.ACCESS_NETWORK_STATE
add_permission android.permission.ACCESS_WIFI_STATE
add_permission android.permission.CHANGE_WIFI_MULTICAST_STATE
add_permission android.permission.ACCESS_LOCAL_NETWORK

# Резервное копирование Android (Google Диск и перенос на новый телефон) выключено: база зашифрована
# ключом из Keystore, который на другой телефон не переносится, — копия там всё равно не откроется.
# Для переноса — свои резервные копии приложения и связь по Wi-Fi.
if ! grep -q 'android:allowBackup' "$MANIFEST"; then
  sed -i 's#<application#<application\n        android:allowBackup="false"\n        android:dataExtractionRules="@xml/data_extraction_rules"#' "$MANIFEST"
fi
mkdir -p "$MAIN/res/xml"
cat > "$MAIN/res/xml/data_extraction_rules.xml" <<'XML'
<?xml version="1.0" encoding="utf-8"?>
<data-extraction-rules>
    <cloud-backup>
        <exclude domain="root" path="." />
        <exclude domain="file" path="." />
        <exclude domain="database" path="." />
        <exclude domain="sharedpref" path="." />
        <exclude domain="external" path="." />
    </cloud-backup>
    <device-transfer>
        <exclude domain="root" path="." />
        <exclude domain="file" path="." />
        <exclude domain="database" path="." />
        <exclude domain="sharedpref" path="." />
        <exclude domain="external" path="." />
    </device-transfer>
</data-extraction-rules>
XML

# Пока targetSdk 36. Приложениям с targetSdk 37 на Android 17 нужно отдельно запрашивать доступ
# к локальной сети — перейдём, когда добавим этот запрос.
GRADLE="$PROJECT/app/build.gradle.kts"
[ -f "$GRADLE" ] && sed -i -E 's/targetSdk = [0-9]+/targetSdk = 36/' "$GRADLE"

echo "Android-проект донастроен (общая часть kit):"
grep -E "uses-permission|allowBackup|dataExtractionRules|targetSdk|minSdk" -r "$MANIFEST" "$GRADLE" 2>/dev/null || true
