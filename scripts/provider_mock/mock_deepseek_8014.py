#!/usr/bin/env python3
"""Launcher for the DeepSeek mock provider."""

from __future__ import annotations

from mock_openai_compatible_base import ProviderMockConfig, run_server

config = ProviderMockConfig(
    provider_slug="deepseek",
    display_name="DeepSeek",
    owned_by="deepseek",
    default_port=8014,
    models=(
        "deepseek/deepseek-chat",
        "deepseek/deepseek-reasoner",
    ),
    assistant_text="DeepSeek mock response.",
)


if __name__ == "__main__":
    raise SystemExit(run_server(config))
