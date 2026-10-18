#!/usr/bin/env python3
"""One-shot namespace sweep for the TerraZone rebrand (Task 7).

Replaces the old developer namespaces across text files and renames the
packaging/app-icon files that carry the old ids. Run from the repo root:

    python3 packaging/rebrand_sweep.py
"""
from __future__ import annotations

import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# Text replacements, longest first (so compound strings win).
TEXT_RULES = [
    ("app.terrazon.terrazon", "app.terrazon.terrazon"),
    ("app.terrazon.terrazon", "app.terrazon.terrazon"),
    ("app.terrazon", "app.terrazon"),
    ("maintainers@terrazon.app", "maintainers@terrazon.app"),
    ("github.com/terrazon-app/terrazon", "github.com/terrazon-app/terrazon"),
    ("github.com/terrazon-app/terrazon", "github.com/terrazon-app/terrazon"),
    ("terrazon-app/terrazon", "terrazon-app/terrazon"),
    ("terrazon-app/terrazon", "terrazon-app/terrazon"),
    ("org.terrazon", "org.terrazon"),
]

TEXT_EXT = {
    ".rs", ".toml", ".md", ".yml", ".yaml", ".xml", ".in", ".desktop", ".sh",
    ".py", ".json", ".txt", ".tsv", ".svg", ".metainfo", ".mime", ".plist",
    ".service", ".css", ".html", ".js", ".gitignore",
}

# Exact file renames (old name -> new name), relative to ROOT.
FILE_RENAMES = [
    "packaging/linux/app.terrazon.terrazon.desktop",
    "packaging/linux/app.terrazon.terrazon.metainfo.xml.in",
    "packaging/linux/app.terrazon.terrazon.mime.xml",
    "packaging/linux/flatpak/app.terrazon.terrazon.yml",
    "packaging/linux/flatpak/app.terrazon.terrazon.bundle.yml",
]

SKIP_DIRS = {"target", ".git", "node_modules"}


def main() -> None:
    changed = 0
    for p in ROOT.rglob("*"):
        if any(part in SKIP_DIRS for part in p.parts):
            continue
        if not p.is_file() or p.suffix not in TEXT_EXT and p.name not in {
            "INSTALL.txt", "NOTICE", "LICENSE-MIT", "LICENSE-APACHE", "clippy.toml",
        }:
            continue
        try:
            text = p.read_text(encoding="utf-8")
        except (UnicodeDecodeError, PermissionError):
            continue
        new = text
        for old, repl in TEXT_RULES:
            new = new.replace(old, repl)
        if new != text:
            p.write_text(new, encoding="utf-8")
            changed += 1
            print(f"edited  {p.relative_to(ROOT)}")
    print(f"--- {changed} files edited")

    for rel in FILE_RENAMES:
        old = ROOT / rel
        newp = ROOT / re.sub(r"ai\.storyteller\.(terrazon|terrazon)", "app.terrazon.terrazon", rel)
        if old.exists():
            newp.parent.mkdir(parents=True, exist_ok=True)
            old.rename(newp)
            print(f"renamed {rel} -> {newp.relative_to(ROOT)}")
    # The app-id files inside hicolor (new pngs already use app.terrazon.terrazon).
    for p in (ROOT / "assets/app-icon/hicolor").rglob("app.terrazon.terrazon.*"):
        np = p.with_name(p.name.replace("app.terrazon.terrazon", "app.terrazon.terrazon"))
        p.rename(np)
        print(f"renamed {p.relative_to(ROOT)} -> {np.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
