#!/usr/bin/env python3
"""Generate a deterministic corpus for the opt-in tract inference benchmark."""
import argparse
from pathlib import Path

parser = argparse.ArgumentParser()
parser.add_argument("directory", type=Path)
parser.add_argument(
    "--kind", choices=["mixed", "short", "long", "repository"], default="mixed"
)
parser.add_argument("--count", type=int, default=40)
args = parser.parse_args()
args.directory.mkdir(parents=True, exist_ok=True)
vocabulary = (
    "coding retrieval memory architecture styleguide dependency patch service "
    "storage lifetime ownership compiler failure indexing concept semantic rust"
).split()
paragraphs = []
if args.kind == "repository":
    root = Path(__file__).resolve().parents[1]
    sources = [root / "README.md", root / "CHANGELOG.md"]
    sources.extend(sorted((root / "docs").rglob("*.md")))
    for source in sources:
        paragraphs.extend(
            p.strip()[:3000]
            for p in source.read_text().split("\n\n")
            if len(p.split()) >= 25
        )
    assert len(paragraphs) >= args.count, "not enough source paragraphs"
for i in range(args.count):
    if args.kind == "repository":
        body = paragraphs[i]
    else:
        n = {"mixed": [12, 48, 128, 230][i % 4], "short": 12, "long": 230}[
            args.kind
        ]
        words = [vocabulary[(i * 7 + j * 3) % len(vocabulary)] for j in range(n)]
        body = f"Document {i} " + " ".join(words) + "."
    (args.directory / f"example-{i:03}.md").write_text(
        "---\ntype: Note\n---\n" + body + "\n"
    )
print(f"Generated {args.count} {args.kind} documents in {args.directory}")
