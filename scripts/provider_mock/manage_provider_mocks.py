#!/usr/bin/env python3
"""Lifecycle manager for local provider mock services."""

from __future__ import annotations

import argparse
import os
import signal
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable


SCRIPT_DIR = Path(__file__).resolve().parent
DEFAULT_RUNTIME_DIR = SCRIPT_DIR / ".runtime"
DEFAULT_STOP_TIMEOUT_SECONDS = 5.0
ACTIVE_PROCESSES: dict[str, subprocess.Popen] = {}


@dataclass(frozen=True)
class MockSpec:
    name: str
    script_path: Path
    port: int


@dataclass(frozen=True)
class MockStatus:
    name: str
    port: int
    state: str
    pid: int | None
    script_path: Path


MOCK_SPECS: tuple[MockSpec, ...] = (
    MockSpec("openai", SCRIPT_DIR / "mock_openai_8011.py", 8011),
    MockSpec("kimi", SCRIPT_DIR / "mock_kimi_8012.py", 8012),
    MockSpec("qwen", SCRIPT_DIR / "mock_qwen_8013.py", 8013),
    MockSpec("deepseek", SCRIPT_DIR / "mock_deepseek_8014.py", 8014),
    MockSpec("groq", SCRIPT_DIR / "mock_groq_8015.py", 8015),
    MockSpec("mistral", SCRIPT_DIR / "mock_mistral_8016.py", 8016),
    MockSpec("xai", SCRIPT_DIR / "mock_xai_8017.py", 8017),
    MockSpec("openrouter", SCRIPT_DIR / "mock_openrouter_8018.py", 8018),
    MockSpec("anthropic", SCRIPT_DIR / "mock_anthropic_8023.py", 8023),
    MockSpec("gemini", SCRIPT_DIR / "mock_gemini_8022.py", 8022),
)


def ensure_runtime_dirs(runtime_dir: Path) -> None:
    (runtime_dir / "logs").mkdir(parents=True, exist_ok=True)
    (runtime_dir / "pids").mkdir(parents=True, exist_ok=True)


def get_pid_path(runtime_dir: Path, spec: MockSpec) -> Path:
    return runtime_dir / "pids" / f"{spec.name}.pid"


def get_log_path(runtime_dir: Path, spec: MockSpec) -> Path:
    return runtime_dir / "logs" / f"{spec.name}.log"


def read_pid(pid_path: Path) -> int | None:
    if not pid_path.exists():
        return None
    try:
        return int(pid_path.read_text(encoding="utf-8").strip())
    except (OSError, ValueError):
        return None


def is_process_alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    try:
        process_state = subprocess.check_output(
            ["ps", "-o", "stat=", "-p", str(pid)],
            text=True,
            stderr=subprocess.DEVNULL,
        ).strip()
    except subprocess.SubprocessError:
        return True
    if not process_state:
        return False
    if process_state.startswith("Z"):
        return False
    return True


def reap_child_process(pid: int) -> None:
    try:
        os.waitpid(pid, os.WNOHANG)
    except ChildProcessError:
        return


def get_mock_status(spec: MockSpec, runtime_dir: Path) -> MockStatus:
    ensure_runtime_dirs(runtime_dir)
    pid_path = get_pid_path(runtime_dir, spec)
    pid = read_pid(pid_path)
    if pid is None:
        if pid_path.exists():
            pid_path.unlink(missing_ok=True)
        return MockStatus(spec.name, spec.port, "stopped", None, spec.script_path)

    if is_process_alive(pid):
        return MockStatus(spec.name, spec.port, "running", pid, spec.script_path)

    pid_path.unlink(missing_ok=True)
    return MockStatus(spec.name, spec.port, "stopped", None, spec.script_path)


def start_mock(spec: MockSpec, runtime_dir: Path) -> str:
    ensure_runtime_dirs(runtime_dir)
    current_status = get_mock_status(spec=spec, runtime_dir=runtime_dir)
    if current_status.state == "running":
        return "already_running"

    log_path = get_log_path(runtime_dir, spec)
    pid_path = get_pid_path(runtime_dir, spec)
    with log_path.open("ab") as log_file:
        process = subprocess.Popen(
            [sys.executable, str(spec.script_path)],
            cwd=str(spec.script_path.parent),
            stdout=log_file,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
    ACTIVE_PROCESSES[spec.name] = process

    pid_path.write_text(f"{process.pid}\n", encoding="utf-8")

    stable_polls = 0
    for _ in range(20):
        if process.poll() is not None:
            ACTIVE_PROCESSES.pop(spec.name, None)
            pid_path.unlink(missing_ok=True)
            return "failed"
        if is_process_alive(process.pid):
            stable_polls += 1
            if stable_polls >= 3:
                return "started"
        else:
            stable_polls = 0
        time.sleep(0.1)

    ACTIVE_PROCESSES.pop(spec.name, None)
    pid_path.unlink(missing_ok=True)
    return "failed"


def stop_mock(
    spec: MockSpec,
    runtime_dir: Path,
    timeout_seconds: float = DEFAULT_STOP_TIMEOUT_SECONDS,
) -> str:
    ensure_runtime_dirs(runtime_dir)
    current_status = get_mock_status(spec=spec, runtime_dir=runtime_dir)
    pid_path = get_pid_path(runtime_dir, spec)

    if current_status.state != "running" or current_status.pid is None:
        pid_path.unlink(missing_ok=True)
        return "already_stopped"

    pid = current_status.pid
    try:
        os.kill(pid, signal.SIGTERM)
    except ProcessLookupError:
        pid_path.unlink(missing_ok=True)
        return "already_stopped"

    deadline = time.time() + timeout_seconds
    while time.time() < deadline:
        if not is_process_alive(pid):
            process = ACTIVE_PROCESSES.pop(spec.name, None)
            if process is not None:
                process.wait(timeout=0.1)
            reap_child_process(pid)
            pid_path.unlink(missing_ok=True)
            return "stopped"
        time.sleep(0.1)

    try:
        os.kill(pid, signal.SIGKILL)
    except ProcessLookupError:
        pass

    for _ in range(10):
        if not is_process_alive(pid):
            process = ACTIVE_PROCESSES.pop(spec.name, None)
            if process is not None:
                process.wait(timeout=0.1)
            reap_child_process(pid)
            pid_path.unlink(missing_ok=True)
            return "stopped"
        time.sleep(0.1)

    ACTIVE_PROCESSES.pop(spec.name, None)
    return "failed"


def find_specs(names: list[str] | None) -> list[MockSpec]:
    if not names:
        return list(MOCK_SPECS)

    wanted = {name.strip() for name in names if name.strip()}
    found = [spec for spec in MOCK_SPECS if spec.name in wanted]
    missing = sorted(wanted - {spec.name for spec in found})
    if missing:
        raise ValueError(f"Unknown mock names: {', '.join(missing)}")
    return found


def format_status_line(status: MockStatus) -> str:
    pid_text = str(status.pid) if status.pid is not None else "-"
    return f"{status.name:<12} {status.state:<8} pid={pid_text:<8} port={status.port}"


def start_specs(specs: Iterable[MockSpec], runtime_dir: Path) -> int:
    exit_code = 0
    for spec in specs:
        result = start_mock(spec=spec, runtime_dir=runtime_dir)
        print(f"{spec.name}: {result}")
        if result == "failed":
            exit_code = 1
    return exit_code


def stop_specs(specs: Iterable[MockSpec], runtime_dir: Path) -> int:
    exit_code = 0
    for spec in specs:
        result = stop_mock(spec=spec, runtime_dir=runtime_dir)
        print(f"{spec.name}: {result}")
        if result == "failed":
            exit_code = 1
    return exit_code


def status_specs(specs: Iterable[MockSpec], runtime_dir: Path) -> int:
    for spec in specs:
        print(format_status_line(get_mock_status(spec=spec, runtime_dir=runtime_dir)))
    return 0


def restart_specs(specs: Iterable[MockSpec], runtime_dir: Path) -> int:
    stop_exit = stop_specs(specs=specs, runtime_dir=runtime_dir)
    start_exit = start_specs(specs=specs, runtime_dir=runtime_dir)
    if stop_exit != 0 or start_exit != 0:
        return 1
    return 0


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "action",
        choices=("start", "stop", "restart", "status"),
        help="Lifecycle action to execute.",
    )
    parser.add_argument(
        "names",
        nargs="*",
        help="Optional mock names. Defaults to all mocks.",
    )
    parser.add_argument(
        "--runtime-dir",
        default=str(DEFAULT_RUNTIME_DIR),
        help="Directory for PID files and logs.",
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv or sys.argv[1:])
    runtime_dir = Path(args.runtime_dir).resolve()
    try:
        specs = find_specs(args.names)
    except ValueError as exc:
        print(str(exc), file=sys.stderr)
        return 2

    if args.action == "start":
        return start_specs(specs=specs, runtime_dir=runtime_dir)
    if args.action == "stop":
        return stop_specs(specs=specs, runtime_dir=runtime_dir)
    if args.action == "restart":
        return restart_specs(specs=specs, runtime_dir=runtime_dir)
    return status_specs(specs=specs, runtime_dir=runtime_dir)


if __name__ == "__main__":
    raise SystemExit(main())
