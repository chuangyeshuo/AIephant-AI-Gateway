#!/usr/bin/env python3
"""Launcher for the xAI mock provider."""

from __future__ import annotations

from mock_openai_compatible_base import ProviderMockConfig, run_server

config = ProviderMockConfig(
    provider_slug="xai",
    display_name="xAI",
    owned_by="xai",
    default_port=8017,
    models=(
        "xai/grok-3",
        "xai/grok-3-mini",
    ),
    assistant_text="xAI mock response.",
    chat_usage_overrides={"num_sources_used": 2},
    responses_usage_overrides={"cost": 0.00031},
    emit_empty_choices_usage_chunk_in_chat_stream=True,
)


if __name__ == "__main__":
    raise SystemExit(run_server(config))
