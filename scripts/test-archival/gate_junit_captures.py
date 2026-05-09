#!/usr/bin/env python3
"""Parse nextest JUnit (ci-gate) testcase system-out/err for gate archival.

Subcommands:
  append-log <junit.xml> <segment-label>
      Human-readable excerpt to stdout (for gate.log via tee).
  write-json --out <out.json> [--libtest-events <jsonl>] <junit.xml> <segment-label> ...
      One JSON file with per-segment testcase captures; optional libtest JSONL merge.

Env:
  GATE_JUNIT_LOG_ALL=1 — include every testcase (including libtest-only stdout);
      default: omit cases whose stdout is only libtest template lines and stderr empty,
      except failures always included.
"""
from __future__ import annotations

import argparse
import json
import os
import sys
import xml.etree.ElementTree as ET
from typing import Any

MAX_FIELD_CHARS = 65_536
JSON_KIND = "gate-lib-test-captures"
JSON_VERSION = 1


def _trim_field(text: str | None) -> str:
    if not text:
        return ""
    s = text.strip()
    if len(s) <= MAX_FIELD_CHARS:
        return s
    return (
        s[:MAX_FIELD_CHARS]
        + f"\n...[truncated, original length {len(s)} chars]\n"
    )


def _is_harness_only_line(line: str) -> bool:
    s = line.strip()
    if not s:
        return True
    if s.startswith("running ") and " test" in s:
        return True
    if s.startswith("test result:"):
        return True
    if s.startswith("test ") and " ... " in s:
        return True
    return False


def _stdout_is_only_harness(stdout: str) -> bool:
    return all(_is_harness_only_line(line) for line in stdout.splitlines())


def _suites(root: ET.Element) -> list[ET.Element]:
    if root.tag == "testsuites":
        return root.findall("testsuite")
    if root.tag == "testsuite":
        return [root]
    return []


def _failure_records(case: ET.Element) -> list[dict[str, str]]:
    out: list[dict[str, str]] = []
    for fn in case.findall("failure") + case.findall("error"):
        out.append(
            {
                "kind": fn.tag,
                "message": (fn.attrib.get("message") or "").strip(),
                "body": _trim_field((fn.text or "").strip() or ""),
            }
        )
    return out


def _case_should_include(
    failures: list[dict[str, str]],
    out: str,
    err: str,
    log_all: bool,
) -> bool:
    if log_all:
        return True
    if failures:
        return True
    if err.strip():
        return True
    if not _stdout_is_only_harness(out):
        return True
    return False


def iter_cases(
    path: str,
    segment_label: str,
    *,
    log_all: bool,
) -> list[dict[str, Any]]:
    tree = ET.parse(path)
    root = tree.getroot()
    cases_out: list[dict[str, Any]] = []
    for suite in _suites(root):
        for case in suite.findall("testcase"):
            out_el, err_el = case.find("system-out"), case.find("system-err")
            raw_out = out_el.text if out_el is not None and out_el.text else ""
            raw_err = err_el.text if err_el is not None and err_el.text else ""
            out = raw_out.strip()
            err = raw_err.strip()
            failures = _failure_records(case)
            if not _case_should_include(failures, out, err, log_all):
                continue
            time_s = case.attrib.get("time")
            try:
                time_sec = float(time_s) if time_s is not None else None
            except ValueError:
                time_sec = None
            cases_out.append(
                {
                    "segment": segment_label,
                    "classname": case.attrib.get("classname"),
                    "name": case.attrib.get("name"),
                    "time_sec": time_sec,
                    "input": {
                        "captured_stdout": _trim_field(raw_out),
                        "captured_stderr": _trim_field(raw_err),
                    },
                    "result": {
                        "failures": failures,
                    },
                }
            )
    return cases_out


def cmd_append_log(path: str, label: str) -> None:
    log_all = os.environ.get("GATE_JUNIT_LOG_ALL") == "1"
    blocks: list[str] = []
    tree = ET.parse(path)
    root = tree.getroot()
    for suite in _suites(root):
        for case in suite.findall("testcase"):
            out_el, err_el = case.find("system-out"), case.find("system-err")
            raw_out = out_el.text if out_el is not None and out_el.text else ""
            raw_err = err_el.text if err_el is not None and err_el.text else ""
            out = raw_out.strip()
            err = raw_err.strip()
            failures = _failure_records(case)
            if not _case_should_include(failures, out, err, log_all):
                continue
            name = case.attrib.get("name", "?")
            classname = case.attrib.get("classname", "?")
            lines = [
                "",
                f"[gate] testcase classname={classname!r} name={name!r}",
            ]
            for i, fr in enumerate(failures):
                title = fr["message"] or fr["kind"]
                lines.append(f"  --- failure[{i}] ({title}) ---")
                lines.append(fr["body"] or "(empty)")
            lines.append("  --- system-out ---")
            lines.append(_trim_field(raw_out) or "(empty)")
            lines.append("  --- system-err ---")
            lines.append(_trim_field(raw_err) or "(empty)")
            blocks.append("\n".join(lines))

    if not blocks:
        return

    sep = "=" * 60
    print(f"\n{sep}")
    print(f"[gate] JUnit testcase output excerpt segment={label!r} file={path!r}")
    print(sep)
    print("\n".join(blocks))
    print(sep)


def _load_libtest_index(path: str) -> dict[tuple[str, str], dict[str, Any]]:
    idx: dict[tuple[str, str], dict[str, Any]] = {}
    try:
        with open(path, encoding="utf-8") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                o = json.loads(line)
                if o.get("type") != "test":
                    continue
                seg = o.get("gate_segment")
                nm = o.get("name")
                if not isinstance(seg, str) or not isinstance(nm, str):
                    continue
                tail = nm.split("$", 1)[1] if "$" in nm else nm
                idx[(seg, tail)] = o
    except OSError:
        pass
    return idx


def cmd_write_json(
    out_path: str,
    pairs: list[tuple[str, str]],
    libtest_events: str,
) -> None:
    log_all = os.environ.get("GATE_JUNIT_LOG_ALL") == "1"
    lib_idx = (
        _load_libtest_index(libtest_events) if libtest_events.strip() else {}
    )
    segments: list[dict[str, Any]] = []
    for junit_path, seg_label in pairs:
        if not os.path.isfile(junit_path):
            continue
        cases = iter_cases(junit_path, seg_label, log_all=log_all)
        for c in cases:
            ev = lib_idx.get((seg_label, c.get("name")))
            if ev:
                c["libtest"] = {
                    k: ev[k]
                    for k in ("event", "exec_time", "name", "message", "stdout")
                    if k in ev
                }
        segments.append(
            {
                "label": seg_label,
                "junit_path": junit_path,
                "case_count": len(cases),
                "cases": cases,
            }
        )
    doc: dict[str, Any] = {
        "kind": JSON_KIND,
        "version": JSON_VERSION,
        "log_all": log_all,
        "about_semantic_io": (
            "Rust #[test] does not write function arguments or return values to stdout by default; "
            "JUnit system-out only contains what the test prints plus libtest boilerplate lines. "
            "For structured inputs/outputs use the gate_test_io! macro inside tests "
            "(see ai-gateway/src/gate_archival.rs) or println JSON yourself."
        ),
        "segments": segments,
    }
    if libtest_events.strip():
        doc["libtest_events_source"] = libtest_events
    with open(out_path, "w", encoding="utf-8") as f:
        json.dump(doc, f, ensure_ascii=False, indent=2)
        f.write("\n")


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__)
    sub = p.add_subparsers(dest="cmd", required=True)

    p_log = sub.add_parser("append-log", help="Print excerpt for gate.log")
    p_log.add_argument("junit_xml")
    p_log.add_argument("segment_label")

    p_json = sub.add_parser("write-json", help="Write captures JSON")
    p_json.add_argument("--out", required=True, help="Output JSON path")
    p_json.add_argument(
        "--libtest-events",
        default="",
        help="Optional nextest libtest-json-plus jsonl (from gate_nextest_human_json_mux.py)",
    )
    p_json.add_argument(
        "segment_pairs",
        nargs="+",
        metavar="PAIR",
        help="junit.xml path followed by segment label (repeat)",
    )

    args = p.parse_args()
    if args.cmd == "append-log":
        cmd_append_log(args.junit_xml, args.segment_label)
        return

    if args.cmd == "write-json":
        pairs_list = args.segment_pairs
        if len(pairs_list) % 2 != 0:
            p.error("write-json requires an even number of PAIR args: path label ...")
        pairs = [
            (pairs_list[i], pairs_list[i + 1])
            for i in range(0, len(pairs_list), 2)
        ]
        cmd_write_json(args.out, pairs, args.libtest_events)
        return

    raise AssertionError("unhandled cmd")


if __name__ == "__main__":
    main()
