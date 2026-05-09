#!/usr/bin/env python3
"""
Load from repo .env (default: <repo>/.env): provider API key, MASTER_KEY_ENCRYPTION_KEY,
and database URL (POSTGRES_DATABASE_URL or AI_GATEWAY__DATABASE__URL or DATABASE_URL). Encrypt for master_keys
(AES-256-GCM, same as ai-gateway crypto/master_key.rs), INSERT one provider row into
PostgreSQL. No hardcoded default DB URL - URL must come from .env or --database-url.

Requires: pip install cryptography "psycopg[binary]"

Examples:
  python3 scripts/insert_master_key_from_env.py \
    --provider-code openai \
    --api-key "$OPENAI_API_KEY"

  python3 scripts/insert_master_key_from_env.py \
    --provider-code deepseek \
    --api-key-env DEEPSEEK_API_KEY
"""

from __future__ import annotations

import argparse
import base64
import os
import sys
from pathlib import Path

KEY_LEN = 32
NONCE_LEN = 12

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_ENV = REPO_ROOT / ".env"


def load_env_file(path: Path) -> None:
    """Set os.environ from KEY=value lines (strip quotes). Later lines override."""
    if not path.is_file():
        return
    for raw in path.read_text(encoding="utf-8").splitlines():
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


def encrypt_api_key(plaintext: str, master_key_32: bytes) -> tuple[bytes, bytes]:
    from cryptography.hazmat.primitives.ciphers.aead import AESGCM

    aes = AESGCM(master_key_32)
    nonce = os.urandom(NONCE_LEN)
    ciphertext = aes.encrypt(nonce, plaintext.encode("utf-8"), None)
    return ciphertext, nonce


def mask_key(api_key: str) -> str:
    """master_keys.masked_key is varchar(20)."""
    api_key = api_key.strip()
    if len(api_key) <= 8:
        return (api_key + "...")[:20]
    return (api_key[:3] + "..." + api_key[-4:])[:20]


def resolve_api_key(args: argparse.Namespace, provider_code: str) -> tuple[str, str]:
    api_key = (args.api_key or "").strip()
    env_name = (args.api_key_env or "").strip()

    if api_key:
        return api_key, ("argument --api-key" if not env_name else env_name)

    if not env_name:
        env_name = f"{provider_code.upper()}_API_KEY"

    api_key = (os.environ.get(env_name) or "").strip()
    if not api_key:
        raise ValueError(
            f"{env_name} missing in environment (after loading {args.env_file}). "
            "Pass --api-key directly, or set --api-key-env."
        )

    return api_key, env_name


def main() -> int:
    parser = argparse.ArgumentParser(
        description=(
            "Insert provider master_keys row: provider code + key + DB URL from --env-file "
            "(MASTER_KEY_ENCRYPTION_KEY, POSTGRES_DATABASE_URL or AI_GATEWAY__DATABASE__URL "
            "or DATABASE_URL), unless --database-url is set."
        ),
    )
    parser.add_argument(
        "--env-file",
        type=Path,
        default=DEFAULT_ENV,
        help=f"Dotenv file (default: {DEFAULT_ENV})",
    )
    parser.add_argument(
        "--provider-code",
        required=True,
        help="providers.code to bind (e.g. openai, openrouter, anthropic, google, deepseek, zai)",
    )
    parser.add_argument(
        "--api-key",
        default="",
        help="Provider API key plaintext; if empty, resolved from --api-key-env or <PROVIDER>_API_KEY.",
    )
    parser.add_argument(
        "--api-key-env",
        default="",
        help="Environment variable for API key (e.g. GEMINI_API_KEY). Default: <PROVIDER_CODE>_API_KEY.",
    )
    parser.add_argument(
        "--database-url",
        default="",
        help=(
            "PostgreSQL URL. If empty, uses POSTGRES_DATABASE_URL or AI_GATEWAY__DATABASE__URL or DATABASE_URL "
            "from --env-file (after load)."
        ),
    )
    parser.add_argument(
        "--label",
        default="",
        help="master_keys.label. Default: local-<provider-code>-from-env",
    )
    args = parser.parse_args()

    load_env_file(args.env_file)

    provider_code = args.provider_code.strip().lower()
    if not provider_code:
        print("Error: --provider-code cannot be empty.", file=sys.stderr)
        return 1

    try:
        api_key, api_key_source = resolve_api_key(args, provider_code)
    except ValueError as e:
        print(f"Error: {e}", file=sys.stderr)
        return 1

    label = (args.label or f"local-{provider_code}-from-env")[:100]
    b64 = (os.environ.get("MASTER_KEY_ENCRYPTION_KEY") or "").strip()
    if not b64:
        print(
            f"Error: MASTER_KEY_ENCRYPTION_KEY missing (after loading {args.env_file})",
            file=sys.stderr,
        )
        return 1

    try:
        dek = base64.b64decode(b64, validate=True)
    except Exception as e:
        print(f"Error: MASTER_KEY_ENCRYPTION_KEY Base64 invalid: {e}", file=sys.stderr)
        return 1
    if len(dek) != KEY_LEN:
        print(
            f"Error: MASTER_KEY_ENCRYPTION_KEY must decode to {KEY_LEN} bytes, got {len(dek)}",
            file=sys.stderr,
        )
        return 1

    db_url = (args.database_url or "").strip()
    if not db_url:
        db_url = (os.environ.get("POSTGRES_DATABASE_URL") or "").strip()
    if not db_url:
        db_url = (os.environ.get("AI_GATEWAY__DATABASE__URL") or "").strip()
    if not db_url:
        db_url = (os.environ.get("DATABASE_URL") or "").strip()
    if not db_url:
        print(
            "Error: database URL missing. Set POSTGRES_DATABASE_URL or AI_GATEWAY__DATABASE__URL or DATABASE_URL "
            f"in {args.env_file}, or pass --database-url.",
            file=sys.stderr,
        )
        return 1

    try:
        import psycopg
    except ImportError:
        print('Error: pip install "psycopg[binary]"', file=sys.stderr)
        return 1

    key_ciphertext, key_nonce = encrypt_api_key(api_key, dek)
    masked = mask_key(api_key)

    try:
        with psycopg.connect(db_url, autocommit=True) as conn:
            with conn.cursor() as cur:
                cur.execute(
                    """
                    SELECT id FROM workspaces
                    WHERE deleted_at IS NULL
                    ORDER BY created_at
                    LIMIT 1
                    """
                )
                row = cur.fetchone()
                if not row:
                    print(
                        "Error: no row in workspaces; seed DB or create a workspace first.",
                        file=sys.stderr,
                    )
                    return 1
                workspace_id = row[0]

                cur.execute(
                    """
                    SELECT id FROM providers
                    WHERE lower(code) = %s AND enabled = true
                    LIMIT 1
                    """,
                    (provider_code,),
                )
                row = cur.fetchone()
                if not row:
                    print(
                        f"Error: no enabled provider with code '{provider_code}'.",
                        file=sys.stderr,
                    )
                    return 1
                provider_id = row[0]

                cur.execute(
                    """
                    INSERT INTO master_keys (
                        workspace_id,
                        label,
                        provider_id,
                        key_ciphertext,
                        key_nonce,
                        key_salt,
                        masked_key,
                        base_url,
                        status
                    )
                    VALUES (
                        %s,
                        %s,
                        %s,
                        %s,
                        %s,
                        NULL,
                        %s,
                        NULL,
                        'active'
                    )
                    RETURNING id
                    """,
                    (
                        workspace_id,
                        label,
                        provider_id,
                        key_ciphertext,
                        key_nonce,
                        masked,
                    ),
                )
                new_id = cur.fetchone()[0]
    except Exception as e:
        print(f"Error: database: {e}", file=sys.stderr)
        return 1

    print(f"Inserted master_keys.id = {new_id}")
    print(f"  workspace_id   = {workspace_id}")
    print(f"  provider_id    = {provider_id} ({provider_code})")
    print(f"  label          = {label}")
    print(f"  api_key_source = {api_key_source}")
    print(f"  masked_key     = {masked}")
    print(
        "Bind a virtual_keys.master_key_id to this UUID (or use existing VK) to call the gateway."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
