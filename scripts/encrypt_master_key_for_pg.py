#!/usr/bin/env python3
"""
Encrypt a provider API key for master_keys.key_ciphertext / key_nonce.

Uses AES-256-GCM with a random 12-byte nonce, matching
ai-gateway/src/crypto/master_key.rs and scripts/gen-master-key-sql.mjs.

Outputs hex strings ready for PostgreSQL:
  decode('<hex>', 'hex')

Dependencies:
  pip install cryptography
"""

from __future__ import annotations

import argparse
import base64
import os
import sys

KEY_LEN = 32
NONCE_LEN = 12


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Generate master_keys ciphertext/nonce for decode(..., 'hex').",
    )
    parser.add_argument(
        "--api-key",
        required=True,
        help="Upstream provider API key plaintext (e.g. sk-...).",
    )
    parser.add_argument(
        "--master-key-id",
        default="<master_key_id>",
        help="UUID for the generated SQL WHERE clause.",
    )
    parser.add_argument(
        "--master-encryption-key",
        default=os.environ.get("MASTER_KEY_ENCRYPTION_KEY", ""),
        help="Base64-encoded 32-byte key (default: env MASTER_KEY_ENCRYPTION_KEY).",
    )
    args = parser.parse_args()

    b64 = (args.master_encryption_key or "").strip()
    if not b64:
        print(
            "Error: set MASTER_KEY_ENCRYPTION_KEY or pass --master-encryption-key",
            file=sys.stderr,
        )
        return 1

    try:
        key = base64.b64decode(b64, validate=True)
    except Exception as e:
        print(f"Error: MASTER_KEY_ENCRYPTION_KEY is not valid Base64: {e}", file=sys.stderr)
        return 1

    if len(key) != KEY_LEN:
        print(
            f"Error: key must decode to {KEY_LEN} bytes, got {len(key)}",
            file=sys.stderr,
        )
        return 1

    try:
        from cryptography.hazmat.primitives.ciphers.aead import AESGCM
    except ImportError:
        print("Error: install cryptography: pip install cryptography", file=sys.stderr)
        return 1

    aes = AESGCM(key)
    nonce = os.urandom(NONCE_LEN)
    plaintext = args.api_key.encode("utf-8")
    # Ciphertext includes 16-byte GCM auth tag (same layout as Rust aes-gcm / Node script).
    key_ciphertext = aes.encrypt(nonce, plaintext, None)

    nonce_hex = nonce.hex()
    ciphertext_hex = key_ciphertext.hex()

    print(f"key_nonce (hex):      {nonce_hex}")
    print(f"key_ciphertext (hex): {ciphertext_hex}")
    print("")
    print("PostgreSQL decode(...) fragments:")
    print(f"  decode('{ciphertext_hex}', 'hex')")
    print(f"  decode('{nonce_hex}', 'hex')")
    print("")
    print("SQL:")
    print(
        f"""UPDATE master_keys
SET
  key_ciphertext = decode('{ciphertext_hex}', 'hex'),
  key_nonce      = decode('{nonce_hex}', 'hex'),
  key_salt       = NULL,
  updated_at     = now()
WHERE id = '{args.master_key_id}'::uuid;"""
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
