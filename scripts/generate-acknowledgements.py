#!/usr/bin/env python3
"""Convert cargo-about output and bundled asset notices into the offline catalogue."""
import hashlib
import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[1]


def generate(report):
    texts, entries, seen = [], [], {}

    def intern(text):
        text = text.strip() + "\n"
        if text not in seen:
            seen[text] = len(texts)
            texts.append(text)
        return seen[text]

    packages = {}
    for license in report["licenses"]:
        index = intern(license["text"])
        for user in license["used_by"]:
            crate = user["crate"]
            entry = packages.setdefault(crate["id"], {
                "name": crate["name"], "version": crate["version"],
                "license_ids": set(), "texts": [],
                "source": crate.get("repository") or crate.get("homepage") or "",
            })
            entry["license_ids"].add(license["id"])
            if index not in entry["texts"]:
                entry["texts"].append(index)
    missing = {c["package"]["id"] for c in report["crates"]} - packages.keys()
    if missing:
        raise RuntimeError(f"Missing notices: {sorted(missing)}")
    for entry in packages.values():
        entry["license"] = " AND ".join(sorted(entry.pop("license_ids")))
        entries.append(entry)
    for name, license, source, path in [
        ("JetBrains Mono Nerd Font", "MIT AND OFL-1.1", "https://github.com/ryanoasis/nerd-fonts", "assets/fonts/JETBRAINS_MONO_NERD_FONT_LICENSE.txt"),
        ("M PLUS 1", "OFL-1.1", "https://github.com/coz-m/MPLUS_FONTS", "assets/fonts/MPLUS_FONTS_OFL.txt"),
        ("Noto Sans CJK JP", "OFL-1.1", "https://github.com/notofonts/noto-cjk", "assets/fonts/NOTO_SANS_CJK_OFL.txt"),
        ("Dracula palette", "MIT", "https://github.com/dracula/dracula-theme", "assets/themes/dracula.LICENSE"),
        ("Monokai palette", "MIT", "https://github.com/microsoft/vscode", "assets/themes/monokai.LICENSE"),
        ("Nord palette", "MIT", "https://github.com/nordtheme/nord", "assets/themes/nord.LICENSE"),
        ("Solarized palettes", "MIT", "https://github.com/altercation/solarized", "assets/themes/solarized.LICENSE"),
        ("Tokyo Night palette", "Apache-2.0", "https://github.com/folke/tokyonight.nvim", "assets/themes/tokyo-night.LICENSE"),
        ("Bundled palettes", "See attribution", "", "assets/themes/README.md"),
    ]:
        entries.append({"name": name, "version": "", "license": license, "source": source, "texts": [intern((ROOT / path).read_text())]})
    return {"generator": "cargo-about", "cargo_lock_sha256": hashlib.sha256((ROOT / "Cargo.lock").read_bytes()).hexdigest(),
            "entries": sorted(entries, key=lambda e: (e['name'].lower(), e['version'], e['source'])), "texts": texts}


if __name__ == "__main__":
    report = json.loads(subprocess.check_output([
        "cargo", "about", "generate", "--locked", "--fail", "--format", "json",
    ], cwd=ROOT, text=True))
    result = generate(report)
    output = ROOT / "assets/acknowledgements.json"
    output.write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n")
    print(f"Wrote {len(result['entries'])} entries and {len(result['texts'])} license texts to {output}")
