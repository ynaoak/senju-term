#!/usr/bin/env node
/**
 * Guards the packaging config against the drift that only shows up at release
 * time, when a tag has already been pushed.
 *
 * Every check here stands for a failure that is invisible until someone tries
 * to ship:
 *
 * - **macOS window drift.** Tauri merges `tauri.macos.conf.json` over the base
 *   config with RFC 7386 semantics, and arrays are *replaced*, not merged. So
 *   the macOS file has to repeat the whole window object; change the window
 *   size in the base config alone and macOS silently keeps the old one.
 * - **Version drift.** The workspace version and `tauri.conf.json`'s version
 *   are what the release name and the updater compare; if they disagree the
 *   updater can offer an "update" to a version the user already runs.
 * - **MSIX template drift.** The packaging script substitutes fixed tokens and
 *   the manifest references fixed asset paths. Rename either and the Store
 *   package fails to build (or builds with placeholder identity).
 *
 * Usage: node scripts/check-release-config.mjs
 */

import { readFileSync, existsSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = dirname(fileURLToPath(new URL('.', import.meta.url)));
const problems = [];
const fail = (msg) => problems.push(msg);
const readJson = (p) => JSON.parse(readFileSync(join(root, p), 'utf8'));

/* 1. macOS window config must match the base one, bar the macOS-only keys. */
const base = readJson('src-tauri/tauri.conf.json');
const mac = readJson('src-tauri/tauri.macos.conf.json');
// Keys that legitimately differ: macOS keeps native decorations so its real
// traffic lights can sit over the transparent title bar.
const MACOS_ONLY = new Set(['decorations', 'titleBarStyle', 'hiddenTitle']);
const baseWindow = base.app?.windows?.[0] ?? {};
const macWindow = mac.app?.windows?.[0] ?? {};
for (const [key, value] of Object.entries(baseWindow)) {
  if (MACOS_ONLY.has(key)) continue;
  if (JSON.stringify(macWindow[key]) !== JSON.stringify(value)) {
    fail(`tauri.macos.conf.json window.${key} is ${JSON.stringify(macWindow[key])}, expected ${JSON.stringify(value)} — the platform config replaces the whole windows array, so it must repeat every base field`);
  }
}
for (const key of Object.keys(macWindow)) {
  if (!MACOS_ONLY.has(key) && !(key in baseWindow)) {
    fail(`tauri.macos.conf.json window.${key} has no counterpart in tauri.conf.json`);
  }
}

/* 2. Versions must agree. */
const cargoVersion = readFileSync(join(root, 'Cargo.toml'), 'utf8')
  .match(/^\s*version\s*=\s*"([^"]+)"/m)?.[1];
if (cargoVersion !== base.version) {
  fail(`version mismatch: Cargo.toml has ${cargoVersion}, tauri.conf.json has ${base.version}`);
}

/* 3. The MSIX template and the script that fills it must still agree. */
const manifest = readFileSync(join(root, 'src-tauri/msstore/AppxManifest.xml'), 'utf8');
const script = readFileSync(join(root, 'scripts/build-msix.ps1'), 'utf8');
for (const token of ['__IDENTITY_NAME__', '__PUBLISHER__', '__PUBLISHER_DISPLAY_NAME__', '__VERSION__']) {
  if (!manifest.includes(token)) fail(`AppxManifest.xml no longer contains ${token}`);
  if (!script.includes(token)) fail(`build-msix.ps1 no longer substitutes ${token}`);
}
// Every logo the manifest names has to exist, or makeappx fails.
for (const asset of manifest.match(/assets\\[A-Za-z0-9.]+/g) ?? []) {
  const rel = join('src-tauri/msstore', asset.replace('\\', '/'));
  if (!existsSync(join(root, rel))) fail(`AppxManifest.xml references a missing asset: ${rel}`);
}
// The manifest names the executable the packaging script copies into place.
const exeName = manifest.match(/Executable="([^"]+)"/)?.[1];
if (exeName && !script.includes(exeName)) {
  fail(`AppxManifest.xml declares Executable="${exeName}" but build-msix.ps1 does not write that name`);
}

if (problems.length) {
  console.error(`release config check: ${problems.length} problem(s)`);
  for (const p of problems) console.error(`  - ${p}`);
  process.exit(1);
}
console.log('release config check: OK');
