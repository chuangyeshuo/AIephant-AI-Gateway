#!/usr/bin/env node

import crypto from "node:crypto";

const KEY_LEN = 32;
const NONCE_LEN = 12;

function usage() {
  console.log(`Usage:
  node scripts/gen-master-key-sql.mjs --api-key "<provider_api_key>" [--master-key-id "<uuid>"]

Environment:
  MASTER_KEY_ENCRYPTION_KEY   Base64 string, must decode to 32 bytes

Output:
  - key_nonce (hex)
  - key_ciphertext (hex; ciphertext + GCM auth tag)
  - SQL UPDATE statement for master_keys
`);
}

function getArg(flag) {
  const idx = process.argv.indexOf(flag);
  if (idx === -1 || idx + 1 >= process.argv.length) return undefined;
  return process.argv[idx + 1];
}

if (process.argv.includes("--help") || process.argv.includes("-h")) {
  usage();
  process.exit(0);
}

const apiKey = getArg("--api-key");
const masterKeyId = getArg("--master-key-id") || "<master_key_id>";
const b64 = process.env.MASTER_KEY_ENCRYPTION_KEY;

if (!apiKey) {
  console.error("Error: missing --api-key");
  usage();
  process.exit(1);
}

if (!b64) {
  console.error("Error: missing MASTER_KEY_ENCRYPTION_KEY env var");
  process.exit(1);
}

let key;
try {
  key = Buffer.from(b64.trim(), "base64");
} catch (e) {
  console.error("Error: MASTER_KEY_ENCRYPTION_KEY is not valid Base64");
  process.exit(1);
}

if (key.length !== KEY_LEN) {
  console.error(
    `Error: MASTER_KEY_ENCRYPTION_KEY must decode to ${KEY_LEN} bytes, got ${key.length}`,
  );
  process.exit(1);
}

const nonce = crypto.randomBytes(NONCE_LEN);
const cipher = crypto.createCipheriv("aes-256-gcm", key, nonce);
const ciphertext = Buffer.concat([cipher.update(apiKey, "utf8"), cipher.final()]);
const authTag = cipher.getAuthTag();
const keyCiphertext = Buffer.concat([ciphertext, authTag]);

const nonceHex = nonce.toString("hex");
const ciphertextHex = keyCiphertext.toString("hex");

console.log(`key_nonce (hex):      ${nonceHex}`);
console.log(`key_ciphertext (hex): ${ciphertextHex}`);
console.log("");
console.log("SQL:");
console.log(`UPDATE master_keys
SET
  key_ciphertext = decode('${ciphertextHex}', 'hex'),
  key_nonce      = decode('${nonceHex}', 'hex'),
  key_salt       = NULL,
  updated_at     = now()
WHERE id = '${masterKeyId}'::uuid;`);
