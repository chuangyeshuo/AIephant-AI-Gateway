#!/usr/bin/env python3
"""Launcher for the OpenAI mock provider."""

from __future__ import annotations

from mock_openai_compatible_base import ProviderMockConfig, run_server

config = ProviderMockConfig(
    provider_slug="openai",
    display_name="OpenAI",
    owned_by="openai",
    default_port=8011,
    models=(
        "gpt-4o-mini",
        "gpt-4.1",
    ),
    assistant_text="OpenAI mock response.",
)


if __name__ == "__main__":
    raise SystemExit(run_server(config))
