#!/usr/bin/env python3
"""Simulate POST /v1/log/request using auth from repo-root .env.

Reads ALEPHANT_CONTROL_PLANE_API_KEY from .env (same key the gateway uses
for Bearer auth). Optional env overrides:

  LOG_SERVICE_BASE_URL   default http://127.0.0.1:8586
"""

from __future__ import annotations

import json
import os
import sys
import uuid
from datetime import datetime, timezone
from pathlib import Path
from urllib.error import HTTPError
from urllib.request import Request, urlopen


def _repo_root() -> Path:
    return Path(__file__).resolve().parents[1]


def load_dotenv(path: Path) -> None:
    if not path.is_file():
        return
    text = path.read_text(encoding="utf-8")
    for raw in text.splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        if "=" not in line:
            continue
        key, _, val = line.partition("=")
        key = key.strip()
        val = val.strip()
        if len(val) >= 2 and val[0] == val[-1] and val[0] in "\"'":
            val = val[1:-1]
        os.environ[key] = val


def _utc_iso_ms_z() -> str:
    dt = datetime.now(timezone.utc)
    return dt.strftime("%Y-%m-%dT%H:%M:%S.") + f"{dt.microsecond // 1000:03d}Z"


def build_payload(authorization: str) -> dict:
    req_id = str(uuid.uuid4())
    resp_id = str(uuid.uuid4())
    user_id = str(uuid.uuid4())
    workspace_id = str(uuid.uuid4())
    t_req = _utc_iso_ms_z()
    t_resp = _utc_iso_ms_z()

    request_obj: dict = {
        "id": req_id,
        "userId": user_id,
        "workspaceId": workspace_id,
        "properties": {},
        "targetUrl": "https://api.openai.com/v1/chat/completions",
        "provider": "OPENAI",
        "bodySize": 120.0,
        "path": "/v1/chat/completions",
        "requestCreatedAt": t_req,
        "isStream": False,
        "body": "",
        "bodyTtlDays": 90,
        "threat": False,
    }

    return {
        "authorization": authorization,
        "alephantMeta": {
            "omitRequestLog": False,
            "omitResponseLog": False,
            "webhookEnabled": False,
            "gatewayDeploymentTarget": "sidecar",
            "gatewayModel": "gpt-4o-mini",
            "gatewayProvider": "openai",
            "providerModelId": "gpt-4o-mini",
            "isPassthroughBilling": True,
            "aiGatewayBodyMapping": "OPENAI",
        },
        "log": {
            "request": request_obj,
            "response": {
                "id": resp_id,
                "status": 200,
                "bodySize": 400.0,
                "delayMs": 842,
                "timeToFirstToken": 0,
                "responseCreatedAt": t_resp,
                "body": "",
                "model": "gpt-4o-mini",
                "promptTokens": 18,
                "completionTokens": 32,
                "promptCacheWriteTokens": 0,
                "promptCacheReadTokens": 0,
                "promptAudioTokens": 0,
                "completionAudioTokens": 0,
                "reasoningTokens": 0,
                "cost": 0,
            },
        },
    }


def main() -> int:
    load_dotenv(_repo_root() / ".env")
    try:
        auth = os.environ["ALEPHANT_CONTROL_PLANE_API_KEY"].strip()
    except KeyError:
        print(
            "error: ALEPHANT_CONTROL_PLANE_API_KEY not set "
            "(add to .env at repo root)",
            file=sys.stderr,
        )
        return 1
    if not auth:
        print("error: ALEPHANT_CONTROL_PLANE_API_KEY is empty", file=sys.stderr)
        return 1

    base = os.environ.get(
        "LOG_SERVICE_BASE_URL",
        "http://127.0.0.1:8586",
    ).rstrip("/")
    url = f"{base}/v1/log/request"
    body = json.dumps(build_payload(auth)).encode("utf-8")
    req = Request(
        url,
        data=body,
        method="POST",
        headers={
            "Content-Type": "application/json",
            "Authorization": f"Bearer {auth}",
        },
    )
    try:
        with urlopen(req, timeout=60) as resp:
            out = resp.read().decode("utf-8", errors="replace")
            if out:
                print(out)
            print(f"HTTP {resp.status}", file=sys.stderr)
    except HTTPError as e:
        err_body = e.read().decode("utf-8", errors="replace")
        print(err_body, file=sys.stderr)
        print(f"HTTP {e.code}", file=sys.stderr)
        return 1
    except OSError as e:
        print(f"error: request failed: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
