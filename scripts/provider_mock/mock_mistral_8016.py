#!/usr/bin/env python3
"""Launcher for the Mistral mock provider."""

from __future__ import annotations

from mock_openai_compatible_base import ProviderMockConfig, run_server

config = ProviderMockConfig(
    provider_slug="mistral",
    display_name="Mistral",
    owned_by="mistral",
    default_port=8016,
    models=(
        "mistral/mistral-7b-instruct-v0:2",
        "mistral/mistral-large-2411",
    ),
    assistant_text="Mistral mock response.",
)


if __name__ == "__main__":
    raise SystemExit(run_server(config))
