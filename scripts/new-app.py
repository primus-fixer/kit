#!/usr/bin/env python3
"""Новое приложение на общем модуле kit — из заготовки `template/`.

    python3 scripts/new-app.py --id yashik --latin Yashik \\
        --name Ящик --acc Ящик --gen Ящика [--feminine] \\
        --tag YSH --tcp 47520 --udp 47521 \\
        --desc "Ящик — почта: все ящики в одном месте" \\
        --kit-rev <коммит или метка kit> ПАПКА

Что получается: ядро `crates/<id>-core` (название, порты, метки, имена файлов), приложение Tauri со входом
по PIN-коду, обменом по Wi-Fi и облаком, экраны (главный и настройки), сценарии сборок для Windows и Android.
Папка может быть пустой или клоном пустого репозитория; файлы, которые уже есть, не перезаписываются
(кроме `--force`).

Строки, которые задаются здесь (`--id`, `--latin`, `--tag`, порты), у выпущенного приложения не меняются:
с другими устройства не откроют свою базу и перестанут узнавать друг друга.

Для проверок заготовки (scripts/test-new-app.sh): `--kit-path` — kit из этой папки вместо GitHub,
`--kit-npm` — пакет интерфейса (например, `file:…/primus-fixer-kit-0.12.0.tgz`).
"""

import argparse
import re
import shutil
import sys
from pathlib import Path

KIT = Path(__file__).resolve().parent.parent
TEMPLATE = KIT / "template"

TEXT_SUFFIXES = {".rs", ".toml", ".json", ".ts", ".tsx", ".css", ".html", ".md", ".yml", ".yaml", ".sh", ".svg", ".xml"}
TEXT_NAMES = {".gitignore"}

# Что уже занято другими приложениями на kit: в одной сети и в одном облаке они не должны мешать друг другу.
TAKEN_IDS = {"kit", "karman", "polka", "most", "bridge"}
TAKEN_TAGS = {"KRM": "«Карман»", "PLK": "«Полка»"}
TAKEN_PORTS = {47500: "«Карман»", 47501: "«Карман»", 47510: "«Полка»", 47511: "«Полка»"}

KIT_CRATES = ("kit-core", "kit-secure", "kit-tauri", "kit-cloud")
# Имена с двойным подчёркиванием, которые не заполняются: их подставляют Tauri и Vite.
NOT_TOKENS = {"__TAURI_INTERNALS__", "__APP_VERSION__"}


def fail(text: str) -> None:
    print(f"new-app: {text}", file=sys.stderr)
    sys.exit(2)


def kit_version() -> str:
    m = re.search(r'^version = "([^"]+)"', (KIT / "Cargo.toml").read_text(encoding="utf-8"), re.M)
    return m.group(1) if m else "?"


def plain(value: str, what: str) -> str:
    """Строка для литерала Rust и TypeScript: без кавычек, обратной черты и подстановок."""
    value = value.strip()
    if not value:
        fail(f"{what}: пусто")
    if any(c in value for c in "\"'`\\\n\r") or "${" in value:
        fail(f"{what}: без кавычек, обратной черты и переводов строки")
    return value


def check(args: argparse.Namespace) -> dict:
    if not re.fullmatch(r"[a-z][a-z0-9]{2,15}", args.id):
        fail("--id: латиницей, маленькими буквами и цифрами, 3–16 знаков, первая — буква (yashik)")
    if args.id in TAKEN_IDS:
        fail(f"--id: «{args.id}» уже занят")
    if not re.fullmatch(r"[A-Za-z][A-Za-z0-9]{1,31}", args.latin):
        fail("--latin: название латиницей, буквы и цифры (Yashik)")
    if args.latin.lower() != args.id:
        fail("--latin: то же название, что --id, только с заглавными буквами")
    if not re.fullmatch(r"[A-Z]{3}", args.tag):
        fail("--tag: три заглавные латинские буквы (YSH) — из них метки пакетов и файлов")
    if args.tag in TAKEN_TAGS:
        fail(f"--tag: {args.tag} занят приложением {TAKEN_TAGS[args.tag]}")
    for port, what in ((args.tcp, "--tcp"), (args.udp, "--udp")):
        if not 1024 <= port <= 65535:
            fail(f"{what}: порт от 1024 до 65535")
        if port in TAKEN_PORTS:
            fail(f"{what}: порт {port} занят приложением {TAKEN_PORTS[port]}")
    if args.tcp == args.udp:
        fail("--tcp и --udp: разные порты")
    if not re.fullmatch(r"[0-9a-f]{7,40}|v\d+\.\d+\.\d+", args.kit_rev):
        fail("--kit-rev: коммит kit (шестнадцатеричный) или метка вида v0.12.0")
    name = plain(args.name, "--name")
    return {
        "__ID_UPPER__": args.id.upper(),
        "__ID__": args.id,
        "__CRATE__": f"{args.id}_core",
        "__APPLIB__": f"{args.id}_app_lib",
        "__LATIN__": args.latin,
        "__NAME_ACC__": plain(args.acc, "--acc"),
        "__NAME_GEN__": plain(args.gen, "--gen"),
        "__NAME_LC__": name.lower(),
        "__NAME__": name,
        "__FEMININE__": "true" if args.feminine else "false",
        "__DESCRIPTION__": plain(args.desc, "--desc"),
        "__TCP_PORT__": str(args.tcp),
        "__UDP_PORT__": str(args.udp),
        "__TAG__": args.tag,
        "__E__": args.id[0],
        "__MAGIC8__": args.id.upper()[:6].ljust(6, "X") + "BK",
        "__KIT_REV__": args.kit_rev,
        "__KIT_VERSION__": kit_version(),
    }


def fill(text: str, tokens: dict) -> str:
    for key in sorted(tokens, key=len, reverse=True):
        text = text.replace(key, tokens[key])
    return text


def kit_sources(text: str, rel: str, args: argparse.Namespace) -> str:
    """Откуда брать kit: метка — `tag` вместо `rev`; для проверок — папка kit и свой пакет интерфейса."""
    if rel.endswith("Cargo.toml"):
        if args.kit_path:
            for crate in KIT_CRATES:
                path = (Path(args.kit_path).resolve() / "crates" / crate).as_posix()
                text = re.sub(rf'^({crate} = \{{ )git = "https://github.com/primus-fixer/kit", rev = "[^"]+"', rf'\g<1>path = "{path}"', text, flags=re.M)
        elif args.kit_rev.startswith("v"):
            text = text.replace(f'rev = "{args.kit_rev}"', f'tag = "{args.kit_rev}"')
    if rel == "app/package.json" and args.kit_npm:
        text = re.sub(r'"@primus-fixer/kit": "[^"]+"', f'"@primus-fixer/kit": "{args.kit_npm}"', text)
    return text


def main() -> None:
    p = argparse.ArgumentParser(description="Новое приложение на kit из заготовки template/")
    p.add_argument("--id", required=True, help="латиницей, маленькими буквами: yashik")
    p.add_argument("--latin", required=True, help="латиницей, как пишется: Yashik")
    p.add_argument("--name", required=True, help="название, именительный падеж: Ящик")
    p.add_argument("--acc", required=True, help="винительный падеж: (вход в) Ящик")
    p.add_argument("--gen", required=True, help="родительный падеж: (PIN-код) Ящика")
    p.add_argument("--feminine", action="store_true", help="название женского рода: «Полка открыта»")
    p.add_argument("--tag", required=True, help="три заглавные буквы для меток: YSH")
    p.add_argument("--tcp", type=int, required=True, help="порт обмена по Wi-Fi (TCP)")
    p.add_argument("--udp", type=int, required=True, help="порт поиска в сети (UDP)")
    p.add_argument("--desc", required=True, help="одна строка о приложении")
    p.add_argument("--kit-rev", required=True, help="коммит или метка kit")
    p.add_argument("--kit-path", help="для проверок: kit из этой папки")
    p.add_argument("--kit-npm", help="для проверок: пакет интерфейса kit")
    p.add_argument("--force", action="store_true", help="перезаписать файлы, которые уже есть")
    p.add_argument("dest", help="папка нового приложения")
    args = p.parse_args()
    tokens = check(args)

    dest = Path(args.dest).resolve()
    plan = []
    for src in sorted(TEMPLATE.rglob("*")):
        if src.is_dir():
            continue
        rel = src.relative_to(TEMPLATE).as_posix()
        out_rel = rel.replace("__ID__", args.id)
        out = dest / out_rel
        if out.exists() and not args.force:
            fail(f"{out_rel} уже есть — уберите его или запустите с --force")
        plan.append((src, out, out_rel))

    for src, out, out_rel in plan:
        out.parent.mkdir(parents=True, exist_ok=True)
        if src.suffix in TEXT_SUFFIXES or src.name in TEXT_NAMES:
            text = kit_sources(fill(src.read_text(encoding="utf-8"), tokens), out_rel, args)
            left = sorted(set(re.findall(r"__[A-Z][A-Z0-9_]*__", text)) - NOT_TOKENS)
            if left:
                fail(f"{out_rel}: не заполнено {', '.join(left)}")
            out.write_text(text, encoding="utf-8")
        else:
            shutil.copyfile(src, out)

    print(f"Готово: «{tokens['__NAME__']}» в {dest} ({len(plan)} файлов, kit {tokens['__KIT_VERSION__']}).")
    print("Дальше:")
    print(f"  cd {dest}/app && npm install      # package-lock.json — в репозиторий")
    print(f"  cd {dest} && cargo test            # Cargo.lock — в репозиторий")
    print("  npm run tauri dev                  # в app/: приложение; npm run dev — демо в браузере")


if __name__ == "__main__":
    main()
