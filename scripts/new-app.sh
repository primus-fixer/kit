#!/usr/bin/env bash
# Новое приложение из заготовки kit (template/): копия заготовки со своим названием, портами и метками,
# ссылки на kit — на коммит, из которого её сделали.
#
#   bash scripts/new-app.sh <папка> <id> <Название> <Винительный> <Родительный> <ж|м> <порт> <МЕТКА>
#
#   <id>        латиницей, строчными: имя программы, базы, ключей и пакетов (most → most-core, most.db);
#   <Название>  и два падежа — для сообщений: «Мост открыт», «откройте „Мост“», «данные „Моста“»;
#   <ж|м>       род названия: «Полка открыта» — ж, «Мост открыт» — м;
#   <порт>      порт обмена по Wi-Fi, следующий — поиск в сети; занятые: 47500 («Карман»), 47510 («Полка»), 47520 (заготовка);
#   <МЕТКА>     три заглавные латинские буквы — метки пакетов Wi-Fi и файлов облака (у «Кармана» KRM, у «Полки» PLK).
#
# Пример: bash scripts/new-app.sh ../most most Мост Мост Моста м 47530 MST
#
# **Эти настройки у выпущенного приложения не меняются:** иначе устройства перестанут узнавать друг друга,
# а база, облако и копии — открываться. Поэтому выбирайте их сразу.
set -euo pipefail

if [ $# -ne 8 ]; then
  sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'
  exit 2
fi

DEST="$1" ID="$2" NAME="$3" ACC="$4" GEN="$5" SEX="$6" PORT="$7" MARK="$8"
KIT="$(cd "$(dirname "$0")/.." && pwd)"

fail() { echo "new-app: $*" >&2; exit 1; }
[[ "$ID" =~ ^[a-z][a-z0-9]{1,15}$ ]] || fail "id — латиница строчными и цифры, 2–16 знаков, с буквы: «$ID»"
[ "$ID" != starter ] || fail "id «starter» занят заготовкой"
[[ "$MARK" =~ ^[A-Z]{3}$ ]] || fail "метка — три заглавные латинские буквы: «$MARK»"
case "$MARK" in KRM | PLK | ZGT) fail "метка $MARK занята («Карман», «Полка», заготовка)" ;; esac
[[ "$PORT" =~ ^[0-9]+$ ]] && [ "$PORT" -ge 1024 ] && [ "$PORT" -le 65534 ] || fail "порт — число от 1024 до 65534: «$PORT»"
case "$PORT" in 47500 | 47501 | 47510 | 47511 | 47520 | 47521) fail "порт $PORT занят" ;; esac
case "$SEX" in ж) FEM=true ;; м) FEM=false ;; *) fail "род — ж или м: «$SEX»" ;; esac
[ ! -e "$DEST" ] || fail "папка $DEST уже есть"

REV="$(git -C "$KIT" rev-parse HEAD)"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$KIT/Cargo.toml" | head -1)"
if [ -n "$(git -C "$KIT" status --porcelain)" ]; then
  echo "new-app: в kit есть неотправленные правки — приложение сошлётся на коммит $REV без них" >&2
fi

UNDER="${ID//-/_}"
CAP="$(printf '%s' "${ID:0:1}" | tr '[:lower:]' '[:upper:]')${ID:1}"
LOWER="$(printf '%s' "$NAME" | perl -CSA -ne 'print lc')"
BAK="$(printf '%s' "$ID" | tr '[:lower:]' '[:upper:]' | tr -cd '[:upper:]' | cut -c1-5)"
while [ ${#BAK} -lt 5 ]; do BAK="${BAK}X"; done
BAK="${BAK}BAK"
EXT="$(printf '%s' "${MARK:0:1}" | tr '[:upper:]' '[:lower:]')"

mkdir -p "$DEST"
# Копия без сборок и пакетов.
(cd "$KIT/template" && tar --exclude=./target --exclude=./Cargo.lock --exclude=./app/node_modules --exclude=./app/package-lock.json --exclude=./app/dist --exclude=./app/src-tauri/gen -cf - .) | (cd "$DEST" && tar -xf -)
git -C "$KIT" show "$REV:template/crates/starter-core/Cargo.toml" > /dev/null 2>&1 || echo "new-app: заготовки нет в коммите $REV — отправьте kit на GitHub" >&2

mv "$DEST/crates/starter-core" "$DEST/crates/$ID-core"

export ID UNDER CAP NAME ACC GEN LOWER FEM PORT PORT2=$((PORT + 1)) MARK BAK EXT REV VERSION
# Замены — в perl: значения приходят через окружение, поэтому выражения в одинарных кавычках.
# shellcheck disable=SC2016
find "$DEST" -type f \( -name '*.rs' -o -name '*.toml' -o -name '*.json' -o -name '*.ts' -o -name '*.tsx' -o -name '*.html' -o -name '*.css' -o -name '*.yml' -o -name '*.md' -o -name '.gitignore' \) -print0 |
  xargs -0 perl -Mutf8 -MEncode -CSD -pi -e '
    BEGIN { our %W = map { $_ => Encode::decode_utf8($ENV{$_}) } qw(NAME ACC GEN LOWER) }
    # Ссылки на kit: папки внутри kit → коммит на GitHub.
    s{path = "(?:\.\./)+crates/(kit-[a-z]+)"(.*?) # KIT$}{git = "https://github.com/primus-fixer/kit", rev = "$ENV{REV}"$2};
    s{"\@primus-fixer/kit": "file:\.\./\.\."}{"\@primus-fixer/kit": "github:primus-fixer/kit#$ENV{REV}"};
    s{KIT_REV}{$ENV{REV}}g; s{KIT_VERSION}{$ENV{VERSION}}g;
    # Название, падежи и род.
    s{Заготовку}{$W{ACC}}g; s{Заготовки}{$W{GEN}}g; s{Заготовка}{$W{NAME}}g; s{заготовка"}{$W{LOWER}"}g;
    s{feminine: true}{feminine: $ENV{FEM}}g;
    # Имена программы, пакетов, ключей и файлов.
    s{starter_}{$ENV{UNDER}_}g; s{starter}{$ENV{ID}}g; s{Starter}{$ENV{CAP}}g;
    # Порты, метки, расширения файлов облака, метка копий.
    s{47520}{$ENV{PORT}}g; s{47521}{$ENV{PORT2}}g;
    s{ZGT([LQAC])}{$ENV{MARK}$1}g; s{"\.z(ev|bl)"}{".$ENV{EXT}$1"}g; s{STARTBAK}{$ENV{BAK}}g;
  '

# Слова «заготовки» в описаниях — уже про своё приложение.
perl -Mutf8 -CSD -pi -e 's{ \(сделано из заготовки kit, template/\)}{ (сделано из заготовки kit)}' "$DEST/crates/$ID-core/src/lib.rs"

LEFT="$(grep -rIl -e starter -e ZGT -e STARTBAK -e 47520 "$DEST" 2>/dev/null || true)"
[ -z "$LEFT" ] || fail "остались слова заготовки в: $LEFT"

# Файлы версий библиотек (NEW_APP_NO_LOCKS=1 — без них, для проверки скрипта).
if [ -z "${NEW_APP_NO_LOCKS:-}" ] && command -v npm > /dev/null; then
  (cd "$DEST/app" && npm install --package-lock-only --no-audit --no-fund > /dev/null) || echo "new-app: npm не составил package-lock.json — сделайте npm install в app/" >&2
fi
if [ -z "${NEW_APP_NO_LOCKS:-}" ] && command -v cargo > /dev/null; then
  (cd "$DEST" && cargo generate-lockfile -q) || echo "new-app: cargo не составил Cargo.lock — он появится при первой сборке" >&2
fi

cat <<EOF
Готово: $DEST — «$NAME» на kit $VERSION ($REV).
Дальше:
  cd $DEST && git init && git add -A && git commit -m "«$NAME» из заготовки kit $VERSION"
  cd app && npm install && npm run tauri dev
Записи — crates/$ID-core/src/model.rs и экраны app/src/screens; что ещё поменять — README.md.
EOF
