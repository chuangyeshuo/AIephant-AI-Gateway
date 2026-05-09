#!/usr/bin/env python3
"""Launcher for the Qwen mock provider."""

from __future__ import annotations

from mock_openai_compatible_base import ProviderMockConfig, run_server

config = ProviderMockConfig(
    provider_slug="qwen",
    display_name="Qwen",
    owned_by="dashscope",
    default_port=8013,
    models=(
        "dashscope/qwen-plus",
        "dashscope/qwen-turbo",
    ),
    assistant_text="Qwen mock response.",
)


if __name__ == "__main__":
    raise SystemExit(run_server(config))
