#!/usr/bin/env node

import crypto from "node:crypto";

function usage() {
  console.log(`Usage:
  node scripts/gen-virtual-key-hash.mjs --auth "Bearer <virtual_key>"
  node scripts/gen-virtual-key-hash.mjs --key "<virtual_key>"

Notes:
  - Gateway extracts key from "Bearer <key>" then stores SHA-256(key).
  - Output is lowercase hex suitable for virtual_keys.key_hash.
`);
}

function getArg(flag) {
  const idx = process.argv.indexOf(flag);
  if (idx === -1 || idx + 1 >= process.argv.length) return undefined;
  return process.argv[idx + 1];
}

const auth = getArg("--auth");
const keyArg = getArg("--key");

if (process.argv.includes("--help") || process.argv.includes("-h")) {
  usage();
  process.exit(0);
}

if (!auth && !keyArg) {
  usage();
  process.exit(1);
}

let key = keyArg;
if (!key && auth) {
  key = auth.startsWith("Bearer ") ? auth.slice("Bearer ".length) : auth;
}

if (!key || key.length === 0) {
  console.error("Error: virtual key is empty.");
  process.exit(1);
}

const normalizedKey = key.startsWith("Bearer ") ? key.slice("Bearer ".length) : key;
const hashHex = crypto.createHash("sha256").update(normalizedKey, "utf8").digest("hex");

console.log(hashHex);
