#!/usr/bin/env python3
"""Gemini-native mock upstream on port 8022."""

from __future__ import annotations

import sys

from mock_native_gemini_base import ProviderMockConfig, parse_args, run_server


CONFIG = ProviderMockConfig(
    provider_slug="gemini",
    display_name="Gemini",
    default_port=8022,
    models=(
        "gemini-2.5-flash",
        "gemini-2.5-pro",
    ),
    assistant_text="Gemini mock response.",
)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv or sys.argv[1:])
    port = args.port or CONFIG.default_port
    return run_server(CONFIG, args.host, port)


if __name__ == "__main__":
    raise SystemExit(main())
