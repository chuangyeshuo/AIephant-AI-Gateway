#!/usr/bin/env python3
"""Launcher for the OpenRouter mock provider."""

from __future__ import annotations

from mock_openai_compatible_base import ProviderMockConfig, run_server

config = ProviderMockConfig(
    provider_slug="openrouter",
    display_name="OpenRouter",
    owned_by="openrouter",
    default_port=8018,
    models=(
        "openrouter/anthropic/claude-3.5-sonnet",
        "openrouter/openai/gpt-4o-mini",
    ),
    assistant_text="OpenRouter mock response.",
    chat_usage_overrides={"cost": 0.00042},
    responses_usage_overrides={"cost": 0.00042},
)


if __name__ == "__main__":
    raise SystemExit(run_server(config))
