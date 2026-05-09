#!/usr/bin/env python3
"""Inject <properties> into JUnit <testcase> from nextest libtest-json-plus stream.

Adds machine-readable fields (exec_time, full libtest name) and documents when
stdout is only Rust libtest boilerplate (no user-printed inputs/outputs).
"""
from __future__ import annotations

import argparse
import json
import xml.etree.ElementTree as ET


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


def _libtest_tail(full_name: str) -> str:
    if "$" in full_name:
        return full_name.split("$", 1)[1]
    return full_name


def _load_libtest_map(jsonl_path: str, segment: str) -> dict[str, dict]:
    """Map junit ``name`` attribute -> last test event dict for this segment."""
    m: dict[str, dict] = {}
    try:
        with open(jsonl_path, encoding="utf-8") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                o = json.loads(line)
                if o.get("type") != "test":
                    continue
                if o.get("gate_segment") != segment:
                    continue
                nm = o.get("name")
                if not isinstance(nm, str):
                    continue
                m[_libtest_tail(nm)] = o
    except OSError:
        pass
    return m


def _strip_gate_properties(case: ET.Element) -> None:
    for props in list(case.findall("properties")):
        for pr in list(props.findall("property")):
            n = pr.attrib.get("name", "")
            if n.startswith("gate."):
                props.remove(pr)
        if len(props.findall("property")) == 0:
            case.remove(props)


def _ensure_properties(case: ET.Element) -> ET.Element:
    for props in case.findall("properties"):
        return props
    props = ET.Element("properties")
    case.insert(0, props)
    return props


def cmd_enrich(jsonl: str, junit_path: str, segment: str) -> None:
    lib = _load_libtest_map(jsonl, segment)
    tree = ET.parse(junit_path)
    root = tree.getroot()
    suites = root.findall("testsuite") if root.tag == "testsuites" else [root]

    for suite in suites:
        for case in suite.findall("testcase"):
            name = case.attrib.get("name") or ""
            out_el = case.find("system-out")
            raw_out = out_el.text if out_el is not None and out_el.text else ""
            harness_only = _stdout_is_only_harness(raw_out.strip())

            _strip_gate_properties(case)
            props = _ensure_properties(case)

            def add_prop(k: str, v: str) -> None:
                ET.SubElement(props, "property", name=k, value=v)

            add_prop(
                "gate.rust_stdout_semantic",
                "libtest_template_only"
                if harness_only
                else "includes_user_or_non_libtest_lines",
            )
            add_prop(
                "gate.semantic_io_hint",
                "Rust #[test] does not log function arguments or return values; use println!/tracing "
                "or the gate_test_io! macro from ai-gateway to emit JSON so it appears in system-out.",
            )

            ev = lib.get(name)
            if ev:
                add_prop("gate.libtest_matched", "true")
                add_prop("gate.libtest_full_name", ev.get("name", ""))
                et = ev.get("exec_time")
                if et is not None:
                    add_prop("gate.libtest_exec_time_sec", str(et))
                ev_name = ev.get("event")
                if ev_name:
                    add_prop("gate.libtest_event", str(ev_name))
                if "message" in ev and ev["message"]:
                    add_prop("gate.libtest_message", str(ev["message"]))
            else:
                add_prop("gate.libtest_matched", "false")

    ET.indent(tree, space="    ")
    tree.write(junit_path, encoding="utf-8", xml_declaration=True)


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("libtest_jsonl")
    p.add_argument("junit_xml")
    p.add_argument("segment_label")
    args = p.parse_args()
    cmd_enrich(args.libtest_jsonl, args.junit_xml, args.segment_label)


if __name__ == "__main__":
    main()
