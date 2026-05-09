#!/usr/bin/env python3
"""Shared OpenAI-compatible mock upstream for provider adaptation testing."""

from __future__ import annotations

import argparse
import json
import sys
import time
from dataclasses import dataclass, field
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any, Iterable
from urllib.parse import urlparse


@dataclass(frozen=True)
class ProviderMockConfig:
    provider_slug: str
    display_name: str
    owned_by: str
    default_port: int
    models: tuple[str, ...]
    assistant_text: str
    chat_usage_overrides: dict[str, Any] = field(default_factory=dict)
    responses_usage_overrides: dict[str, Any] = field(default_factory=dict)
    emit_empty_choices_usage_chunk_in_chat_stream: bool = False


def normalize_path(path: str) -> str:
    parsed = urlparse(path)
    normalized = parsed.path
    while normalized.startswith("/v1/v1/"):
        normalized = normalized[3:]
    return normalized


def strip_provider_prefix(model: str | None) -> str:
    if not model:
        return ""
    if "/" in model:
        return model.split("/", 1)[1]
    return model


def pick_response_model(raw_model: str | None, config: ProviderMockConfig) -> str:
    normalized = strip_provider_prefix(raw_model)
    if normalized:
        return normalized
    return config.models[0]


def build_models_payload(config: ProviderMockConfig) -> dict[str, Any]:
    created = int(time.time())
    return {
        "object": "list",
        "data": [
            {
                "id": model,
                "object": "model",
                "created": created,
                "owned_by": config.owned_by,
            }
            for model in config.models
        ],
    }


def build_chat_payload(
    config: ProviderMockConfig,
    raw_model: str | None,
) -> dict[str, Any]:
    created = int(time.time())
    model = pick_response_model(raw_model, config)
    usage = {
        "prompt_tokens": 12,
        "completion_tokens": 6,
        "total_tokens": 18,
    }
    usage.update(config.chat_usage_overrides)
    return {
        "id": f"chatcmpl-{config.provider_slug}-mock",
        "object": "chat.completion",
        "created": created,
        "model": model,
        "choices": [
            {
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": config.assistant_text,
                },
                "finish_reason": "stop",
            }
        ],
        "usage": usage,
        "service_tier": "auto",
    }


def build_chat_stream_chunks(
    config: ProviderMockConfig,
    raw_model: str | None,
    include_usage_chunk: bool = False,
) -> list[str]:
    created = int(time.time())
    model = pick_response_model(raw_model, config)
    first = {
        "id": f"chatcmpl-{config.provider_slug}-stream",
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [
            {
                "index": 0,
                "delta": {"role": "assistant", "content": config.assistant_text},
                "finish_reason": None,
            }
        ],
    }
    second = {
        "id": f"chatcmpl-{config.provider_slug}-stream",
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [
            {
                "index": 0,
                "delta": {},
                "finish_reason": "stop",
            }
        ],
    }
    chunks = [
        f"data: {json.dumps(first, separators=(',', ':'))}\n\n",
        f"data: {json.dumps(second, separators=(',', ':'))}\n\n",
    ]
    if include_usage_chunk and config.emit_empty_choices_usage_chunk_in_chat_stream:
        usage = {
            "prompt_tokens": 12,
            "completion_tokens": 6,
            "total_tokens": 18,
        }
        usage.update(config.chat_usage_overrides)
        usage_chunk = {
            "id": f"chatcmpl-{config.provider_slug}-stream",
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [],
            "usage": usage,
        }
        chunks.append(f"data: {json.dumps(usage_chunk, separators=(',', ':'))}\n\n")
    chunks.append("data: [DONE]\n\n")
    return chunks


def build_responses_payload(
    config: ProviderMockConfig,
    raw_model: str | None,
) -> dict[str, Any]:
    created = int(time.time())
    model = pick_response_model(raw_model, config)
    usage = {
        "input_tokens": 12,
        "output_tokens": 6,
        "total_tokens": 18,
    }
    usage.update(config.responses_usage_overrides)
    return {
        "id": f"resp-{config.provider_slug}-mock",
        "object": "response",
        "created_at": created,
        "model": model,
        "status": "completed",
        "output": [
            {
                "id": f"msg-{config.provider_slug}-mock",
                "type": "message",
                "status": "completed",
                "role": "assistant",
                "content": [
                    {
                        "type": "output_text",
                        "text": config.assistant_text,
                        "annotations": [],
                    }
                ],
            }
        ],
        "usage": usage,
    }


def build_responses_created_event_response(
    response_id: str,
    model: str,
    created: int,
) -> dict[str, Any]:
    return {
        "id": response_id,
        "object": "response",
        "created_at": created,
        "model": model,
        "status": "in_progress",
        "output": [],
    }


def build_responses_completed_event_response(
    config: ProviderMockConfig,
    response_id: str,
    message_id: str,
    model: str,
    created: int,
) -> dict[str, Any]:
    usage = {
        "input_tokens": 12,
        "output_tokens": 6,
        "total_tokens": 18,
    }
    usage.update(config.responses_usage_overrides)
    return {
        "id": response_id,
        "object": "response",
        "created_at": created,
        "model": model,
        "status": "completed",
        "output": [
            {
                "id": message_id,
                "type": "message",
                "status": "completed",
                "role": "assistant",
                "content": [
                    {
                        "type": "output_text",
                        "text": config.assistant_text,
                        "annotations": [],
                    }
                ],
            }
        ],
        "usage": usage,
    }


def build_responses_stream_chunks(
    config: ProviderMockConfig,
    raw_model: str | None,
) -> list[str]:
    created = int(time.time())
    model = pick_response_model(raw_model, config)
    message_id = f"msg-{config.provider_slug}-stream"
    response_id = f"resp-{config.provider_slug}-stream"
    events = [
        {
            "type": "response.created",
            "response": build_responses_created_event_response(
                response_id=response_id,
                model=model,
                created=created,
            ),
        },
        {
            "type": "response.output_item.added",
            "output_index": 0,
            "item": {
                "id": message_id,
                "type": "message",
                "status": "in_progress",
                "role": "assistant",
                "content": [],
            },
        },
        {
            "type": "response.content_part.added",
            "item_id": message_id,
            "output_index": 0,
            "content_index": 0,
            "part": {
                "type": "output_text",
                "text": "",
                "annotations": [],
            },
        },
        {
            "type": "response.output_text.delta",
            "item_id": message_id,
            "output_index": 0,
            "content_index": 0,
            "delta": config.assistant_text,
        },
        {
            "type": "response.output_text.done",
            "item_id": message_id,
            "output_index": 0,
            "content_index": 0,
            "text": config.assistant_text,
        },
        {
            "type": "response.content_part.done",
            "item_id": message_id,
            "output_index": 0,
            "content_index": 0,
            "part": {
                "type": "output_text",
                "text": config.assistant_text,
                "annotations": [],
            },
        },
        {
            "type": "response.output_item.done",
            "output_index": 0,
            "item": {
                "id": message_id,
                "type": "message",
                "status": "completed",
                "role": "assistant",
                "content": [
                    {
                        "type": "output_text",
                        "text": config.assistant_text,
                        "annotations": [],
                    }
                ],
            },
        },
        {
            "type": "response.completed",
            "response": build_responses_completed_event_response(
                config=config,
                response_id=response_id,
                message_id=message_id,
                model=model,
                created=created,
            ),
        },
    ]
    return [
        f"data: {json.dumps(event, separators=(',', ':'))}\n\n" for event in events
    ] + ["data: [DONE]\n\n"]


def _json_dumps(body: dict[str, Any]) -> bytes:
    return json.dumps(body, ensure_ascii=True).encode("utf-8")


def make_handler(config: ProviderMockConfig):
    class OpenAICompatibleMockHandler(BaseHTTPRequestHandler):
        server_version = f"ProviderMock/{config.provider_slug}"

        def log_message(self, fmt: str, *args: Any) -> None:
            sys.stderr.write(
                f"[mock-{config.provider_slug}] {self.client_address[0]} "
                f"{fmt % args}\n"
            )

        def do_GET(self) -> None:  # noqa: N802
            path = normalize_path(self.path)
            self.log_message("GET %s", path)
            if path in {"/models", "/v1/models"}:
                self._send_json(HTTPStatus.OK, build_models_payload(config))
                return
            if path in {"/", ""}:
                self._send_json(
                    HTTPStatus.OK,
                    {
                        "provider": config.provider_slug,
                        "display_name": config.display_name,
                        "supported_paths": [
                            "/models",
                            "/v1/models",
                            "/chat/completions",
                            "/v1/chat/completions",
                            "/responses",
                            "/v1/responses",
                        ],
                    },
                )
                return
            self._send_not_found(path)

        def do_POST(self) -> None:  # noqa: N802
            path = normalize_path(self.path)
            payload = self._read_json_body()
            model = payload.get("model")
            stream = bool(payload.get("stream"))
            stream_options = payload.get("stream_options")
            include_usage_chunk = bool(
                isinstance(stream_options, dict) and stream_options.get("include_usage")
            )
            self.log_message("POST %s model=%s stream=%s", path, model, stream)

            if path in {"/chat/completions", "/v1/chat/completions"}:
                if stream:
                    self._send_stream(
                        build_chat_stream_chunks(
                            config,
                            model,
                            include_usage_chunk=include_usage_chunk,
                        )
                    )
                else:
                    self._send_json(HTTPStatus.OK, build_chat_payload(config, model))
                return

            if path in {"/responses", "/v1/responses"}:
                if stream:
                    self._send_stream(build_responses_stream_chunks(config, model))
                else:
                    self._send_json(
                        HTTPStatus.OK, build_responses_payload(config, model)
                    )
                return

            self._send_not_found(path)

        def _read_json_body(self) -> dict[str, Any]:
            content_length = self.headers.get("Content-Length", "0")
            try:
                length = int(content_length)
            except ValueError:
                length = 0
            raw = self.rfile.read(length) if length > 0 else b"{}"
            if not raw:
                return {}
            try:
                body = json.loads(raw.decode("utf-8"))
            except json.JSONDecodeError:
                return {}
            return body if isinstance(body, dict) else {}

        def _send_json(self, status: HTTPStatus, body: dict[str, Any]) -> None:
            payload = _json_dumps(body)
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

        def _send_stream(self, chunks: Iterable[str]) -> None:
            self.send_response(HTTPStatus.OK)
            self.send_header("Content-Type", "text/event-stream; charset=utf-8")
            self.send_header("Cache-Control", "no-cache")
            self.send_header("Connection", "close")
            self.end_headers()
            for chunk in chunks:
                self.wfile.write(chunk.encode("utf-8"))
                self.wfile.flush()

        def _send_not_found(self, path: str) -> None:
            self._send_json(
                HTTPStatus.NOT_FOUND,
                {
                    "error": {
                        "message": f"Not found: {path}",
                        "provider": config.provider_slug,
                    }
                },
            )

    return OpenAICompatibleMockHandler


def create_server(
    config: ProviderMockConfig,
    host: str,
    port: int,
) -> ThreadingHTTPServer:
    return ThreadingHTTPServer((host, port), make_handler(config))


def parse_args(config: ProviderMockConfig) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=f"Run a provider mock upstream for {config.display_name}."
    )
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=config.default_port)
    return parser.parse_args()


def run_server(config: ProviderMockConfig) -> int:
    args = parse_args(config)
    server = create_server(config=config, host=args.host, port=args.port)
    host, port = server.server_address
    print(
        f"mock-{config.provider_slug} listening on http://{host}:{port}",
        flush=True,
    )
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print(f"\nshutting down mock-{config.provider_slug}", flush=True)
    finally:
        server.server_close()
    return 0
