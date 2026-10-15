#!/usr/bin/env python3
"""Synthesize a large OCA project for perf tests (architecture A10 budgets).

Deterministic (seeded): the same arguments always produce the same corpus, so
CI perf numbers are comparable across runs. Output is a *real* project layout
per `OCA-build.md §3` — Markdown/YAML on disk, no `.oca/` (the index is built
by `oca reindex`, which arrives in Phase 5).

Usage:
    scripts/generate-fixture.py [--words 500000] [--chapters 100] \
        [--characters 24] [--songs 12] [--seed 42] [--out DIR]
"""
from __future__ import annotations

import argparse
import json
import random
import re
import time
from pathlib import Path

# A deterministic, plausible English-ish pool — content doesn't matter for perf,
# only volume and structure do.
WORDS = (
    "the of and a to in is was he for it with as his on be at by i this had "
    "not are but from or have an they which one you were her all she there "
    "would their we him been has when who will more no if out so said what "
    "up its about into than them can only other new some could time these two "
    "may then do first any my now such like our over man me even most made "
    "after also did many before must through back years where much your way "
    "well down should because each just those people mr how too little state "
    "good very make world still own see men work long get here between both "
    "life being under never day same another know while last might us great "
    "old year off come since against go came right used take three night "
    "forest river mountain village shadow storm firelight silence voice name "
    "letter journey promise secret threshold winter ember lantern path home"
).split()

SECTIONS = [
    "Intro", "Verse", "Pre-Chorus", "Chorus", "Post-Chorus",
    "Bridge", "Breakdown", "Outro", "Interlude",
]

CHAPTER_TITLES = [
    "The Awakening", "Ashes", "The Long Road", "Whispers", "Blackwood",
    "The Crossing", "Embers", "What Was Lost", "The Return", "Storm",
]


def slug(text: str) -> str:
    return re.sub(r"[^a-z0-9]+", "-", text.lower()).strip("-")


class Gen:
    def __init__(self, rng: random.Random):
        self.rng = rng

    def words(self, n: int) -> list[str]:
        return [self.rng.choice(WORDS) for _ in range(n)]

    def sentence(self) -> str:
        n = self.rng.randint(6, 16)
        words = self.words(n)
        words[0] = words[0].capitalize()
        return " ".join(words) + self.rng.choice([".", ".", ".", "!", "?"])

    def paragraph(self, sentences: int = 6) -> str:
        return "\n\n".join(self.sentence() for _ in range(sentences))

    def paragraphs_for(self, target_words: int) -> list[str]:
        paras: list[str] = []
        count = 0
        while count < target_words:
            p = self.paragraph(self.rng.randint(4, 8))
            paras.append(p)
            count += len(p.split())
        return paras


def frontmatter(pairs: list[tuple[str, str]]) -> str:
    lines = ["---"] + [f"{k}: {v}" for k, v in pairs] + ["---", ""]
    return "\n".join(lines)


def write_chapter(gen: Gen, path: Path, doc_id: str, title: str,
                  chapter_no: int, words: int, pov: str) -> int:
    path.parent.mkdir(parents=True, exist_ok=True)
    body = "\n\n".join(gen.paragraphs_for(words))
    text = frontmatter([
        ("id", doc_id),
        ("title", title),
        ("chapter", str(chapter_no)),
        ("tags", f"[fixture, chapter{chapter_no}]"),
        ("status", "draft"),
        ("pov", pov),
    ]) + f"# {title}\n\n{body}\n"
    path.write_text(text, encoding="utf-8")
    return len(text.split())


def write_character(gen: Gen, path: Path, name: str, idx: int) -> int:
    path.parent.mkdir(parents=True, exist_ok=True)
    first = name.lower()
    states = "\n".join(
        f"  - at: ch_{i:03d}\n    location: place_{i % 7}\n    emotional: mood_{i % 5}"
        for i in (1, 4, 9) if i <= idx + 1
    ) or "  - at: ch_001\n    location: village\n    emotional: neutral"
    text = (
        "---\n"
        f"id: {first}\n"
        f"name: {name}\n"
        f"role: {'protagonist' if idx == 0 else 'supporting'}\n"
        f"aliases: [\"{first[:3]}y\", \"the one\"]\n"
        "states:\n"
        f"{states}\n"
        "---\n\n"
        f"# {name}\n\n{gen.paragraph(4)}\n"
    )
    path.write_text(text, encoding="utf-8")
    return len(text.split())


def write_song(gen: Gen, song_dir: Path, title: str, idx: int) -> int:
    song_dir.mkdir(parents=True, exist_ok=True)
    song_yaml = (
        "---\n"
        "type: song\n"
        f"id: song_{slug(title)}\n"
        "language: en\n"
        f"title: {title}\n"
        'artist: ""\n'
        "status: draft\n"
        "genre: [fixture]\n"
        "mood: [neutral]\n"
        "tempo: 92\n"
        "key: D minor\n"
        "time_signature: 4/4\n"
        "themes: [fixture]\n"
        f"plan: [verse, chorus, verse, chorus, bridge, outro]\n"
        "links:\n"
        "  chapters: []\n"
        "  characters: []\n"
    )
    (song_dir / "song.yaml").write_text(song_yaml, encoding="utf-8")

    used = 0
    parts = [f"# {title}\n"]
    for s in ["Verse 1", "Chorus", "Verse 2", "Bridge", "Outro"]:
        paras = gen.paragraphs_for(60)
        block = f"## {s}\n\n" + "\n\n".join(paras) + "\n"
        parts.append(block)
        used += len(block.split())
    (song_dir / "lyrics.md").write_text("\n".join(parts), encoding="utf-8")
    return used + len(song_yaml.split())


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--words", type=int, default=500_000)
    ap.add_argument("--chapters", type=int, default=100)
    ap.add_argument("--characters", type=int, default=24)
    ap.add_argument("--songs", type=int, default=12)
    ap.add_argument("--seed", type=int, default=42)
    ap.add_argument("--out", type=Path, default=Path("target/fixtures/oca-500k"))
    args = ap.parse_args()

    started = time.time()
    rng = random.Random(args.seed)
    gen = Gen(rng)
    out: Path = args.out
    out.mkdir(parents=True, exist_ok=True)

    # project.yaml (A2: project_id, format_version, language, kind)
    (out / "project.yaml").write_text(
        "project_id: fixture-oca-500k\n"
        "format_version: 1\n"
        "language: en\n"
        "kind: mixed\n"
        "title: Perf Fixture\n",
        encoding="utf-8",
    )

    total = 0
    per_chapter = max(1, args.words // args.chapters)
    for i in range(1, args.chapters + 1):
        title = CHAPTER_TITLES[i % len(CHAPTER_TITLES)] + f" {i}"
        act = (i - 1) // 25 + 1
        path = out / "manuscript" / f"{act:02d}-act-{act}" / f"{i:03d}-{slug(title)}.md"
        total += write_chapter(
            gen, path, f"ch_{i:03d}", title, i, per_chapter,
            pov="elena" if i % 2 else "marcus",
        )

    names = ["Elena", "Marcus", "Iris", "Dane"] + [
        f"Character{n}" for n in range(5, args.characters + 1)
    ]
    for idx, name in enumerate(names[: args.characters]):
        total += write_character(gen, out / "characters" / f"{slug(name)}.md", name, idx)

    (out / "world").mkdir(exist_ok=True)
    (out / "world" / "rules.md").write_text(
        frontmatter([("id", "world_rules")]) + "# World Rules\n\n" + gen.paragraph(6) + "\n",
        encoding="utf-8",
    )
    total += 10

    for i in range(1, args.songs + 1):
        title = CHAPTER_TITLES[i % len(CHAPTER_TITLES)] + f" (Song {i})"
        total += write_song(gen, out / "music" / "songs" / f"{slug(title)}", title, i)

    (out / "music" / "artist.yaml").write_text(
        "name: Fixture Artist\ngenres: [fixture]\n", encoding="utf-8"
    )
    (out / "outline").mkdir(exist_ok=True)
    (out / "outline" / "structure.md").write_text(
        frontmatter([("id", "outline_structure")])
        + "# Structure\n\n" + gen.paragraph(4) + "\n",
        encoding="utf-8",
    )
    (out / "notes").mkdir(exist_ok=True)

    files = [p for p in out.rglob("*") if p.is_file()]
    manifest = {
        "seed": args.seed,
        "target_words": args.words,
        "actual_words": total,
        "chapters": args.chapters,
        "characters": args.characters,
        "songs": args.songs,
        "files": len(files),
        "generated_unix": int(started),
        "seconds": round(time.time() - started, 2),
    }
    (out / "fixture-manifest.json").write_text(
        json.dumps(manifest, indent=2) + "\n", encoding="utf-8"
    )
    print(json.dumps(manifest, indent=2))


if __name__ == "__main__":
    main()
