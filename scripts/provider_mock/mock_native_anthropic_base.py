#!/usr/bin/env python3
"""Shared Anthropic-native mock upstream for provider adaptation testing."""

from __future__ import annotations

import argparse
import json
import socket
import sys
import time
from dataclasses import dataclass
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any
from urllib.parse import urlparse


@dataclass(frozen=True)
class ProviderMockConfig:
    provider_slug: str
    display_name: str
    default_port: int
    models: tuple[str, ...]
    assistant_text: str


def normalize_path(path: str) -> str:
    parsed = urlparse(path)
    normalized = parsed.path
    while normalized.startswith("/v1/v1/"):
        normalized = normalized[3:]
    return normalized


def pick_response_model(raw_model: str | None, config: ProviderMockConfig) -> str:
    if raw_model:
        return raw_model
    return config.models[0]


def build_messages_payload(
    config: ProviderMockConfig,
    raw_model: str | None,
) -> dict[str, Any]:
    model = pick_response_model(raw_model, config)
    return {
        "id": f"msg_{config.provider_slug}_mock",
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": [
            {
                "type": "text",
                "text": config.assistant_text,
            }
        ],
        "stop_reason": "end_turn",
        "stop_sequence": None,
        "usage": {
            "input_tokens": 12,
            "output_tokens": 6,
        },
    }


def build_messages_stream_chunks(
    config: ProviderMockConfig,
    raw_model: str | None,
) -> list[bytes]:
    payload = build_messages_payload(config, raw_model)
    usage = payload["usage"]
    text = payload["content"][0]["text"]
    chunks = [
        {
            "event": "message_start",
            "data": {
                "type": "message_start",
                "message": {
                    "id": payload["id"],
                    "type": "message",
                    "role": "assistant",
                    "model": payload["model"],
                    "content": [],
                    "stop_reason": None,
                    "stop_sequence": None,
                    "usage": {
                        "input_tokens": usage["input_tokens"],
                        "output_tokens": 0,
                    },
                },
            },
        },
        {
            "event": "content_block_start",
            "data": {
                "type": "content_block_start",
                "index": 0,
                "content_block": {
                    "type": "text",
                    "text": "",
                },
            },
        },
        {
            "event": "content_block_delta",
            "data": {
                "type": "content_block_delta",
                "index": 0,
                "delta": {
                    "type": "text_delta",
                    "text": text,
                },
            },
        },
        {
            "event": "content_block_stop",
            "data": {
                "type": "content_block_stop",
                "index": 0,
            },
        },
        {
            "event": "message_delta",
            "data": {
                "type": "message_delta",
                "delta": {
                    "stop_reason": payload["stop_reason"],
                    "stop_sequence": payload["stop_sequence"],
                },
                "usage": {
                    "output_tokens": usage["output_tokens"],
                },
            },
        },
        {
            "event": "message_stop",
            "data": {
                "type": "message_stop",
                "usage": usage,
            },
        },
    ]
    return [
        f"event: {chunk['event']}\ndata: {json.dumps(chunk['data'])}\n\n".encode(
            "utf-8"
        )
        for chunk in chunks
    ]


def create_handler(config: ProviderMockConfig):
    class NativeAnthropicMockHandler(BaseHTTPRequestHandler):
        server_version = "NativeAnthropicMock/0.1"
        protocol_version = "HTTP/1.1"

        def log_message(self, format: str, *args) -> None:
            return

        def _read_json(self) -> dict[str, Any]:
            content_length = int(self.headers.get("Content-Length", "0"))
            raw_body = self.rfile.read(content_length) if content_length else b"{}"
            if not raw_body:
                return {}
            return json.loads(raw_body.decode("utf-8"))

        def _write_bytes(
            self,
            status: HTTPStatus,
            body: bytes,
            content_type: str,
        ) -> None:
            self.send_response(status)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def _write_json(self, payload: dict[str, Any]) -> None:
            self._write_bytes(
                HTTPStatus.OK,
                json.dumps(payload).encode("utf-8"),
                "application/json",
            )

        def _write_stream(self, chunks: list[bytes]) -> None:
            self.send_response(HTTPStatus.OK)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Cache-Control", "no-cache")
            self.send_header("Transfer-Encoding", "chunked")
            self.end_headers()
            self.connection.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
            for chunk in chunks:
                size_line = f"{len(chunk):X}\r\n".encode("utf-8")
                self.wfile.write(size_line)
                self.wfile.write(chunk)
                self.wfile.write(b"\r\n")
                self.wfile.flush()
                time.sleep(0.01)
            self.wfile.write(b"0\r\n\r\n")
            self.wfile.flush()

        def _write_not_found(self) -> None:
            self._write_bytes(
                HTTPStatus.NOT_FOUND,
                json.dumps({"error": "not found"}).encode("utf-8"),
                "application/json",
            )

        def do_POST(self) -> None:
            path = normalize_path(self.path)
            if path != "/v1/messages":
                self._write_not_found()
                return

            payload = self._read_json()
            raw_model = payload.get("model")
            if payload.get("stream") is True:
                self._write_stream(build_messages_stream_chunks(config, raw_model))
                return

            self._write_json(build_messages_payload(config, raw_model))

        def do_GET(self) -> None:
            self._write_not_found()

    return NativeAnthropicMockHandler


def create_server(
    config: ProviderMockConfig,
    host: str,
    port: int,
) -> ThreadingHTTPServer:
    return ThreadingHTTPServer((host, port), create_handler(config))


def run_server(config: ProviderMockConfig, host: str, port: int) -> int:
    server = create_server(config, host, port)
    actual_port = server.server_address[1]
    print(
        f"[{config.display_name}] native mock listening on http://{host}:{actual_port}",
        file=sys.stderr,
    )
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
    return 0


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=0)
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv or sys.argv[1:])
    raise NotImplementedError("Use a provider-specific launcher script.")


if __name__ == "__main__":
    raise SystemExit(main())
