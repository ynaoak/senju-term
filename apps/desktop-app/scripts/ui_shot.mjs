/* Headless screenshot harness for the Senju Term frontend.
 *
 * Loads ui/index.html in Chromium with a stub `window.__TAURI__` so the real
 * app.js boots without a Rust backend, then captures PNGs of each view. This
 * is a design aid (not a test): it lets us iterate on the UI visually.
 *
 * Usage: node scripts/ui_shot.mjs [outDir]
 */
import { createRequire } from 'module';
import { fileURLToPath, pathToFileURL } from 'url';
import path from 'path';
import fs from 'fs';

const require = createRequire('/opt/node22/lib/node_modules/');
const { chromium } = require('playwright');

const here = path.dirname(fileURLToPath(import.meta.url));
const uiDir = path.resolve(here, '..', 'ui');
const outDir = path.resolve(process.argv[2] || path.join(here, '..', '..', '..', 'scratch-shots'));
fs.mkdirSync(outDir, { recursive: true });

// A minimal in-page fake backend: enough state for the UI to render richly.
const STUB = () => {
  const uid = () => 'id-' + Math.random().toString(36).slice(2, 9);
  const store = {
    settings: { font_size: 14, shell: '', default_profile_id: '', font_family: '', scrollback: 10000 },
    workflows: [
      { id: uid(), name: 'デプロイ (staging)', description: 'ステージング環境へデプロイ', command: 'deploy {{env:staging}}', tags: ['ops', 'deploy'], shortcut: 'ctrl+shift+g', show_button: true },
      { id: uid(), name: 'ログ追尾', description: 'アプリログを tail', command: 'tail -f /var/log/app.log', tags: ['debug'], shortcut: '', show_button: true },
      { id: uid(), name: 'DB バックアップ', description: '', command: 'pg_dump {{db}} > backup.sql', tags: ['ops'], shortcut: '', show_button: false },
      { id: uid(), name: 'git 同期', description: 'fetch & rebase', command: 'git fetch && git rebase origin/main', tags: ['git'], shortcut: 'alt+g', show_button: false },
    ],
    hosts: [
      { id: uid(), name: 'prod-web-01', host: '10.0.1.11', port: 22, username: 'deploy', auth_method: 'key', key_path: '~/.ssh/id_ed25519' },
      { id: uid(), name: 'bastion', host: 'bastion.example.com', port: 2222, username: 'admin', auth_method: 'keypassword', key_path: '~/.ssh/id_rsa' },
      { id: uid(), name: '', host: 'db.internal', port: 22, username: 'postgres', auth_method: 'password', key_path: '' },
    ],
    profiles: [
      { id: 'p1', name: 'zsh', command: '/bin/zsh', args: [], cwd: '' },
      { id: 'p2', name: 'bash', command: '/bin/bash', args: ['-l'], cwd: '~/work' },
      { id: 'p3', name: 'Python REPL', command: 'python3', args: [], cwd: '' },
    ],
  };
  store.settings.default_profile_id = 'p1';

  let sess = 0;
  const listeners = {};
  const emit = (name, payload) => (listeners[name] || []).forEach((cb) => cb({ payload }));
  // btoa() alone throws on non-Latin1 (➜, Japanese); encode UTF-8 first, the
  // same byte-oriented base64 the Rust backend sends.
  const b64 = (s) => btoa(String.fromCharCode(...new TextEncoder().encode(s)));

  window.__TAURI__ = {
    core: {
      invoke: async (cmd, args) => {
        switch (cmd) {
          case 'get_settings':
            return { ...store.settings, theme: window.__LIGHT__ ? 'light' : 'dark' };
          case 'save_settings': store.settings = args.settings; return null;
          case 'list_workflows': return window.__EMPTY__ ? [] : store.workflows;
          case 'list_ssh_hosts': return store.hosts;
          case 'list_profiles': return store.profiles;
          case 'workflow_placeholders': return [];
          case 'fill_workflow': return args.command;
          case 'create_local_session': {
            const id = 'sess-' + (++sess);
            const title = ['zsh', 'bash', 'node', 'vim ~/notes.md'][sess % 4] || 'zsh';
            // Emit OSC 133-framed sample output so command-block chips render.
            const A = '\x1b]133;A\x07', B = '\x1b]133;B\x07', C = '\x1b]133;C\x07';
            const D = (code) => `\x1b]133;D;${code}\x07`;
            const prompt = `\x1b[38;5;44m➜  \x1b[38;5;39m~/senju-term\x1b[0m \x1b[38;5;245m(main)\x1b[0m $ `;
            setTimeout(() => {
              emit('session:data', { id, data: b64(
                `\x1b[38;5;44mSenju Term\x1b[0m — welcome\r\n` +
                `\x1b[38;5;245mLast login: Wed Jul 17\x1b[0m\r\n` +
                `${A}${prompt}${B}cargo test -p senju-core\r\n` +
                `${C}running 26 tests\r\ntest result: \x1b[32mok\x1b[0m. 26 passed; 0 failed\r\n${D(0)}` +
                `${A}${prompt}${B}cat missing.txt\r\n` +
                `${C}cat: missing.txt: No such file or directory\r\n${D(1)}` +
                `${A}${prompt}`) });
            }, 30);
            return { id, title, kind: 'local' };
          }
          case 'session_write': case 'session_resize': case 'session_kill': return null;
          case 'save_workflow': case 'delete_workflow': return null;
          case 'save_ssh_host': case 'delete_ssh_host': return null;
          case 'save_profile': case 'delete_profile': return null;
          case 'open_external': return null;
          default: return null;
        }
      },
    },
    event: { listen: async (name, cb) => { (listeners[name] ||= []).push(cb); return () => {}; } },
    window: {
      getCurrentWindow: () => ({
        isMaximized: async () => false,
        minimize: async () => {}, toggleMaximize: async () => {}, close: async () => {},
        onResized: () => {},
      }),
    },
  };
};

const VIEWS = ['shell', 'workflows', 'hosts', 'profiles', 'settings'];

const browser = await chromium.launch({ executablePath: '/opt/pw-browsers/chromium-1194/chrome-linux/chrome' });
const ctx = await browser.newContext({ viewport: { width: 1280, height: 820 }, deviceScaleFactor: 2 });
const page = await ctx.newPage();
await page.addInitScript(STUB);
page.on('console', (m) => { if (m.type() === 'error') console.log('PAGE ERROR:', m.text()); });
await page.goto(pathToFileURL(path.join(uiDir, 'index.html')).href);
await page.waitForTimeout(500);

for (const v of VIEWS) {
  await page.evaluate((name) => window.setView && window.setView(name), v);
  await page.waitForTimeout(250);
  await page.screenshot({ path: path.join(outDir, `${v}.png`) });
}

// Split view
await page.evaluate(() => window.setView && window.setView('shell'));
await page.evaluate(() => window.toggleSplit && window.toggleSplit());
await page.waitForTimeout(300);
await page.screenshot({ path: path.join(outDir, 'shell-split.png') });

// A modal (workflow editor)
await page.evaluate(() => window.editWorkflow && window.editWorkflow(null));
await page.waitForTimeout(200);
await page.screenshot({ path: path.join(outDir, 'modal-workflow.png') });
await page.keyboard.press('Escape');

// Command palette
await page.evaluate(() => window.openPalette && window.openPalette());
await page.waitForTimeout(150);
await page.evaluate(() => { const i = document.querySelector('#palette-input'); if (i) { i.value = 's'; i.dispatchEvent(new Event('input')); } });
await page.waitForTimeout(150);
await page.screenshot({ path: path.join(outDir, 'palette.png') });
await page.keyboard.press('Escape');

// SSH host editor modal (sectioned form).
await page.evaluate(() => window.editHost && window.editHost(null));
await page.waitForTimeout(200);
await page.screenshot({ path: path.join(outDir, 'modal-host.png') });
await page.keyboard.press('Escape');

await page.keyboard.press('Escape');

// Keyboard shortcut cheat sheet.
await page.evaluate(() => window.openShortcuts && window.openShortcuts());
await page.waitForTimeout(200);
await page.screenshot({ path: path.join(outDir, 'shortcuts.png') });
await page.keyboard.press('Escape');

// Empty state (search with no match) + a toast.
await page.evaluate(() => {
  window.setView('hosts');
  const i = document.querySelector('#host-search');
  i.value = 'zzzznomatch';
  i.dispatchEvent(new Event('input'));
  window.toast('設定を保存しました');
});
await page.waitForTimeout(300);
await page.screenshot({ path: path.join(outDir, 'empty-and-toast.png') });

// Light theme: shell + workflows on a fresh page with theme=light settings.
const lightPage = await ctx.newPage();
await lightPage.addInitScript(STUB);
await lightPage.addInitScript(() => { window.__LIGHT__ = true; });
await lightPage.goto(pathToFileURL(path.join(uiDir, 'index.html')).href);
await lightPage.waitForTimeout(500);
await lightPage.screenshot({ path: path.join(outDir, 'light-shell.png') });
await lightPage.evaluate(() => window.setView('workflows'));
await lightPage.waitForTimeout(250);
await lightPage.screenshot({ path: path.join(outDir, 'light-workflows.png') });
await lightPage.close();

// Empty state with CTA: load a second page whose stub starts with no hosts.
const page2 = await ctx.newPage();
await page2.addInitScript(STUB);
await page2.addInitScript(() => { window.__EMPTY__ = true; });
await page2.goto(pathToFileURL(path.join(uiDir, 'index.html')).href);
await page2.waitForTimeout(400);
await page2.evaluate(() => { window.setView('workflows'); });
await page2.waitForTimeout(200);
await page2.screenshot({ path: path.join(outDir, 'empty-cta.png') });

await browser.close();
console.log('shots ->', outDir);
