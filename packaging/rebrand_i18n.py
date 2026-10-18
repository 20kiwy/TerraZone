#!/usr/bin/env python3
"""Update the i18n catalogs for the TerraZone link/brand rebrand (Task 7).

Per catalog (crates/ui-egui/src/i18n/*.tsv):
  * drop rows whose English source is gone from the UI
    (ArtCraft labels, getartcraft, Discord strings, the OSM community label);
  * rewrite the About strings that changed
    ("Part of ArtCraft. ..." -> "TerraZone. ...",
     "vector illustration" -> "map illustration" in the version tagline);
  * add the new TerraZone community / website rows in every language.
"""
from __future__ import annotations

from pathlib import Path

DIR = Path("/home/z/terrazon/terrazon/crates/ui-egui/src/i18n")

DROP = {
    "ArtCraft Website",
    "ArtCraft website",
    "TerraZone on terrazon.app",
    "Discord",
    "Join Our Discord",
    "Join our Discord",
    "Join the ArtCraft community on Discord (discord.gg/artcraft)",
    "Join the OpenStreetMap community (community.openstreetmap.org)",
    "OpenStreetMap Community",
}

OLD_ABOUT = "Part of ArtCraft. MIT OR Apache-2.0. Fonts: Source Sans 3, Inter, JetBrains Mono (OFL). Icons: Lucide (ISC) + TerraZone."
OLD_ABOUT_ZH = "Part of ArtCraft. MIT OR Apache-2.0. Fonts: Source Sans 3, Inter, JetBrains Mono, Noto Sans SC (OFL). Icons: Lucide (ISC) + TerraZone."
NEW_ABOUT = "TerraZone. MIT OR Apache-2.0. Fonts: Source Sans 3, Inter, JetBrains Mono (OFL). Icons: Lucide (ISC) + TerraZone."

ABOUT_TR = {
    "cs": "TerraZone. MIT OR Apache-2.0. Písma: Source Sans 3, Inter, JetBrains Mono (OFL). Ikony: Lucide (ISC) + TerraZone.",
    "es": "TerraZone. MIT OR Apache-2.0. Fuentes: Source Sans 3, Inter, JetBrains Mono (OFL). Iconos: Lucide (ISC) + TerraZone.",
    "fr": "TerraZone. MIT OR Apache-2.0. Polices : Source Sans 3, Inter, JetBrains Mono (OFL). Icônes : Lucide (ISC) + TerraZone.",
    "it": "TerraZone. MIT OR Apache-2.0. Font: Source Sans 3, Inter, JetBrains Mono (OFL). Icone: Lucide (ISC) + TerraZone.",
    "ja": "TerraZone。MIT OR Apache-2.0。フォント：Source Sans 3、Inter、JetBrains Mono (OFL)。アイコン：Lucide (ISC) + TerraZone。",
    "pt-br": "TerraZone. MIT OR Apache-2.0. Fontes: Source Sans 3, Inter, JetBrains Mono (OFL). Ícones: Lucide (ISC) + TerraZone.",
    "ru": "TerraZone. MIT OR Apache-2.0. Шрифты: Source Sans 3, Inter, JetBrains Mono (OFL). Значки: Lucide (ISC) + TerraZone.",
    "uk": "TerraZone. MIT OR Apache-2.0. Шрифти: Source Sans 3, Inter, JetBrains Mono (OFL). Значки: Lucide (ISC) + TerraZone.",
    "zh-hans": "TerraZone。MIT OR Apache-2.0。字体：Source Sans 3、Inter、JetBrains Mono (OFL)。图标：Lucide (ISC) + TerraZone。",
    "zh-hant": "TerraZone。MIT OR Apache-2.0。字體：Source Sans 3、Inter、JetBrains Mono (OFL)。圖示：Lucide (ISC) + TerraZone。",
}

OLD_TAG = "Version {version} — open-source vector illustration in pure Rust."
NEW_TAG = "Version {version} — open-source map illustration in pure Rust."

# word-level replacements for the tagline translation per language
TAG_WORD = {
    "cs": [("vektorová", "mapová"), ("vektoru", "map")],
    "es": [("ilustración vectorial", "ilustración cartográfica")],
    "fr": [("illustration vectorielle", "illustration cartographique")],
    "it": [("illustrazione vettoriale", "illustrazione cartografica")],
    "ja": [("ベクターイラスト", "マップイラスト"), ("ベクター", "マップ")],
    "pt-br": [("ilustração vetorial", "ilustração cartográfica")],
    "ru": [("векторной", "картографической")],
    "uk": [("векторній", "картографічній"), ("векторної", "картографічної"), ("векторна", "картографічна")],
    "zh-hans": [("矢量插画", "地图插画"), ("矢量", "地图")],
    "zh-hant": [("向量插畫", "地圖插畫"), ("向量", "地圖")],
}

NEW_ROWS = {
    "TerraZone Community": {
        "cs": "Komunita TerraZone", "es": "Comunidad TerraZone", "fr": "Communauté TerraZone",
        "it": "Comunità TerraZone", "ja": "TerraZone コミュニティ", "pt-br": "Comunidade TerraZone",
        "ru": "Сообщество TerraZone", "uk": "Спільнота TerraZone",
        "zh-hans": "TerraZone 社区", "zh-hant": "TerraZone 社群",
    },
    "Join the TerraZone community (terrazon.app/community)": {
        "cs": "Připojte se ke komunitě TerraZone (terrazon.app/community)",
        "es": "Únete a la comunidad de TerraZone (terrazon.app/community)",
        "fr": "Rejoignez la communauté TerraZone (terrazon.app/community)",
        "it": "Unisciti alla comunità di TerraZone (terrazon.app/community)",
        "ja": "TerraZone コミュニティに参加 (terrazon.app/community)",
        "pt-br": "Junte-se à comunidade do TerraZone (terrazon.app/community)",
        "ru": "Присоединяйтесь к сообществу TerraZone (terrazon.app/community)",
        "uk": "Приєднуйтесь до спільноти TerraZone (terrazon.app/community)",
        "zh-hans": "加入 TerraZone 社区 (terrazon.app/community)",
        "zh-hant": "加入 TerraZone 社群 (terrazon.app/community)",
    },
    "TerraZone Website": {
        "cs": "Web TerraZone", "es": "Sitio web de TerraZone", "fr": "Site web de TerraZone",
        "it": "Sito web di TerraZone", "ja": "TerraZone の Web サイト", "pt-br": "Site do TerraZone",
        "ru": "Веб-сайт TerraZone", "uk": "Веб-сайт TerraZone",
        "zh-hans": "TerraZone 网站", "zh-hant": "TerraZone 網站",
    },
}


def main() -> None:
    for f in sorted(DIR.glob("*.tsv")):
        lang = f.stem
        lines = f.read_text(encoding="utf-8").splitlines()
        out, dropped, rewritten, tagged = [], 0, 0, 0
        seen_sources = set()
        for line in lines:
            if line.startswith("#") or not line.strip():
                out.append(line)
                continue
            cols = line.split("\t")
            if len(cols) < 3:
                out.append(line)
                continue
            ctx, src, tr = cols[0], cols[1], cols[2]
            if ctx == "" and src in DROP:
                dropped += 1
                continue
            if ctx == "" and src in (OLD_ABOUT, OLD_ABOUT_ZH):
                out.append(f"\t{NEW_ABOUT}\t{ABOUT_TR[lang]}")
                rewritten += 1
                seen_sources.add(NEW_ABOUT)
                continue
            if ctx == "" and src == OLD_TAG:
                new_tr = tr
                for a, b in TAG_WORD.get(lang, []):
                    new_tr = new_tr.replace(a, b)
                out.append(f"\t{NEW_TAG}\t{new_tr}")
                tagged += 1
                seen_sources.add(NEW_TAG)
                continue
            seen_sources.add(src if ctx == "" else f"{ctx}\x01{src}")
            out.append(line)
        # append the new rows (community, hover text, website)
        added = 0
        for src, table in NEW_ROWS.items():
            if src not in seen_sources:
                out.append(f"\t{src}\t{table[lang]}")
                added += 1
        f.write_text("\n".join(out) + "\n", encoding="utf-8")
        print(f"{f.name}: dropped={dropped} about={rewritten} tagline={tagged} added={added}")


if __name__ == "__main__":
    main()
