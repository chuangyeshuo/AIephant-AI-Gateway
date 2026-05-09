#!/usr/bin/env python3
"""Split nextest stdout: human lines -> stdout, libtest JSON lines -> jsonl.

Expects ``NEXTEST_EXPERIMENTAL_LIBTEST_JSON=1`` and ``--message-format libtest-json-plus``.
Each JSON line is annotated with ``gate_segment`` from ``--segment``.
"""
from __future__ import annotations

import argparse
import json
import sys


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--jsonl", required=True, help="Append libtest JSON lines here")
    p.add_argument("--segment", required=True, help="Gate segment label (e.g. ai-gateway lib external)")
    args = p.parse_args()

    for raw in sys.stdin:
        line = raw.rstrip("\n")
        s = line.strip()
        if s.startswith("{"):
            try:
                o = json.loads(s)
            except json.JSONDecodeError:
                sys.stdout.write(raw)
                continue
            if o.get("type") in ("suite", "test"):
                o["gate_segment"] = args.segment
                with open(args.jsonl, "a", encoding="utf-8") as f:
                    f.write(json.dumps(o, ensure_ascii=False) + "\n")
                continue
        sys.stdout.write(raw)


if __name__ == "__main__":
    main()
