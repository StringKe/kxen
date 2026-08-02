#!/usr/bin/env node

import { createHash, createPublicKey, verify } from "node:crypto";
import { createReadStream } from "node:fs";
import { basename } from "node:path";
import { lstat, readFile } from "node:fs/promises";

function fail(message) {
  console.error(message);
  process.exit(1);
}

function decodeCanonicalBase64(value, label) {
  const encoded = value.trim();
  if (encoded.length === 0 || !/^[A-Za-z0-9+/]+={0,2}$/.test(encoded)) {
    fail(`${label} is not canonical base64`);
  }
  const decoded = Buffer.from(encoded, "base64");
  if (decoded.toString("base64") !== encoded) {
    fail(`${label} is not canonical base64`);
  }
  return decoded;
}

const [archivePath, signaturePath, encodedPublicKey] = process.argv.slice(2);
if (!archivePath || !signaturePath || !encodedPublicKey) {
  fail("usage: verify-updater-signature.mjs <archive> <signature> <tauri-public-key>");
}
if (encodedPublicKey.length > 16 * 1024) {
  fail("Tauri updater public key exceeds 16 KiB");
}

let signatureStat;
try {
  signatureStat = await lstat(signaturePath);
} catch (error) {
  fail(`unable to inspect Tauri updater signature: ${error.message}`);
}
if (!signatureStat.isFile()) {
  fail("Tauri updater signature must be a regular file");
}
if (signatureStat.size > 64 * 1024) {
  fail("Tauri updater signature exceeds 64 KiB");
}

const publicKeyBox = decodeCanonicalBase64(encodedPublicKey, "Tauri updater public key")
  .toString("utf8")
  .trimEnd()
  .split("\n");
if (publicKeyBox.length !== 2 || !publicKeyBox[0].startsWith("untrusted comment: ")) {
  fail("Tauri updater public key has an invalid minisign envelope");
}
const publicKeyPacket = decodeCanonicalBase64(publicKeyBox[1], "minisign public key");
if (publicKeyPacket.length !== 42 || publicKeyPacket.subarray(0, 2).toString("ascii") !== "Ed") {
  fail("Tauri updater public key has an unsupported packet");
}

const encodedSignature = await readFile(signaturePath, "utf8");
const signatureBox = decodeCanonicalBase64(encodedSignature, "Tauri updater signature")
  .toString("utf8")
  .trimEnd()
  .split("\n");
if (signatureBox.length !== 4 || !signatureBox[0].startsWith("untrusted comment: ")) {
  fail("Tauri updater signature has an invalid minisign envelope");
}
const signaturePacket = decodeCanonicalBase64(signatureBox[1], "minisign signature");
if (signaturePacket.length !== 74 || signaturePacket.subarray(0, 2).toString("ascii") !== "ED") {
  fail("Tauri updater signature is not an Ed25519 prehashed signature");
}
if (!signaturePacket.subarray(2, 10).equals(publicKeyPacket.subarray(2, 10))) {
  fail("Tauri updater signature key ID does not match the configured public key");
}

const expectedFile = basename(archivePath);
const trustedPrefix = "trusted comment: ";
if (!signatureBox[2].startsWith(trustedPrefix)) {
  fail("Tauri updater signature is missing its trusted comment");
}
const trustedComment = signatureBox[2].slice(trustedPrefix.length);
const escapedFile = expectedFile.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
if (!new RegExp(`^timestamp:[0-9]+\\tfile:${escapedFile}$`).test(trustedComment)) {
  fail(`Tauri updater signature trusted comment does not name ${expectedFile}`);
}
const globalSignature = decodeCanonicalBase64(signatureBox[3], "minisign global signature");
if (globalSignature.length !== 64) {
  fail("Tauri updater global signature has an invalid length");
}

const publicKeyDerPrefix = Buffer.from("302a300506032b6570032100", "hex");
const publicKey = createPublicKey({
  key: Buffer.concat([publicKeyDerPrefix, publicKeyPacket.subarray(10)]),
  format: "der",
  type: "spki",
});
const archiveHasher = createHash("blake2b512");
for await (const chunk of createReadStream(archivePath)) {
  archiveHasher.update(chunk);
}
const archiveDigest = archiveHasher.digest();
const signature = signaturePacket.subarray(10);
if (!verify(null, archiveDigest, publicKey, signature)) {
  fail("Tauri updater archive signature verification failed");
}
const signedComment = Buffer.concat([signature, Buffer.from(trustedComment)]);
if (!verify(null, signedComment, publicKey, globalSignature)) {
  fail("Tauri updater trusted comment signature verification failed");
}

console.log(`PASS updater signature: ${expectedFile}`);
