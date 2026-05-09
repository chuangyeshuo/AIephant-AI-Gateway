#!/usr/bin/env python3
"""Local OpenAI-compatible mock upstream for gateway debugging.

Default address:
  http://127.0.0.1:8011

Supported endpoints:
  - GET  /v1/models
  - GET  /v1/v1/models
  - POST /v1/chat/completions
  - POST /v1/v1/chat/completions

Features:
  - non-stream chat completions
  - stream chat completions via SSE
  - simple request logging for gateway debugging
"""

from __future__ import annotations

import argparse
import json
import sys
import time
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any
from urllib.parse import urlparse


SUPPORTED_MODELS = [
    "gpt-5.1",
    "gpt-5.1-codex",
    "gpt-5.1-codex-max",
    "gpt-5.1-codex-mini",
]


def normalize_path(path: str) -> str:
    parsed = urlparse(path)
    if parsed.path.startswith("/v1/v1/"):
        return parsed.path[3:]
    return parsed.path


def normalize_model(raw_model: str | None) -> str:
    if not raw_model:
        return SUPPORTED_MODELS[0]
    if "/" in raw_model:
        _, _, tail = raw_model.partition("/")
        return tail or SUPPORTED_MODELS[0]
    return raw_model


def models_payload() -> dict[str, Any]:
    now = int(time.time())
    return {
        "object": "list",
        "data": [
            {
                "id": model,
                "object": "model",
                "created": now,
                "owned_by": "mock-openai-8011",
            }
            for model in SUPPORTED_MODELS
        ],
    }


def chat_completion_payload(raw_model: str | None) -> dict[str, Any]:
    model = normalize_model(raw_model)
    created = int(time.time())
    return {
        "id": "chatcmpl-mock-8011",
        "object": "chat.completion",
        "created": created,
        "model": model,
        "choices": [
            {
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "ok",
                },
                "finish_reason": "stop",
            }
        ],
        "usage": {
            "prompt_tokens": 8,
            "completion_tokens": 1,
            "total_tokens": 9,
        },
    }


def stream_chunks(raw_model: str | None) -> list[str]:
    model = normalize_model(raw_model)
    created = int(time.time())
    first = {
        "id": "chatcmpl-mock-8011-stream",
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [
            {
                "index": 0,
                "delta": {"role": "assistant", "content": "ok"},
                "finish_reason": None,
            }
        ],
    }
    second = {
        "id": "chatcmpl-mock-8011-stream",
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
    return [
        f"data: {json.dumps(first, separators=(',', ':'))}\n\n",
        f"data: {json.dumps(second, separators=(',', ':'))}\n\n",
        "data: [DONE]\n\n",
    ]


class MockOpenAIHandler(BaseHTTPRequestHandler):
    server_version = "MockOpenAI8011/1.0"

    def do_GET(self) -> None:
        path = normalize_path(self.path)
        self.log_message("GET %s", path)
        if path == "/v1/models":
            self._send_json(HTTPStatus.OK, models_payload())
            return
        self._send_json(
            HTTPStatus.NOT_FOUND,
            {"error": {"message": f"Not found: {path}"}},
        )

    def do_POST(self) -> None:
        path = normalize_path(self.path)
        payload = self._read_json_body()
        model = normalize_model(payload.get("model"))
        stream = bool(payload.get("stream"))
        self.log_message(
            "POST %s model=%s stream=%s",
            path,
            model,
            stream,
        )

        if path == "/v1/chat/completions":
            if stream:
                self._send_stream(stream_chunks(payload.get("model")))
            else:
                self._send_json(
                    HTTPStatus.OK,
                    chat_completion_payload(payload.get("model")),
                )
            return

        self._send_json(
            HTTPStatus.NOT_FOUND,
            {"error": {"message": f"Not found: {path}"}},
        )

    def log_message(self, fmt: str, *args: Any) -> None:
        message = fmt % args
        sys.stderr.write(
            f"[mock-openai-8011] {self.client_address[0]} {message}\n"
        )

    def _read_json_body(self) -> dict[str, Any]:
        length = int(self.headers.get("Content-Length", "0"))
        raw = self.rfile.read(length) if length > 0 else b"{}"
        if not raw:
            return {}
        try:
            body = json.loads(raw.decode("utf-8"))
        except json.JSONDecodeError:
            body = {}
        return body if isinstance(body, dict) else {}

    def _send_json(
        self,
        status: HTTPStatus,
        body: dict[str, Any],
    ) -> None:
        payload = json.dumps(body, ensure_ascii=True).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def _send_stream(self, chunks: list[str]) -> None:
        self.send_response(HTTPStatus.OK)
        self.send_header("Content-Type", "text/event-stream; charset=utf-8")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Connection", "close")
        self.end_headers()
        for chunk in chunks:
            self.wfile.write(chunk.encode("utf-8"))
            self.wfile.flush()


def create_server(host: str, port: int) -> ThreadingHTTPServer:
    return ThreadingHTTPServer((host, port), MockOpenAIHandler)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Run a local OpenAI-compatible mock upstream."
    )
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=8011)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    server = create_server(args.host, args.port)
    host, port = server.server_address
    print(f"mock-openai-8011 listening on http://{host}:{port}", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("\nshutting down mock-openai-8011", flush=True)
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
