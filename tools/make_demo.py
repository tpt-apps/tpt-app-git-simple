#!/usr/bin/env python3
"""Generates the embedded demo repository (demo_repo.bin).

Builds a small, relatable git repo (a fictional bakery website) with the
git CLI in a temp directory, then serializes EVERY file — worktree plus
.git — into the binary format the app embeds with include_bytes!:

    u32 LE entry count, then per entry:
    u32 LE path length, path (UTF-8), u64 LE data length, data bytes

Run from the repo root:  python tools/make_demo.py
"""

import hashlib
import os
import shutil
import subprocess
import sys
import tempfile

REPO = "sunrise-bakery-website"

FILES = {
    "index.html": (
        "<!doctype html>\n<html>\n<head><link rel=stylesheet href=css/style.css></head>\n"
        "<body><h1>Sunrise Bakery</h1><p>Fresh bread every morning.</p></body>\n</html>\n"
    ),
    "css/style.css": (
        "body { font-family: sans-serif; color: #333 }\nh1 { color: #b3661f }\n"
    ),
}

MENU_V1 = (
    "<!doctype html>\n<html><head><link rel=stylesheet href=css/style.css></head>\n"
    "<body><h1>Menu</h1><ul><li>Sourdough loaf — $6</li><li>Croissant — $3</li></ul></body>\n</html>\n"
)
MENU_V2 = (
    "<!doctype html>\n<html><head><link rel=stylesheet href=css/style.css></head>\n"
    "<body><h1>Menu</h1><ul><li>Sourdough loaf — $6.50</li><li>Croissant — $3</li>"
    "<li>Cinnamon roll — $3.50</li></ul></body>\n</html>\n"
)
CSS_V2 = (
    "body { font-family: sans-serif; color: #333 }\nh1 { color: #b3661f }\n"
    "ul { line-height: 1.6 }\n"
)
CSS_REDESIGN = (
    "body { font-family: sans-serif; color: #2a2a2a; background: #fdf6ec }\n"
    "h1 { color: #7a4b8f }\nul { line-height: 1.6 }\n"
)
ABOUT = (
    "<!doctype html>\n<html><body><h1>About us</h1>"
    "<p>Family-owned since 1998, on the corner of Willis and Manners.</p></body>\n</html>\n"
)
INDEX_V2 = (
    "<!doctype html>\n<html>\n<head><link rel=stylesheet href=css/style.css></head>\n"
    "<body><h1>Sunrise Bakery</h1><p>Warm bread, every morning from 7am.</p></body>\n</html>\n"
)


def run(*args):
    subprocess.run(["git", *args], check=True, capture_output=True)


def write(path, content):
    os.makedirs(os.path.dirname(path), exist_ok=True) if "/" in path else None
    with open(path, "w", newline="\n") as f:
        f.write(content)


def main():
    tmp = tempfile.mkdtemp(prefix="gitsimple-demo-")
    try:
        repo = os.path.join(tmp, REPO)
        os.makedirs(repo)
        os.chdir(repo)

        def commit_all(msg):
            run("add", "-A")
            run("commit", "-m", msg,
                "--author=Ana Baker <ana@sunrisebakery.example>")

        run("init", "-b", "main")

        for name, content in FILES.items():
            write(name, content)
        commit_all("Add homepage and stylesheet")

        write("menu.html", MENU_V1)
        commit_all("Add the menu page")

        write("menu.html", MENU_V2)
        write("css/style.css", CSS_V2)
        commit_all("Add cinnamon rolls and update spring prices")

        run("checkout", "-b", "redesign")
        write("css/style.css", CSS_REDESIGN)
        commit_all("Try a warmer color scheme")
        run("checkout", "main")

        write("about.html", ABOUT)
        commit_all("Add the about page")

        write("index.html", INDEX_V2)
        commit_all("Fix the welcome text and opening hours")

        # Serialize worktree + .git into demo_repo.bin.
        entries = []
        for root, dirs, files in os.walk("."):
            dirs[:] = [d for d in dirs if d != ".git"] or dirs
            for name in files:
                full = os.path.join(root, name)
                rel = os.path.relpath(full, ".").replace("\\", "/")
                if rel.startswith(".git/") or rel == ".git":
                    continue
                entries.append((rel, open(full, "rb").read()))
        for root, dirs, files in os.walk(".git"):
            for name in files:
                full = os.path.join(root, name)
                rel = os.path.relpath(full, ".").replace("\\", "/")
                entries.append((rel, open(full, "rb").read()))
        entries.sort()

        out = bytearray()
        out += len(entries).to_bytes(4, "little")
        for rel, data in entries:
            encoded = rel.encode()
            out += len(encoded).to_bytes(4, "little")
            out += encoded
            out += len(data).to_bytes(8, "little")
            out += data

        dest = os.path.join(os.path.dirname(__file__), "..", "src", "demo_repo.bin")
        with open(dest, "wb") as f:
            f.write(out)
        n_objects = sum(1 for rel, _ in entries if rel.startswith(".git/objects/"))
        print(f"demo_repo.bin: {len(entries)} files, {len(out)} bytes, {n_objects} git objects")
    finally:
        os.chdir(os.path.dirname(__file__))
        shutil.rmtree(tmp, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
