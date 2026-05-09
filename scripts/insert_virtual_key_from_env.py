#!/usr/bin/env python3
"""
Insert one row into virtual_keys for local/dev use.

Resolves workspace_id from master_keys (must match the given master_key_id).
Virtual key material is hashed like ai-gateway virtual_key::legacy_key::hash_key:
SHA-256 of the key string (strip a leading \"Bearer \" prefix), lowercase hex, 64 chars.
key_prefix is the first 16 characters of that key material (same as unified_api tests).

Database URL: POSTGRES_DATABASE_URL or AI_GATEWAY__DATABASE__URL or DATABASE_URL from --env-file, or --database-url.

Requires: pip install \"psycopg[binary]\"

Examples:
  # Use an existing plaintext VK (e.g. sk-...) from env
  export MASTER_KEY_ID='<uuid from insert_*_master_key output>'
  export VIRTUAL_KEY='sk-my-local-vk-12345'
  python3 scripts/insert_virtual_key_from_env.py

  python3 scripts/insert_virtual_key_from_env.py \\
    --master-key-id '<uuid>' --virtual-key 'sk-test' --label my-vk

  # Generate a new sk-local-... key and insert (prints the secret once)
  python3 scripts/insert_virtual_key_from_env.py --master-key-id '<uuid>' --generate
"""

from __future__ import annotations

import argparse
import hashlib
import os
import secrets
import sys
from pathlib import Path
from uuid import UUID

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


def key_material(key: str) -> str:
    """Strip leading \"Bearer \" (case on prefix only); same as legacy_key::hash_key input."""
    material = key.strip()
    if material.lower().startswith("bearer "):
        material = material[7:].lstrip()
    return material


def hash_key(key: str) -> str:
    """Match ai-gateway/src/virtual_key/legacy_key.rs hash_key."""
    material = key_material(key)
    digest = hashlib.sha256(material.encode("utf-8")).digest()
    return digest.hex()


def key_prefix_from_virtual_key(key: str) -> str:
    """First 16 Unicode chars of key material (cf. unified_api tests .chars().take(16))."""
    material = key_material(key)
    return "".join(list(material)[:16])


def parse_uuid(s: str, name: str) -> UUID:
    try:
        return UUID(s.strip())
    except ValueError as e:
        raise SystemExit(f"Error: {name} is not a valid UUID: {e}") from e


def main() -> int:
    parser = argparse.ArgumentParser(
        description=(
            "Insert virtual_keys row: MASTER_KEY_ID + VIRTUAL_KEY (or --generate). "
            "workspace_id is taken from master_keys."
        ),
    )
    parser.add_argument(
        "--env-file",
        type=Path,
        default=DEFAULT_ENV,
        help=f"Dotenv file (default: {DEFAULT_ENV})",
    )
    parser.add_argument(
        "--database-url",
        default="",
        help=(
            "PostgreSQL URL. If empty, uses POSTGRES_DATABASE_URL or AI_GATEWAY__DATABASE__URL or DATABASE_URL "
            "from env after loading --env-file."
        ),
    )
    parser.add_argument(
        "--master-key-id",
        default="",
        help="master_keys.id (default: env MASTER_KEY_ID)",
    )
    parser.add_argument(
        "--virtual-key",
        default="",
        help="Plaintext VK clients send (default: env VIRTUAL_KEY). Ignored if --generate.",
    )
    parser.add_argument(
        "--generate",
        action="store_true",
        help="Generate sk-local-<hex> and insert it (prints secret once).",
    )
    parser.add_argument(
        "--label",
        default="local-vk-from-env",
        help="virtual_keys.label (max 100 chars)",
    )
    parser.add_argument(
        "--entity-type",
        default="",
        help="Optional vk_entity_type_enum e.g. agent, member (requires --entity-id)",
    )
    parser.add_argument(
        "--entity-id",
        default="",
        help="UUID for entity (required if --entity-type is set)",
    )
    args = parser.parse_args()

    load_env_file(args.env_file)

    master_key_id_str = (args.master_key_id or os.environ.get("MASTER_KEY_ID") or "").strip()
    if not master_key_id_str:
        print(
            "Error: MASTER_KEY_ID missing. Pass --master-key-id or set env MASTER_KEY_ID.",
            file=sys.stderr,
        )
        return 1
    master_key_id = parse_uuid(master_key_id_str, "MASTER_KEY_ID")

    if args.generate:
        virtual_key = f"sk-local-{secrets.token_hex(24)}"
        print("Generated VIRTUAL_KEY (save this; shown once):", file=sys.stderr)
        print(virtual_key, file=sys.stderr)
        print(file=sys.stderr)
    else:
        virtual_key = (args.virtual_key or os.environ.get("VIRTUAL_KEY") or "").strip()
        if not virtual_key:
            print(
                "Error: VIRTUAL_KEY missing. Set env VIRTUAL_KEY, pass --virtual-key, "
                "or use --generate.",
                file=sys.stderr,
            )
            return 1

    entity_type = (args.entity_type or "").strip() or None
    entity_id_str = (args.entity_id or "").strip() or None
    if (entity_type is None) ^ (entity_id_str is None):
        print(
            "Error: set both --entity-type and --entity-id, or neither.",
            file=sys.stderr,
        )
        return 1
    entity_id = parse_uuid(entity_id_str, "entity_id") if entity_id_str else None

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

    kh = hash_key(virtual_key)
    prefix = key_prefix_from_virtual_key(virtual_key)
    label = args.label[:100]

    try:
        with psycopg.connect(db_url, autocommit=True) as conn:
            with conn.cursor() as cur:
                cur.execute(
                    """
                    SELECT workspace_id
                    FROM master_keys
                    WHERE id = %s
                      AND deleted_at IS NULL
                      AND status = 'active'
                    """,
                    (master_key_id,),
                )
                row = cur.fetchone()
                if not row:
                    print(
                        "Error: master_keys row not found, deleted, or not active for "
                        f"id={master_key_id}.",
                        file=sys.stderr,
                    )
                    return 1
                workspace_id = row[0]

                cur.execute(
                    """
                    INSERT INTO virtual_keys (
                        workspace_id,
                        master_key_id,
                        label,
                        key_hash,
                        key_prefix,
                        entity_type,
                        entity_id,
                        status,
                        period_spend_cents,
                        period_request_count,
                        period_start,
                        deleted_at,
                        created_at,
                        updated_at
                    )
                    VALUES (
                        %s,
                        %s,
                        %s,
                        %s,
                        %s,
                        %s,
                        %s,
                        'active'::virtual_key_status_enum,
                        0,
                        0,
                        CURRENT_DATE,
                        NULL,
                        now(),
                        now()
                    )
                    RETURNING id
                    """,
                    (
                        workspace_id,
                        master_key_id,
                        label,
                        kh,
                        prefix,
                        entity_type,
                        entity_id,
                    ),
                )
                new_id = cur.fetchone()[0]
    except Exception as e:
        print(f"Error: database: {e}", file=sys.stderr)
        return 1

    print(f"Inserted virtual_keys.id = {new_id}")
    print(f"  workspace_id   = {workspace_id}")
    print(f"  master_key_id  = {master_key_id}")
    print(f"  label          = {label}")
    print(f"  key_hash       = {kh}")
    print(f"  key_prefix     = {prefix}")
    print("Use Authorization: Bearer <VIRTUAL_KEY> (or raw key) against the gateway.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
