#!/usr/bin/env python3
"""Shared Gemini-native mock upstream for provider adaptation testing."""

from __future__ import annotations

import argparse
import json
import re
import socket
import sys
import time
from dataclasses import dataclass
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any
from urllib.parse import urlparse


MODEL_ROUTE_RE = re.compile(
    r"^/(?:v1beta(?:1)?/)?models/(?P<model>[^:]+):(?P<method>generateContent|streamGenerateContent)$"
)


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
    while normalized.startswith("/v1beta/v1beta/"):
        normalized = normalized[7:]
    while normalized.startswith("/v1beta1/v1beta1/"):
        normalized = normalized[8:]
    return normalized


def extract_route_parts(path: str) -> tuple[str | None, str | None]:
    match = MODEL_ROUTE_RE.match(normalize_path(path))
    if match is None:
        return None, None
    return match.group("model"), match.group("method")


def build_generate_content_payload(
    config: ProviderMockConfig,
    raw_model: str | None,
) -> dict[str, Any]:
    model = raw_model or config.models[0]
    return {
        "responseId": f"{config.provider_slug}-{model}-{int(time.time())}",
        "modelVersion": model,
        "candidates": [
            {
                "index": 0,
                "content": {
                    "role": "model",
                    "parts": [
                        {
                            "text": config.assistant_text,
                        }
                    ],
                },
                "finishReason": "STOP",
            }
        ],
        "usageMetadata": {
            "promptTokenCount": 12,
            "candidatesTokenCount": 6,
            "totalTokenCount": 18,
        },
    }


def build_stream_generate_content_chunks(
    config: ProviderMockConfig,
    raw_model: str | None,
) -> list[bytes]:
    payload = build_generate_content_payload(config, raw_model)
    response_id = payload["responseId"]
    model = payload["modelVersion"]
    first = {
        "responseId": response_id,
        "modelVersion": model,
        "candidates": [
            {
                "index": 0,
                "content": {
                    "role": "model",
                    "parts": [
                        {
                            "text": config.assistant_text,
                        }
                    ],
                },
            }
        ],
    }
    second = {
        "responseId": response_id,
        "modelVersion": model,
        "candidates": [
            {
                "index": 0,
                "finishReason": "STOP",
            }
        ],
        "usageMetadata": payload["usageMetadata"],
    }
    return [
        f"data: {json.dumps(first)}\n\n".encode("utf-8"),
        f"data: {json.dumps(second)}\n\n".encode("utf-8"),
    ]


def create_handler(config: ProviderMockConfig):
    class NativeGeminiMockHandler(BaseHTTPRequestHandler):
        server_version = "NativeGeminiMock/0.1"
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
            self.wfile.write(b"0\r\n\r\n")
            self.wfile.flush()

        def _write_not_found(self) -> None:
            self._write_bytes(
                HTTPStatus.NOT_FOUND,
                json.dumps({"error": "not found"}).encode("utf-8"),
                "application/json",
            )

        def do_POST(self) -> None:
            route_model, method = extract_route_parts(self.path)
            if route_model is None or method is None:
                self._write_not_found()
                return

            self._read_json()
            if method == "streamGenerateContent":
                self._write_stream(
                    build_stream_generate_content_chunks(config, route_model)
                )
                return

            self._write_json(build_generate_content_payload(config, route_model))

        def do_GET(self) -> None:
            self._write_not_found()

    return NativeGeminiMockHandler


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
