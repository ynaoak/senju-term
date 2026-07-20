/* Captures real app screenshots for the web-lp landing page.
 *
 * Distinct from ui_shot.mjs (a design-iteration aid): this produces the
 * final marketing assets checked into ../../web-lp/assets/. Loads the real
 * ui/index.html in headless Chromium with a stub __TAURI__ backend (no
 * Rust needed), captures a few curated views, then the caller (see the
 * `npm`-free `convert -trim` step in the accompanying shell command) crops
 * each to its content — the app's dark canvas trims cleanly since the empty
 * space below the content is a single flat color.
 *
 * Usage: node scripts/lp_shots.mjs [outDir]
 */
import { createRequire } from 'module';
import { fileURLToPath, pathToFileURL } from 'url';
import path from 'path';
import fs from 'fs';

const require = createRequire('/opt/node22/lib/node_modules/');
const { chromium } = require('playwright');

const here = path.dirname(fileURLToPath(import.meta.url));
const uiDir = path.resolve(here, '..', 'ui');
const outDir = path.resolve(process.argv[2] || path.join(here, '..', '..', 'web-lp', 'assets'));
fs.mkdirSync(outDir, { recursive: true });

const STUB = () => {
  const uid = () => 'id-' + Math.random().toString(36).slice(2, 9);
  const store = {
    settings: { font_size: 14, shell: '', default_profile_id: 'p1', font_family: '', scrollback: 10000, theme: 'dark', shell_integration: true },
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
    ],
  };

  let sess = 0;
  const listeners = {};
  const emit = (name, payload) => (listeners[name] || []).forEach((cb) => cb({ payload }));
  const b64 = (s) => btoa(String.fromCharCode(...new TextEncoder().encode(s)));

  window.__TAURI__ = {
    core: {
      invoke: async (cmd, args) => {
        switch (cmd) {
          case 'get_settings': return store.settings;
          case 'save_settings': store.settings = args.settings; return null;
          case 'list_workflows': return store.workflows;
          case 'list_ssh_hosts': return store.hosts;
          case 'list_profiles': return store.profiles;
          case 'workflow_placeholders': return [];
          case 'fill_workflow': return args.command;
          case 'create_local_session': {
            const id = 'sess-' + (++sess);
            const title = ['bash', 'node'][sess - 1] || 'bash';
            const A = '\x1b]133;A\x07', B = '\x1b]133;B\x07', C = '\x1b]133;C\x07';
            const D = (code) => `\x1b]133;D;${code}\x07`;
            const prompt = `\x1b[38;5;44m➜  \x1b[38;5;39m~/senju-term\x1b[0m \x1b[38;5;245m(main)\x1b[0m $ `;
            setTimeout(() => {
              emit('session:data', { id, data: b64(
                `\x1b[38;5;44mSenju Term\x1b[0m v0.1.0 — Rust + Tauri 2\r\n\r\n` +
                `${A}${prompt}${B}cargo test -p senju-core\r\n` +
                `${C}running 31 tests\r\ntest result: \x1b[32mok\x1b[0m. 31 passed; 0 failed\r\n${D(0)}` +
                `${A}${prompt}${B}git push origin main\r\n` +
                `${C}remote: Permission to ynaoak/senju-term.git denied\r\nfatal: unable to access repository\r\n${D(128)}` +
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

const browser = await chromium.launch({ executablePath: '/opt/pw-browsers/chromium-1194/chrome-linux/chrome' });
const ctx = await browser.newContext({ viewport: { width: 1280, height: 820 }, deviceScaleFactor: 2 });
const page = await ctx.newPage();
await page.addInitScript(STUB);
page.on('console', (m) => { if (m.type() === 'error') console.log('PAGE ERROR:', m.text()); });
await page.goto(pathToFileURL(path.join(uiDir, 'index.html')).href);
await page.waitForTimeout(500);

// A second thread for the sidebar's visual variety, driven entirely through
// real UI interactions (click the pin button, click the row) rather than
// touching `state` directly — app.js's `const state` isn't exposed on
// `window` (only its top-level `function` declarations are), and going
// through the actual click handlers exercises the same code a user would.
// boot() already created thread #1 (with the transcript above); one more
// gives the sidebar some depth without repeating the same label three times.
await page.waitForTimeout(150); // let thread #1's welcome/build output land
await page.evaluate(() => window.newLocalThread());
await page.waitForTimeout(150);
// Pin the first (still creation-ordered first, since no pins exist yet).
await page.click('#thread-list .thread-item:nth-child(1) .pin');
// Then re-focus its pane so the colorful transcript is what's on screen.
await page.click('#thread-list .thread-item:nth-child(1) .title');
await page.waitForTimeout(200);
await page.evaluate(() => window.setView('shell'));
await page.waitForTimeout(300);
await page.screenshot({ path: path.join(outDir, 'hero.png') });

await page.evaluate(() => window.setView('workflows'));
await page.waitForTimeout(250);
await page.screenshot({ path: path.join(outDir, 'workflows.png') });

await page.evaluate(() => window.setView('hosts'));
await page.waitForTimeout(250);
await page.screenshot({ path: path.join(outDir, 'hosts.png') });

await browser.close();
console.log('lp shots ->', outDir);
