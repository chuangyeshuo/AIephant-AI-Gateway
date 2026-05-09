#!/usr/bin/env python3
"""Launcher for the Groq mock provider."""

from __future__ import annotations

from mock_openai_compatible_base import ProviderMockConfig, run_server

config = ProviderMockConfig(
    provider_slug="groq",
    display_name="Groq",
    owned_by="groq",
    default_port=8015,
    models=(
        "groq/meta-llama/llama-4-maverick-17b-128e-instruct",
        "groq/meta-llama/llama-4-scout-17b-16e-instruct",
    ),
    assistant_text="Groq mock response.",
)


if __name__ == "__main__":
    raise SystemExit(run_server(config))
