"""Generate a large test vault for performance work.

Usage: python scripts/gen_vault.py <vault-dir> [count]
"""

import os
import random
import sys
import uuid
from datetime import datetime, timedelta, timezone

random.seed(42)

WORDS = (
    "idea card link note system memory thought writing reading book chapter "
    "argument evidence claim source context question answer pattern habit focus "
    "project rust python code design module function test review learning time "
    "process structure network concept principle example summary insight theory"
).split()
TAGS = [f"tag{i}" for i in range(30)] + [
    "rust", "python", "cpp", "books", "ideas", "work", "home", "research", "ml", "web"
]
COLORS = ["slate", "yellow", "orange", "rose", "violet", "blue", "emerald"]

RUST = """fn {name}(items: &[i32]) -> i32 {{
    let mut total = 0;
    for x in items {{
        if *x > {n} {{
            total += x * 2;
        }}
    }}
    total
}}"""
PYTHON = """def {name}(items):
    \"\"\"Sum doubled values above {n}.\"\"\"
    total = 0
    for x in items:
        if x > {n}:
            total += x * 2
    return total"""
CPP = """#include <vector>

int {name}(const std::vector<int>& items) {{
    int total = 0;
    for (int x : items) {{
        if (x > {n}) total += x * 2;
    }}
    return total;
}}"""


def sentence(n):
    words = random.choices(WORDS, k=n)
    return " ".join(words).capitalize() + "."


def note_body():
    parts = []
    for _ in range(random.randint(1, 4)):
        kind = random.random()
        if kind < 0.5:
            parts.append(" ".join(sentence(random.randint(6, 14)) for _ in range(random.randint(1, 3))))
        elif kind < 0.75:
            parts.append("\n".join(f"- {sentence(random.randint(3, 7))}" for _ in range(random.randint(2, 5))))
        elif kind < 0.9:
            parts.append(f"## {sentence(3)[:-1]}")
        else:
            parts.append(f"> {sentence(8)}")
    text = "\n\n".join(parts)
    return text.replace(" idea ", " **idea** ", 1).replace(" code ", " `code` ", 1)


def yaml_str(s):
    return "'" + s.replace("'", "''") + "'"


def main():
    root = sys.argv[1]
    count = int(sys.argv[2]) if len(sys.argv) > 2 else 5000
    os.makedirs(root, exist_ok=True)
    start = datetime(2024, 10, 1, tzinfo=timezone.utc)
    for i in range(count):
        created = start + timedelta(seconds=random.randint(0, 730 * 86400))
        updated = created + timedelta(seconds=random.randint(0, 30 * 86400))
        r = random.random()
        kind = "note" if r < 0.5 else ("todo" if r < 0.75 else "snippet")
        tags = random.sample(TAGS, random.randint(0, 3))
        lines = [
            "---",
            f"id: {uuid.UUID(int=random.getrandbits(128))}",
            f"kind: {kind}",
            f"title: {yaml_str(sentence(random.randint(2, 6))[:-1])}",
        ]
        if tags:
            lines.append("tags:")
            lines += [f"- {t}" for t in tags]
        lines.append(f"color: {random.choice(COLORS)}")
        if random.random() < 0.002:
            lines.append("pinned: true")
        lines += [f"created: {created.isoformat().replace('+00:00', 'Z')}",
                  f"updated: {updated.isoformat().replace('+00:00', 'Z')}", "---", ""]
        if kind == "note":
            body = note_body()
        elif kind == "todo":
            body = "\n".join(
                f"- [{'x' if random.random() < 0.5 else ' '}] {sentence(random.randint(2, 6))}"
                for _ in range(random.randint(2, 8))
            )
        else:
            lang, tpl = random.choice([("rust", RUST), ("python", PYTHON), ("cpp", CPP)])
            code = tpl.format(name=f"compute_{i}", n=random.randint(0, 99))
            body = f"```{lang}\n{code}\n```"
        name = created.strftime("%Y%m%d%H%M%S")
        path = os.path.join(root, f"{name}.md")
        n = 1
        while os.path.exists(path):
            n += 1
            path = os.path.join(root, f"{name}-{n}.md")
        with open(path, "w", encoding="utf-8", newline="\n") as f:
            f.write("\n".join(lines) + "\n" + body + "\n")
    print(f"wrote {count} cards to {root}")


if __name__ == "__main__":
    main()
