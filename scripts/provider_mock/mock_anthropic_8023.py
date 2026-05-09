#!/usr/bin/env python3
"""Anthropic-native mock upstream on port 8023."""

from __future__ import annotations

import sys

from mock_native_anthropic_base import ProviderMockConfig, parse_args, run_server


CONFIG = ProviderMockConfig(
    provider_slug="anthropic",
    display_name="Anthropic",
    default_port=8023,
    models=(
        "claude-3-5-sonnet-20241022",
        "claude-3-7-sonnet-20250219",
    ),
    assistant_text="Anthropic mock response.",
)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv or sys.argv[1:])
    port = args.port or CONFIG.default_port
    return run_server(CONFIG, args.host, port)


if __name__ == "__main__":
    raise SystemExit(main())
