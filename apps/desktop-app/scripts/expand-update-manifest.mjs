#!/usr/bin/env node
/**
 * Expands a release's `latest.json` so every install the app ships can find
 * *its own* update.
 *
 * tauri-action writes one entry per build job, keyed by what it built. The
 * updater plugin looks for something narrower, and two gaps fall out of that:
 *
 * 1. **macOS never matches.** A universal build is published as
 *    `darwin-universal`, but the plugin asks for `darwin-{arch}` — the running
 *    process's arch, `aarch64` or `x86_64`. There is no "universal" fallback in
 *    the plugin, so the key is never found and macOS silently never updates.
 *    Both arch keys are aliased onto the universal artifact.
 *
 * 2. **Windows crosses installer formats.** The plugin looks for
 *    `{os}-{arch}-{installer}` before `{os}-{arch}`, which is exactly how an
 *    MSI install is meant to be kept on MSI and an NSIS install on NSIS. Only
 *    the bare key is published, so whichever installer tauri-action happened to
 *    record is served to *both* — leaving the machine with two registered
 *    copies of the app. Per-installer keys are added so each stays on its own
 *    format.
 *
 * The bare keys are left in place as a fallback for clients built before this
 * existed. Existing explicit keys are never overwritten.
 *
 * Usage:
 *   node scripts/expand-update-manifest.mjs --manifest latest.json [--assets assets.json]
 *                                           [--sig-dir dir] [--out out.json]
 *   node scripts/expand-update-manifest.mjs --self-test
 *
 * `--assets` is `gh release view --json assets` output; `--sig-dir` holds the
 * downloaded `*.sig` files. Both are needed only to add an installer key whose
 * artifact is not already in the manifest (the MSI case).
 */

import { readFileSync, writeFileSync, existsSync } from 'node:fs';
import { join } from 'node:path';

/** Maps an artifact file name to the updater's installer id, or null. */
export function installerOf(name) {
  const n = name.toLowerCase();
  if (n.endsWith('.nsis.zip') || n.endsWith('-setup.exe')) return 'nsis';
  if (n.endsWith('.msi.zip') || n.endsWith('.msi')) return 'msi';
  if (n.endsWith('.app.tar.gz')) return 'app';
  if (n.endsWith('.appimage.tar.gz') || n.endsWith('.appimage')) return 'appimage';
  return null;
}

const fileName = (url) => decodeURIComponent(url.split('/').pop() || '');

/**
 * @param {object} manifest parsed latest.json
 * @param {Array<{name: string, url: string}>} assets release assets, if known
 * @param {(name: string) => string|null} readSig reads a `.sig` file's contents
 * @returns {{manifest: object, added: string[]}}
 */
export function expand(manifest, assets = [], readSig = () => null) {
  const platforms = { ...(manifest.platforms || {}) };
  const added = [];
  const put = (key, value) => {
    if (platforms[key]) return; // an explicit entry always wins
    platforms[key] = value;
    added.push(key);
  };

  // 1. A universal macOS artifact serves both architectures.
  const universal = platforms['darwin-universal'];
  if (universal) {
    put('darwin-aarch64', universal);
    put('darwin-x86_64', universal);
  }

  // 2. Per-installer keys for whatever is already in the manifest.
  for (const [key, value] of Object.entries(manifest.platforms || {})) {
    if (key.split('-').length > 2) continue; // already an installer-scoped key
    const installer = installerOf(fileName(value.url));
    if (!installer) continue;
    // The universal entry is not a real target key, so scope the aliases
    // instead — that is what a running app actually looks up.
    if (key === 'darwin-universal') {
      put(`darwin-aarch64-${installer}`, value);
      put(`darwin-x86_64-${installer}`, value);
    } else {
      put(`${key}-${installer}`, value);
    }
  }

  // 3. Installer formats that were built but never made it into the manifest —
  //    in practice the MSI, since only one Windows entry is recorded. Their
  //    signature has to come from the uploaded `.sig` file.
  for (const asset of assets) {
    const installer = installerOf(asset.name);
    if (!installer || !asset.name.toLowerCase().endsWith('.zip')) continue;
    const key = `windows-x86_64-${installer}`;
    if (platforms[key]) continue;
    const signature = readSig(`${asset.name}.sig`);
    if (!signature) continue;
    put(key, { signature: signature.trim(), url: asset.url });
  }

  return { manifest: { ...manifest, platforms }, added };
}

/* ---------------- self test ---------------- */

function selfTest() {
  const fail = (msg) => {
    console.error(`FAIL: ${msg}`);
    process.exitCode = 1;
  };
  const eq = (a, b, msg) => {
    if (JSON.stringify(a) !== JSON.stringify(b)) fail(`${msg}\n  got:      ${JSON.stringify(a)}\n  expected: ${JSON.stringify(b)}`);
  };

  const nsis = { signature: 'sig-nsis', url: 'https://x/senju-term_0.2.0_x64-setup.nsis.zip' };
  const mac = { signature: 'sig-mac', url: 'https://x/Senju%20Term_0.2.0_universal.app.tar.gz' };
  const linux = { signature: 'sig-linux', url: 'https://x/senju-term_0.2.0_amd64.AppImage' };
  const base = { version: '0.2.0', platforms: { 'windows-x86_64': nsis, 'darwin-universal': mac, 'linux-x86_64': linux } };

  const assets = [{ name: 'senju-term_0.2.0_x64_en-US.msi.zip', url: 'https://x/senju-term_0.2.0_x64_en-US.msi.zip' }];
  const sigs = { 'senju-term_0.2.0_x64_en-US.msi.zip.sig': 'sig-msi\n' };
  const { manifest: out } = expand(base, assets, (n) => sigs[n] ?? null);
  const p = out.platforms;

  // macOS: both real target keys must resolve, since `darwin-universal` never does.
  eq(p['darwin-aarch64'], mac, 'darwin-aarch64 alias');
  eq(p['darwin-x86_64'], mac, 'darwin-x86_64 alias');
  eq(p['darwin-aarch64-app'], mac, 'darwin-aarch64-app alias');

  // Windows: each installer keeps to its own format.
  eq(p['windows-x86_64-nsis'], nsis, 'nsis key');
  eq(p['windows-x86_64-msi'], { signature: 'sig-msi', url: assets[0].url }, 'msi key from asset + sig');
  eq(p['windows-x86_64'], nsis, 'bare key preserved as fallback');
  eq(p['linux-x86_64-appimage'], linux, 'appimage key');

  // An MSI whose .sig was not uploaded must be skipped, not guessed at — a
  // wrong signature would make the update fail verification after downloading.
  const { manifest: noSig } = expand(base, assets, () => null);
  if (noSig.platforms['windows-x86_64-msi']) fail('msi key added without a signature');

  // Explicit entries win over generated ones.
  const explicit = { ...base, platforms: { ...base.platforms, 'windows-x86_64-nsis': { signature: 's', url: 'https://x/other.nsis.zip' } } };
  eq(expand(explicit).manifest.platforms['windows-x86_64-nsis'].url, 'https://x/other.nsis.zip', 'explicit key preserved');

  // Idempotent: expanding twice changes nothing.
  const once = expand(base, assets, (n) => sigs[n] ?? null).manifest;
  const twice = expand(once, assets, (n) => sigs[n] ?? null);
  eq(twice.added, [], 'second pass adds nothing');

  if (!process.exitCode) console.log('expand-update-manifest self-test: OK');
}

/* ---------------- cli ---------------- */

function arg(name) {
  const i = process.argv.indexOf(`--${name}`);
  return i === -1 ? null : process.argv[i + 1];
}

if (process.argv.includes('--self-test')) {
  selfTest();
} else {
  const manifestPath = arg('manifest');
  if (!manifestPath) {
    console.error('usage: expand-update-manifest.mjs --manifest latest.json [--assets a.json] [--sig-dir d] [--out o.json]');
    process.exit(2);
  }
  const assetsPath = arg('assets');
  const sigDir = arg('sig-dir');
  const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));
  const assets = assetsPath ? (JSON.parse(readFileSync(assetsPath, 'utf8')).assets ?? JSON.parse(readFileSync(assetsPath, 'utf8'))) : [];
  const readSig = (name) => {
    if (!sigDir) return null;
    const p = join(sigDir, name);
    return existsSync(p) ? readFileSync(p, 'utf8') : null;
  };
  const { manifest: out, added } = expand(manifest, assets, readSig);
  writeFileSync(arg('out') || manifestPath, `${JSON.stringify(out, null, 2)}\n`);
  console.log(added.length ? `added platform keys: ${added.join(', ')}` : 'no new platform keys');
}
