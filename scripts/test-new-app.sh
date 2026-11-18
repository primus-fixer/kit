#!/usr/bin/env bash
# Проверка заготовки нового приложения: из template/ создаётся приложение с kit из этой папки (а не с GitHub),
# и оно проходит свои проверки — ядро и мост (cargo test) и интерфейс (сборка с проверкой типов).
#
#   bash scripts/test-new-app.sh            # всё
#   bash scripts/test-new-app.sh --no-cargo # только создание и интерфейс
set -euo pipefail

KIT="$(cd "$(dirname "$0")/.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
CARGO=1
[ "${1:-}" = "--no-cargo" ] && CARGO=0

# Пакет интерфейса kit — архивом, как его ставит npm с GitHub.
(cd "$KIT" && npm pack --silent --pack-destination "$WORK" > /dev/null)
TGZ="$(find "$WORK" -maxdepth 1 -name 'primus-fixer-kit-*.tgz' | head -n 1)"

python3 "$KIT/scripts/new-app.py" --id kitdemo --latin KitDemo --name "Пример" --acc "Пример" --gen "Примера" \
  --tag KDM --tcp 47590 --udp 47591 --desc "Пример приложения на kit" \
  --kit-rev 0000000 --kit-path "$KIT" --kit-npm "file:$TGZ" "$WORK/demo"

# Повторно в ту же папку — отказ: файлы уже есть.
if python3 "$KIT/scripts/new-app.py" --id kitdemo --latin KitDemo --name "Пример" --acc "Пример" --gen "Примера" \
  --tag KDM --tcp 47590 --udp 47591 --desc "Пример" --kit-rev 0000000 "$WORK/demo" 2> /dev/null; then
  echo "new-app перезаписал готовое приложение без --force" >&2
  exit 1
fi
# Занятые порты и метки других приложений — отказ.
if python3 "$KIT/scripts/new-app.py" --id other --latin Other --name "Другое" --acc "Другое" --gen "Другого" \
  --tag PLK --tcp 47592 --udp 47593 --desc "Другое" --kit-rev 0000000 "$WORK/other" 2> /dev/null; then
  echo "new-app взял метку «Полки»" >&2
  exit 1
fi

grep -q 'kitdemo-db-key-v1' "$WORK/demo/app/src-tauri/src/names.rs"
grep -q 'name: "Пример", name_acc: "Пример", name_gen: "Примера", feminine: false' "$WORK/demo/crates/kitdemo-core/src/lib.rs"

(cd "$WORK/demo/app" && npm install --no-audit --no-fund --loglevel=error && npm run build)

if [ "$CARGO" = 1 ]; then
  # Библиотеки kit собраны и для его проверок: общая папка сборки.
  (cd "$WORK/demo" && CARGO_TARGET_DIR="$KIT/target/new-app" cargo test --workspace)
fi
echo "Заготовка: приложение создаётся и проходит свои проверки"
