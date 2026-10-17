/* Terminal-pipeline benchmark for the Senju Term frontend.
 *
 * Measures the cost of the path a PTY byte actually travels once it reaches the
 * webview: IPC payload decode -> Terminal.write() -> render. It drives the real
 * vendored xterm.js and addons from ui/vendor/, so the numbers move when those
 * files or the decode strategy change.
 *
 * Not a test and not a CI gate -- headless Chromium renders WebGL through
 * SwiftShader (software), so renderer timings are only comparable between runs
 * of this harness on the same machine, never against a real GPU.
 *
 *   node scripts/term_bench.mjs                 # full matrix, JSON to stdout
 *   node scripts/term_bench.mjs --out a.json    # also write the raw results
 *   node scripts/term_bench.mjs --mb 8 --reps 5
 *   node scripts/term_bench.mjs --contexts      # WebGL-context survival probe
 */
import { createRequire } from 'module';
import { fileURLToPath } from 'url';
import http from 'node:http';
import path from 'path';
import fs from 'fs';

const require = createRequire('/opt/node22/lib/node_modules/');
const { chromium } = require('playwright');

const here = path.dirname(fileURLToPath(import.meta.url));
const uiDir = path.resolve(here, '..', 'ui');

const argv = process.argv.slice(2);
const flag = (name, def) => {
  const i = argv.indexOf(`--${name}`);
  return i >= 0 && argv[i + 1] && !argv[i + 1].startsWith('--') ? argv[i + 1] : def;
};
const has = (name) => argv.includes(`--${name}`);

const MB = Number(flag('mb', 6));
const REPS = Number(flag('reps', 3));
// The first rep is discarded as warm-up, so one rep leaves nothing to take a
// median of — which used to surface as a full matrix of NaN rather than a
// complaint. `--contexts` does not run reps at all.
if (!has('contexts') && !(REPS >= 2)) {
  console.error(`--reps must be at least 2 (got ${flag('reps', 3)}): the first rep warms the JIT and the glyph atlas and is discarded.`);
  process.exit(2);
}
const LABEL = flag('label', 'run');
const OUT = flag('out', '');

/* ---------------- workloads ----------------
 * Byte payloads shaped like the output that actually stresses a terminal.
 * Generated deterministically so every checkpoint feeds identical bytes. */

function rng(seed) {
  let s = seed >>> 0;
  return () => ((s = (s * 1664525 + 1013904223) >>> 0) / 4294967296);
}

function makePayload(kind, bytes) {
  const r = rng(0x5e17a);
  const out = [];
  let size = 0;
  const push = (s) => { const b = Buffer.from(s, 'utf8'); out.push(b); size += b.length; };
  const words = ['Compiling', 'senju-core', 'v0.1.0', 'warning:', 'unused', 'variable', 'src/lib.rs', 'note:', 'expected', 'found', 'Finished', 'target(s)'];
  const jp = ['接続しました', 'ビルドを開始します', '警告: 未使用の変数', 'テストが完了しました', 'ファイルを書き出しています', 'セッションを再開'];
  while (size < bytes) {
    switch (kind) {
      case 'plain': {
        let line = '';
        const n = 4 + Math.floor(r() * 8);
        for (let i = 0; i < n; i++) line += words[Math.floor(r() * words.length)] + ' ';
        push(line.trimEnd() + '\r\n');
        break;
      }
      case 'ansi': {
        let line = '';
        const n = 4 + Math.floor(r() * 8);
        for (let i = 0; i < n; i++) {
          const fg = 30 + Math.floor(r() * 8);
          line += `\x1b[1;${fg}m` + words[Math.floor(r() * words.length)] + '\x1b[0m ';
        }
        push(line.trimEnd() + '\r\n');
        break;
      }
      case 'cjk': {
        let line = '';
        const n = 2 + Math.floor(r() * 4);
        for (let i = 0; i < n; i++) line += jp[Math.floor(r() * jp.length)] + ' ';
        push(line.trimEnd() + '\r\n');
        break;
      }
      case 'firehose': {
        // `cat` of a big file: very long lines, no colour, forces reflow-free
        // fast-path parsing plus heavy scrolling.
        let line = '';
        while (line.length < 2000) line += words[Math.floor(r() * words.length)] + ' ';
        push(line + '\r\n');
        break;
      }
      default: throw new Error(`unknown workload ${kind}`);
    }
  }
  return Buffer.concat(out).subarray(0, bytes);
}

const WORKLOADS = ['plain', 'ansi', 'cjk', 'firehose'];
const CHUNK = 64 * 1024; // matches FLUSH_THRESHOLD in src-tauri/src/lib.rs

/* ---------------- static server for ui/ ----------------
 * A file:// page cannot fetch the vendor bundles the way the app does, and the
 * bench page must not be written into the source tree, so serve ui/ from
 * memory + disk on loopback. node:http only, same rule as apps/web-lp. */

const BENCH_HTML = `<!doctype html>
<html><head><meta charset="utf-8" />
<link rel="stylesheet" href="vendor/xterm.css" />
<style>html,body{margin:0;background:#000}#host{width:1200px;height:640px}</style>
<script src="vendor/xterm.js"></script>
<script src="vendor/addon-fit.js"></script>
<script src="vendor/addon-webgl.js"></script>
</head><body><div id="host"></div></body></html>`;

function serveUi() {
  return new Promise((resolve) => {
    const srv = http.createServer((req, res) => {
      const url = new URL(req.url, 'http://x');
      if (url.pathname === '/_bench.html') {
        res.writeHead(200, { 'content-type': 'text/html; charset=utf-8' });
        res.end(BENCH_HTML);
        return;
      }
      const p = path.join(uiDir, path.normalize(url.pathname).replace(/^(\.\.[/\\])+/, ''));
      if (!p.startsWith(uiDir) || !fs.existsSync(p) || fs.statSync(p).isDirectory()) {
        res.writeHead(404); res.end('not found'); return;
      }
      const ext = path.extname(p);
      const type = { '.js': 'text/javascript', '.css': 'text/css', '.woff2': 'font/woff2', '.png': 'image/png' }[ext] || 'application/octet-stream';
      res.writeHead(200, { 'content-type': type });
      fs.createReadStream(p).pipe(res);
    });
    srv.listen(0, '127.0.0.1', () => resolve(srv));
  });
}

/* ---------------- in-page driver ----------------
 * Runs inside Chromium. Mirrors createThread()'s Terminal options and the
 * session:data handler's decode, then feeds one 64KB chunk per macrotask --
 * the shape the Rust flusher actually delivers during heavy output. */

const DRIVER = () => {
  // Byte-for-byte the decode in ui/app.js (b64ToBytes).
  window.__b64ToBytes = (b64) => {
    const bin = atob(b64);
    const bytes = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    return bytes;
  };

  window.__makeTerm = (webgl) => {
    // Disposing matters more than it looks: a leaked Terminal keeps its render
    // loop, buffers and (with the addon) a WebGL context alive, so successive
    // runs would measure a browser that is progressively more loaded rather
    // than the change under test.
    if (window.__term) { try { window.__term.dispose(); } catch (_e) {} window.__term = null; }
    const host = document.getElementById('host');
    host.innerHTML = '';
    const term = new Terminal({
      fontSize: 14,
      fontFamily: 'monospace',
      cursorBlink: true,
      allowProposedApi: true,
      scrollback: 10000,
    });
    const fit = new FitAddon.FitAddon();
    term.loadAddon(fit);
    term.open(host);
    fit.fit();
    let gl = null;
    if (webgl && typeof WebglAddon !== 'undefined') {
      try {
        gl = new WebglAddon.WebglAddon();
        gl.onContextLoss(() => { gl.dispose(); gl = null; });
        term.loadAddon(gl);
      } catch (_e) { gl = null; }
    }
    window.__term = term;
    return { cols: term.cols, rows: term.rows, webgl: !!gl };
  };

  // Zero-delay macrotasks: setTimeout clamps to 4ms after nesting depth 5,
  // which would dwarf the numbers we are measuring.
  const mc = new MessageChannel();
  const queue = [];
  mc.port1.onmessage = () => { const fn = queue.shift(); if (fn) fn(); };
  const soon = (fn) => { queue.push(fn); mc.port2.postMessage(0); };

  // Chunks cross the CDP bridge once, as base64 strings (cheap to serialise).
  // For raw mode they are decoded here, outside the timed region, so the run
  // measures only what changes when the IPC stops base64-encoding.
  window.__prepare = (b64Chunks, mode) => {
    window.__chunks = mode === 'b64' ? b64Chunks : b64Chunks.map((c) => window.__b64ToBytes(c));
    return window.__chunks.length;
  };

  window.__run = (mode) => new Promise((resolve) => {
    const term = window.__term;
    const chunks = window.__chunks;
    let decodeMs = 0, writeMs = 0, i = 0;
    const frames = [];
    let last = performance.now();
    let raf = requestAnimationFrame(function tick(t) {
      frames.push(t - last); last = t; raf = requestAnimationFrame(tick);
    });
    const t0 = performance.now();
    const step = () => {
      if (i >= chunks.length) return;
      const c = chunks[i++];
      const d0 = performance.now();
      const bytes = mode === 'b64' ? window.__b64ToBytes(c) : c;
      const d1 = performance.now();
      decodeMs += d1 - d0;
      const done = i >= chunks.length;
      term.write(bytes, done ? () => {
        const t1 = performance.now();
        requestAnimationFrame(() => requestAnimationFrame(() => {
          const t2 = performance.now();
          cancelAnimationFrame(raf);
          const sorted = frames.slice(1).sort((a, b) => a - b);
          resolve({
            totalMs: t1 - t0,
            renderedMs: t2 - t0,
            decodeMs,
            writeMs,
            frames: sorted.length,
            frameP95: sorted.length ? sorted[Math.floor(sorted.length * 0.95)] : 0,
            frameMax: sorted.length ? sorted[sorted.length - 1] : 0,
            jank50: sorted.filter((f) => f > 50).length,
          });
        }));
      } : undefined);
      writeMs += performance.now() - d1;
      soon(step);
    };
    soon(step);
  });

  // How many live WebGL contexts a browser actually grants. xterm's WebGL
  // addon takes one per Terminal, and the app loads it per thread, so this is
  // the ceiling on how many threads can keep GPU rendering at once.
  window.__glCap = (n) => {
    const kept = [];
    for (let i = 0; i < n; i++) {
      const c = document.createElement('canvas');
      c.width = c.height = 256;
      const gl = c.getContext('webgl2', { preserveDrawingBuffer: false });
      kept.push(gl);
    }
    return new Promise((resolve) => setTimeout(() => {
      const alive = kept.filter((g) => g && !g.isContextLost()).length;
      const nulls = kept.filter((g) => !g).length;
      // Drop them so the next probe starts clean.
      kept.forEach((g) => { try { g && g.getExtension('WEBGL_lose_context')?.loseContext(); } catch (_e) {} });
      resolve({ requested: n, alive, refused: nulls, lost: n - alive - nulls });
    }, 300));
  };

};

/* ---------------- harness ---------------- */

const median = (xs) => {
  const s = [...xs].sort((a, b) => a - b);
  return s.length % 2 ? s[(s.length - 1) / 2] : (s[s.length / 2 - 1] + s[s.length / 2]) / 2;
};

async function main() {
  const srv = await serveUi();
  const port = srv.address().port;
  const browser = await chromium.launch();
  const page = await browser.newPage({ viewport: { width: 1280, height: 720 } });
  await page.addInitScript(DRIVER);
  await page.goto(`http://127.0.0.1:${port}/_bench.html`);
  await page.waitForFunction(() => typeof Terminal !== 'undefined');

  const vendorSizes = Object.fromEntries(
    ['xterm.js', 'addon-webgl.js', 'addon-fit.js', 'addon-search.js', 'addon-web-links.js']
      .map((f) => [f, fs.statSync(path.join(uiDir, 'vendor', f)).size]));

  const results = { label: LABEL, mb: MB, reps: REPS, vendorSizes, runs: [], contexts: null };

  if (has('contexts')) {
    results.contexts = {};
    for (const n of [1, 2, 4, 8, 12, 16, 24, 32]) {
      try {
        results.contexts[n] = await page.evaluate((k) => window.__glCap(k), n);
      } catch (e) {
        results.contexts[n] = { requested: n, crashed: String(e.message).slice(0, 80) };
        await page.goto(`http://127.0.0.1:${port}/_bench.html`);
        await page.waitForFunction(() => typeof Terminal !== 'undefined');
      }
      process.stderr.write(`  glCap n=${n} -> ${JSON.stringify(results.contexts[n])}\n`);
    }
  } else {
    const bytes = MB * 1024 * 1024;
    for (const workload of WORKLOADS) {
      const payload = makePayload(workload, bytes);
      const rawChunks = [];
      for (let o = 0; o < payload.length; o += CHUNK) rawChunks.push(payload.subarray(o, o + CHUNK));
      const b64Chunks = rawChunks.map((c) => c.toString('base64'));
      const b64Bytes = b64Chunks.reduce((a, s) => a + s.length, 0);

      for (const webgl of [true, false]) {
        for (const mode of ['b64', 'raw']) {
          // A fresh page per config: the only reliable way to keep GPU/GC state
          // from one configuration leaking into the next one's numbers.
          await page.goto(`http://127.0.0.1:${port}/_bench.html`);
          await page.waitForFunction(() => typeof Terminal !== 'undefined');
          await page.evaluate(([c, m]) => window.__prepare(c, m), [b64Chunks, mode]);
          const reps = [];
          for (let rep = 0; rep < REPS; rep++) {
            await page.evaluate((w) => window.__makeTerm(w), webgl);
            const r = await page.evaluate((m) => window.__run(m), mode);
            if (rep > 0) reps.push(r); // first rep warms JIT and the glyph atlas
          }
          const pick = (k) => median(reps.map((r) => r[k]));
          const totals = reps.map((r) => r.totalMs);
          const spread = ((Math.max(...totals) - Math.min(...totals)) / median(totals)) * 100;
          results.runs.push({
            workload, webgl, mode,
            payloadBytes: payload.length,
            wireBytes: mode === 'b64' ? b64Bytes : payload.length,
            totalMs: +pick('totalMs').toFixed(1),
            renderedMs: +pick('renderedMs').toFixed(1),
            decodeMs: +pick('decodeMs').toFixed(1),
            writeMs: +pick('writeMs').toFixed(1),
            frameP95: +pick('frameP95').toFixed(1),
            frameMax: +pick('frameMax').toFixed(1),
            jank50: pick('jank50'),
            mbPerSec: +((payload.length / 1048576) / (pick('totalMs') / 1000)).toFixed(1),
            spreadPct: +spread.toFixed(1),
          });
          process.stderr.write(`  ${workload.padEnd(9)} webgl=${String(webgl).padEnd(5)} ${mode.padEnd(3)} -> ${results.runs.at(-1).totalMs}ms (${results.runs.at(-1).mbPerSec} MB/s, +-${results.runs.at(-1).spreadPct}%)\n`);
        }
      }
    }
  }

  await browser.close();
  srv.close();
  const json = JSON.stringify(results, null, 2);
  if (OUT) fs.writeFileSync(OUT, json);
  process.stdout.write(json + '\n');
}

main().catch((e) => { console.error(e); process.exit(1); });
