#!/usr/bin/env python3
"""Launcher for the Kimi mock provider."""

from __future__ import annotations

from mock_openai_compatible_base import ProviderMockConfig, run_server

config = ProviderMockConfig(
    provider_slug="kimi",
    display_name="Kimi",
    owned_by="moonshot",
    default_port=8012,
    models=(
        "moonshot/kimi-k2.5",
        "moonshot/kimi-latest",
    ),
    assistant_text="Kimi mock response.",
)


if __name__ == "__main__":
    raise SystemExit(run_server(config))
