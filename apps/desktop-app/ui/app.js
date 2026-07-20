/* Senju Term frontend.
 *
 * Terminal sessions ("threads") live independently of where they are shown:
 * the left sidebar lists every thread, the center area holds one or two
 * stacked panes, and each pane chooses which thread it displays — Warp-style
 * session management. Workflows (custom commands), SSH hosts and settings
 * live in the right sidebar. Talks to the Rust backend exclusively through
 * Tauri invoke/events. */
'use strict';

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const appWindow = window.__TAURI__.window.getCurrentWindow();

const $ = (sel) => document.querySelector(sel);

const state = {
  threads: [],         // { id, title, kind, pinned, term, fit, hostEl, search, customTitle, activity }
  panes: [],           // { root, head, select, kindEl, closeBtn, body, threadId }
  focusedPane: 0,
  workflows: [],
  hosts: [],
  profiles: [],
  settings: { font_size: 14, shell: '', default_profile_id: '', font_family: '', scrollback: 10000, theme: 'dark', shell_integration: true },
  renaming: null,      // thread id currently being renamed inline in the sidebar
};

/* ---------------- terminal threads ---------------- */

const TERM_THEMES = {
  dark: {
    background: '#0d1117',
    foreground: '#d7dde5',
    cursor: '#00e5be',
    cursorAccent: '#0d1117',
    selectionBackground: 'rgba(0, 229, 190, 0.25)',
    black: '#161b22', red: '#e5534b', green: '#3fb950', yellow: '#d29922',
    blue: '#58a6ff', magenta: '#b393f0', cyan: '#39c5cf', white: '#d7dde5',
    brightBlack: '#6e7681', brightRed: '#ff7b72', brightGreen: '#56d364',
    brightYellow: '#e3b341', brightBlue: '#79c0ff', brightMagenta: '#d2a8ff',
    brightCyan: '#56d4dd', brightWhite: '#f0f6fc',
  },
  // GitHub-Light-inspired ANSI ramp so colored CLI output stays readable on
  // a white canvas.
  light: {
    background: '#ffffff',
    foreground: '#24292f',
    cursor: '#0d8f79',
    cursorAccent: '#ffffff',
    selectionBackground: 'rgba(13, 143, 121, 0.22)',
    black: '#24292f', red: '#cf222e', green: '#116329', yellow: '#4d2d00',
    blue: '#0969da', magenta: '#8250df', cyan: '#1b7c83', white: '#6e7781',
    brightBlack: '#57606a', brightRed: '#a40e26', brightGreen: '#1a7f37',
    brightYellow: '#633c01', brightBlue: '#218bff', brightMagenta: '#a475f9',
    brightCyan: '#3192aa', brightWhite: '#8c959f',
  },
};

function currentTermTheme() {
  return TERM_THEMES[state.settings.theme] || TERM_THEMES.dark;
}

/** Applies the configured UI theme to the chrome and every live terminal. */
function applyTheme() {
  document.body.classList.toggle('theme-light', state.settings.theme === 'light');
  for (const t of state.threads) t.term.options.theme = currentTermTheme();
}

const DEFAULT_FONT_STACK =
  '"Cascadia Code", "JetBrains Mono", Consolas, "Noto Sans Mono CJK JP", monospace';

/** The custom font (if set) goes first so it takes priority, falling back to
 * the built-in stack when it's unavailable. */
function fontFamilyStack() {
  const custom = (state.settings.font_family || '').trim();
  return custom ? `${custom}, ${DEFAULT_FONT_STACK}` : DEFAULT_FONT_STACK;
}

function b64ToBytes(b64) {
  const bin = atob(b64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return bytes;
}

function threadById(id) {
  return state.threads.find((t) => t.id === id);
}

function focusedThread() {
  const pane = state.panes[state.focusedPane];
  return pane ? threadById(pane.threadId) : undefined;
}

function measureCells() {
  const pane = state.panes[state.focusedPane];
  const w = pane?.body.clientWidth || 800;
  const h = pane?.body.clientHeight || 500;
  const px = state.settings.font_size;
  return { cols: Math.max(20, Math.floor(w / (px * 0.62))), rows: Math.max(5, Math.floor(h / (px * 1.4))) };
}

async function newLocalThread({ paneIdx = state.focusedPane, profileId = null } = {}) {
  const { cols, rows } = measureCells();
  // Capture the pane object, not just its index: panes can be added/removed
  // while the backend is starting the shell, which would shift the index.
  const pane = state.panes[paneIdx];
  try {
    const info = await invoke('create_local_session', { profileId, cols, rows });
    const idx = pane ? state.panes.indexOf(pane) : -1;
    createThread(info, idx >= 0 ? idx : state.focusedPane);
  } catch (e) {
    toast(`シェルの起動に失敗: ${e}`, true);
  }
}

async function newSshThread(host) {
  const secrets = await promptSshSecrets(host);
  if (secrets === null) return; // cancelled
  const { cols, rows } = measureCells();
  await connectSsh(host, secrets, cols, rows, null);
}

/** Connects, and on an "unknown host key" (TOFU) rejection, offers to trust
 * and retry.
 *
 * `expectedFingerprint` is `null` on the first attempt. If the backend
 * rejects with `UNKNOWN_HOST_KEY:<type>:<fingerprint>`, the modal shows the
 * user exactly that `fingerprint` string; if they approve it, the SAME
 * string (captured straight out of the regex match below — never
 * re-derived, never trusted from anywhere else) is threaded back into the
 * retry as `expectedFingerprint`. The backend only records/accepts the key
 * on that retry if the key it sees THEN has that exact SHA256 fingerprint,
 * which is what stops a MITM from relaying the real key on the first
 * handshake (so the user approves a genuine fingerprint) and substituting
 * its own key on the second.
 *
 * If the retry's key doesn't match (`FINGERPRINT_MISMATCH:`), this is a
 * fail-closed rejection: warn the user and stop — no further retry loop,
 * so an attacker can't get a second (or third) bite at the fingerprint
 * check. */
async function connectSsh(host, secrets, cols, rows, expectedFingerprint) {
  toast(`${host.name || host.host} へ接続中…`);
  try {
    const info = await invoke('create_ssh_session', {
      hostId: host.id,
      password: secrets.password ?? null,
      passphrase: secrets.passphrase ?? null,
      cols, rows,
      expectedFingerprint,
    });
    createThread(info, state.focusedPane);
    toast('接続しました');
  } catch (e) {
    const msg = String(e);
    if (msg.match(/^FINGERPRINT_MISMATCH:/)) {
      // Fail closed: do NOT retry. Retrying here would give an active MITM
      // repeated chances to slip its key in once the user has already
      // approved one fingerprint.
      toast(
        '承認した鍵と異なる鍵が提示されました。中間者攻撃の可能性があるため接続を中止しました',
        true,
      );
      return;
    }
    const unknown = !expectedFingerprint && msg.match(/^UNKNOWN_HOST_KEY:([^:]+):(.+)$/);
    if (unknown) {
      const [, keyType, fingerprint] = unknown;
      if (await confirmTrustHost(host, keyType, fingerprint)) {
        await connectSsh(host, secrets, cols, rows, fingerprint);
      } else {
        toast('未知のホスト鍵のため接続を中止しました', true);
      }
      return;
    }
    toast(`SSH 接続失敗: ${msg}`, true);
  }
}

/** TOFU prompt shown the first time we connect to a host whose key isn't yet
 * recorded in known_hosts. Approving threads the exact fingerprint the user
 * saw back into the retried connection as `expectedFingerprint`, so the
 * backend only trusts a key that provably matches what was approved. */
function confirmTrustHost(host, keyType, fingerprint) {
  return new Promise((resolve) => {
    openModal({
      title:
        `初回接続です。このホスト鍵を信頼して known_hosts に保存し、接続しますか? ` +
        `[ホスト: ${host.name || host.host}:${host.port} / 鍵種別: ${keyType} / フィンガープリント: ${fingerprint}]`,
      okLabel: '信頼して接続',
      body: [],
      onOk: () => resolve(true),
      onCancel: () => resolve(false),
    });
  });
}

function createThread(info, paneIdx) {
  const hostEl = document.createElement('div');
  hostEl.className = 'term-host';
  $('#thread-parking').appendChild(hostEl);

  const term = new Terminal({
    fontSize: state.settings.font_size,
    fontFamily: fontFamilyStack(),
    theme: currentTermTheme(),
    cursorBlink: true,
    allowProposedApi: true,
    scrollback: state.settings.scrollback,
  });
  const fit = new FitAddon.FitAddon();
  term.loadAddon(fit);
  // Links in (attacker-controlled) terminal output must NOT be opened inside
  // the Tauri webview, which holds window.__TAURI__ / IPC. Route http(s) links
  // to the OS browser via the scheme-restricted `open_external` command; ignore
  // everything else.
  term.loadAddon(new WebLinksAddon.WebLinksAddon((_ev, uri) => {
    if (/^https?:\/\//i.test(uri)) invoke('open_external', { url: uri }).catch(() => {});
  }));
  const search = new SearchAddon.SearchAddon();
  term.loadAddon(search);
  // App-level shortcuts must win over the shell (Ctrl+K etc. stay usable
  // inside the terminal, so app shortcuts all use Ctrl+Shift). Returning
  // false keeps xterm from consuming the event; the actual action runs in
  // the window-level keydown handler the event bubbles up to.
  term.attachCustomKeyEventHandler((ev) => {
    // Let a registered workflow shortcut bubble to the window handler instead
    // of being typed into the shell.
    if (ev.type === 'keydown' && workflowForShortcut(ev)) return false;
    const key = ev.key.toLowerCase();
    // Ctrl+Alt+↑/↓ = command-block navigation; bubble to the window handler.
    if (ev.ctrlKey && ev.altKey && (key === 'arrowup' || key === 'arrowdown')) return false;
    const mod = (ev.ctrlKey || ev.metaKey) && ev.shiftKey;
    if (!mod) return true;
    // Ctrl+Shift+C only leaves the terminal (for the app to copy) when there
    // is a selection; with nothing selected it's commonly a shell shortcut
    // (e.g. SIGINT-like copy fallback), so let it through untouched.
    if (key === 'c') return !term.hasSelection();
    return !['p', 't', 'w', 'd', 'v', 'f', 'arrowup', 'arrowdown', '/', '?'].includes(key);
  });
  term.open(hostEl);

  // --- Command blocks (Warp-style), driven by OSC 133 shell integration ---
  // A = prompt start, B = command start, C = output start, D[;exit] = done.
  // Shells emit these via small rc snippets (see README). Threads whose shell
  // doesn't emit them simply have no blocks — everything else works as-is.
  const blocks = [];
  const openBlock = () => blocks.length && !blocks[blocks.length - 1].done
    ? blocks[blocks.length - 1] : null;
  term.parser.registerOscHandler(133, (data) => {
    const [kind, arg] = data.split(';');
    if (kind === 'A') {
      // New prompt: drop disposed markers (scrolled out of scrollback), close
      // any dangling block, then open a fresh one at the prompt line.
      for (let i = blocks.length - 1; i >= 0; i--) {
        if (blocks[i].marker.isDisposed) blocks.splice(i, 1);
      }
      const open = openBlock();
      if (open) open.done = true;
      const marker = term.registerMarker(0);
      if (marker) {
        blocks.push({
          marker, bodyMarker: null, sawOutput: false, done: false,
          collapsed: false, foldDeco: null,
        });
      }
    } else if (kind === 'C') {
      const b = openBlock();
      if (b) {
        b.sawOutput = true;
        // First byte of output/command-echo: the fold boundary (command line
        // itself stays visible; only what follows is ever collapsed).
        if (!b.bodyMarker) b.bodyMarker = term.registerMarker(0);
      }
    } else if (kind === 'D') {
      const b = openBlock();
      if (b) {
        b.done = true;
        // Chip only for blocks that actually ran a command (C seen) — an
        // empty Enter or the shell's startup D would otherwise add noise.
        if (b.sawOutput && !b.marker.isDisposed) {
          const exit = arg !== undefined ? parseInt(arg, 10) : NaN;
          renderBlockToolbar(term, blocks, b, exit);
        }
      }
    }
    return true;
  });

  term.onData((data) => invoke('session_write', { id: info.id, data }).catch(() => {}));
  term.onResize(({ cols, rows }) =>
    invoke('session_resize', { id: info.id, cols, rows }).catch(() => {}));

  const thread = {
    id: info.id, title: info.title, kind: info.kind, pinned: false,
    term, fit, hostEl, search, customTitle: false, activity: false, blocks,
  };
  // OSC-2 title changes (`\e]0;...\a`) rename the thread automatically,
  // unless the user gave it a custom name (task 2).
  term.onTitleChange((title) => {
    if (thread.customTitle || !title) return;
    thread.title = title;
    renderThreads();
  });
  state.threads.push(thread);
  assignThread(paneIdx, info.id);
  // A new thread implies wanting to use it — surface the shell view (a tool
  // panel may be covering the terminal).
  if (currentView !== 'shell') setView('shell');
  renderThreads();
}

async function closeThread(id, { kill = true } = {}) {
  const idx = state.threads.findIndex((t) => t.id === id);
  if (idx < 0) return;
  // Guard pinned threads against accidental user-initiated closes. A natural
  // shell exit (kill:false) always removes the thread without prompting.
  if (kill && state.threads[idx].pinned) {
    if (!(await confirmModal(`固定中の「${state.threads[idx].title}」を終了しますか?`))) return;
  }
  const [thread] = state.threads.splice(idx, 1);
  if (kill) invoke('session_kill', { id }).catch(() => {});
  thread.term.dispose();
  thread.hostEl.remove();
  // Any pane that showed this thread falls back to a thread that is not
  // already displayed elsewhere, or goes empty.
  for (let i = 0; i < state.panes.length; i++) {
    if (state.panes[i].threadId === id) {
      const shown = new Set(state.panes.map((p) => p.threadId));
      const fallback = state.threads.find((t) => !shown.has(t.id));
      setPaneThread(i, fallback ? fallback.id : null);
    }
  }
  renderThreads();
}

/* ---------------- panes (vertical split) ---------------- */

function createPane() {
  const root = document.createElement('div');
  root.className = 'pane';
  // A custom dropdown (not a native <select>) so each row can render the
  // inline Material push_pin SVG, matching the thread list — native <option>
  // elements can only hold text.
  root.innerHTML = `
    <div class="pane-head">
      <div class="pane-thread-dd">
        <button type="button" class="pane-thread-btn" title="このペインに表示するスレッド">
          <span class="ptd-icon"></span>
          <span class="ptd-label"></span>
          <span class="ptd-caret">${icon('chevronDown')}</span>
        </button>
        <ul class="pane-thread-menu hidden" role="listbox"></ul>
      </div>
      <span class="pane-kind"></span>
      <div class="spacer"></div>
      <button class="pane-close icon-btn" title="このペインを閉じる (スレッドは動き続けます)">${icon('x')}</button>
    </div>
    <div class="pane-body"></div>`;
  const pane = {
    root,
    dd: root.querySelector('.pane-thread-dd'),
    ddBtn: root.querySelector('.pane-thread-btn'),
    ddMenu: root.querySelector('.pane-thread-menu'),
    ddIcon: root.querySelector('.ptd-icon'),
    ddLabel: root.querySelector('.ptd-label'),
    kindEl: root.querySelector('.pane-kind'),
    closeBtn: root.querySelector('.pane-close'),
    body: root.querySelector('.pane-body'),
    threadId: null,
  };
  pane.ddBtn.addEventListener('click', (ev) => {
    ev.stopPropagation();
    focusPane(state.panes.indexOf(pane));
    togglePaneMenu(pane);
  });
  pane.ddBtn.addEventListener('keydown', (ev) => {
    if (['ArrowDown', 'Enter', ' '].includes(ev.key)) {
      ev.preventDefault();
      if (pane.ddMenu.classList.contains('hidden')) togglePaneMenu(pane);
    }
  });
  // Interactions inside the dropdown must not bubble to the pane's root
  // mousedown handler below — that fires focusPane() → renderThreads(), which
  // rebuilds the open menu and detaches the row mid-click, swallowing the
  // selection. The dropdown's own handlers call focusPane() explicitly.
  pane.dd.addEventListener('mousedown', (ev) => ev.stopPropagation());
  pane.closeBtn.addEventListener('click', () => removePane(state.panes.indexOf(pane)));
  root.addEventListener('mousedown', () => focusPane(state.panes.indexOf(pane)));
  pane.ro = new ResizeObserver(() => {
    const t = threadById(pane.threadId);
    if (t) t.fit.fit();
  });
  pane.ro.observe(pane.body);
  return pane;
}

/** Closes every pane's thread menu (only one is ever open at a time). */
function closeAllPaneMenus() {
  for (const p of state.panes) {
    p.ddMenu.classList.add('hidden');
    p.dd.classList.remove('open');
  }
  window.removeEventListener('mousedown', paneMenuOutsideClick);
}

function paneMenuOutsideClick(ev) {
  if (!ev.target.closest('.pane-thread-dd')) closeAllPaneMenus();
}

function togglePaneMenu(pane) {
  const wasOpen = !pane.ddMenu.classList.contains('hidden');
  closeAllPaneMenus();
  if (wasOpen) return;
  buildPaneMenu(pane);
  pane.ddMenu.classList.remove('hidden');
  pane.dd.classList.add('open');
  // Focus the current (or first) row so arrow keys work immediately.
  const sel = pane.ddMenu.querySelector('.ptd-item.selected') || pane.ddMenu.querySelector('.ptd-item');
  sel?.focus();
  setTimeout(() => window.addEventListener('mousedown', paneMenuOutsideClick), 0);
}

/** Builds the custom dropdown list for a pane. Each row carries the inline
 * Material push_pin SVG for pinned threads, matching the sidebar. */
function buildPaneMenu(pane) {
  pane.ddMenu.innerHTML = '';
  if (!state.threads.length) {
    const li = document.createElement('li');
    li.className = 'ptd-item empty';
    li.textContent = '(スレッドなし)';
    pane.ddMenu.appendChild(li);
    return;
  }
  for (const t of orderedThreads()) {
    const li = document.createElement('li');
    li.className = 'ptd-item' + (t.id === pane.threadId ? ' selected' : '');
    li.dataset.id = t.id;
    li.tabIndex = 0;
    li.setAttribute('role', 'option');
    li.innerHTML = `
      <span class="ptd-pin">${t.pinned ? pinIcon(true) : ''}</span>
      <span class="ptd-kind">${t.kind === 'ssh' ? 'SSH' : '❯'}</span>
      <span class="ptd-name"></span>`;
    li.querySelector('.ptd-name').textContent = t.title;
    const choose = () => {
      const idx = state.panes.indexOf(pane);
      closeAllPaneMenus();
      focusPane(idx);
      assignThread(idx, t.id);
    };
    li.addEventListener('click', choose);
    li.addEventListener('keydown', (ev) => {
      if (ev.key === 'ArrowDown') { ev.preventDefault(); li.nextElementSibling?.focus(); }
      else if (ev.key === 'ArrowUp') { ev.preventDefault(); li.previousElementSibling?.focus(); }
      else if (ev.key === 'Enter' || ev.key === ' ') { ev.preventDefault(); choose(); }
      else if (ev.key === 'Escape') { ev.preventDefault(); closeAllPaneMenus(); pane.ddBtn.focus(); }
    });
    pane.ddMenu.appendChild(li);
  }
}

function setPaneThread(paneIdx, threadId) {
  const pane = state.panes[paneIdx];
  if (!pane) return;
  // Park the previously shown thread's DOM (it keeps running) — but only if
  // it is still in this pane; during swaps it may already live elsewhere.
  const prev = threadById(pane.threadId);
  if (prev && prev.hostEl.parentElement === pane.body) {
    $('#thread-parking').appendChild(prev.hostEl);
  }
  pane.threadId = threadId;
  const thread = threadById(threadId);
  // Clear leftovers (empty-state placeholder) without touching a hostEl that
  // was already moved to another pane.
  for (const child of [...pane.body.children]) {
    if (!thread || child !== thread.hostEl) child.remove();
  }
  if (thread) {
    thread.activity = false; // now visible, so activity no longer pending
    pane.body.appendChild(thread.hostEl);
    requestAnimationFrame(() => {
      thread.fit.fit();
      thread.term.refresh(0, thread.term.rows - 1);
      // Don't steal focus from the sidebar's inline rename input — this rAF
      // fires one frame after the click that may have started a rename.
      if (paneIdx === state.focusedPane && !state.renaming) thread.term.focus();
    });
  } else {
    const empty = document.createElement('div');
    empty.className = 'pane-empty';
    empty.innerHTML = `
      <div class="es-icon">${icon('terminal')}</div>
      <p class="es-title">このペインは空です</p>
      <p class="es-hint">新しいスレッドを作成するか、左の一覧から選択してください。</p>`;
    const btn = document.createElement('button');
    btn.className = 'accent-btn';
    btn.innerHTML = `${icon('plus')}<span>新しいスレッド</span>`;
    btn.addEventListener('click', () => newLocalThread({ paneIdx }));
    empty.appendChild(btn);
    pane.body.appendChild(empty);
  }
  renderThreads();
}

/** Shows `threadId` in the pane; if another pane already shows it, the two
 * panes swap so a thread is never displayed twice. */
function assignThread(paneIdx, threadId) {
  const pane = state.panes[paneIdx];
  if (!pane) return;
  if (threadId) {
    const otherIdx = state.panes.findIndex((p, i) => i !== paneIdx && p.threadId === threadId);
    if (otherIdx >= 0) setPaneThread(otherIdx, pane.threadId);
  }
  setPaneThread(paneIdx, threadId);
}

function focusPane(idx) {
  if (idx < 0 || idx >= state.panes.length) return;
  state.focusedPane = idx;
  state.panes.forEach((p, i) => p.root.classList.toggle('focused', i === idx));
  threadById(state.panes[idx].threadId)?.term.focus();
  renderThreads();
}

function addPane() {
  if (state.panes.length >= 2) return;
  const pane = createPane();
  const splitter = document.createElement('div');
  splitter.className = 'splitter';
  enableSplitterDrag(splitter);
  $('#terminals').appendChild(splitter);
  $('#terminals').appendChild(pane.root);
  state.panes.push(pane);

  // Show a thread that isn't displayed yet, or open a fresh local shell.
  const shown = new Set(state.panes.map((p) => p.threadId));
  const hidden = state.threads.find((t) => !shown.has(t.id));
  const idx = state.panes.length - 1;
  if (hidden) assignThread(idx, hidden.id);
  else {
    setPaneThread(idx, null);
    newLocalThread({ paneIdx: idx });
  }
  focusPane(idx);
  updateSplitUi();
}

function removePane(idx) {
  if (state.panes.length <= 1) return;
  const [pane] = state.panes.splice(idx, 1);
  pane.ro?.disconnect(); // stop observing the detached body (avoid a leak)
  const prev = threadById(pane.threadId);
  if (prev) $('#thread-parking').appendChild(prev.hostEl); // thread keeps running
  pane.root.remove();
  $('#terminals').querySelector('.splitter')?.remove();
  state.panes[0].root.style.flex = '';
  focusPane(Math.min(state.focusedPane, state.panes.length - 1));
  updateSplitUi();
  renderThreads();
}

function toggleSplit() {
  if (state.panes.length < 2) addPane();
  else removePane(1);
}

function updateSplitUi() {
  const split = state.panes.length >= 2;
  // Update only the label span so the leading split icon stays put.
  $('#split-toggle .split-label').textContent = split ? '分割解除' : '分割';
  document.querySelectorAll('.pane-close').forEach((b) => (b.style.display = split ? '' : 'none'));
}

function enableSplitterDrag(splitter) {
  splitter.addEventListener('mousedown', (ev) => {
    ev.preventDefault();
    document.body.classList.add('row-resizing');
    const box = $('#terminals').getBoundingClientRect();
    const move = (e) => {
      const ratio = Math.min(0.85, Math.max(0.15, (e.clientY - box.top) / box.height));
      state.panes[0].root.style.flex = `0 0 calc(${(ratio * 100).toFixed(1)}% - 4px)`;
    };
    const up = () => {
      document.body.classList.remove('row-resizing');
      window.removeEventListener('mousemove', move);
      window.removeEventListener('mouseup', up);
    };
    window.addEventListener('mousemove', move);
    window.addEventListener('mouseup', up);
  });
}

/* ---------------- left thread list ---------------- */

const PANE_MARK = ['▲', '▼'];

// Google Material Symbols "push_pin" (filled when pinned, outlined otherwise),
// inlined as SVG so it works offline with no webfont/CDN dependency.
const PIN_PATH = {
  filled:
    'M16 9V4l1 0c.55 0 1-.45 1-1s-.45-1-1-1H7c-.55 0-1 .45-1 1s.45 1 1 1l1 0v5c0 ' +
    '1.66-1.34 3-3 3v2h5.97v7l1 1 1-1v-7H19v-2c-1.66 0-3-1.34-3-3z',
  outlined:
    'M14 4v5c0 1.12.37 2.16 1 3H9c.65-.86 1-1.9 1-3V4h4m3-2H7c-.55 0-1 .45-1 1s.45 ' +
    '1 1 1h1v5c0 1.66-1.34 3-3 3v2h5.97v7l1 1 1-1v-7H19v-2c-1.66 0-3-1.34-3-3V4h1c.55 ' +
    '0 1-.45 1-1s-.45-1-1-1z',
};

function pinIcon(pinned) {
  return `<svg class="mi" viewBox="0 0 24 24" aria-hidden="true"><path d="${
    pinned ? PIN_PATH.filled : PIN_PATH.outlined
  }"/></svg>`;
}

/* A small, coherent inline-SVG icon set (Lucide-style 24×24 strokes, plus a
 * couple of filled marks) so the whole UI speaks one visual language instead
 * of mixing emoji and Unicode glyphs. Inlined — no webfont/CDN. */
const ICONS = {
  menu: '<line x1="3" y1="6" x2="21" y2="6"/><line x1="3" y1="12" x2="21" y2="12"/><line x1="3" y1="18" x2="21" y2="18"/>',
  plus: '<line x1="12" y1="5" x2="12" y2="19"/><line x1="5" y1="12" x2="19" y2="12"/>',
  play: '<polygon points="6 4 20 12 6 20 6 4"/>',
  insert: '<line x1="12" y1="4" x2="12" y2="16"/><polyline points="7 11 12 16 17 11"/><line x1="5" y1="20" x2="19" y2="20"/>',
  edit: '<path d="M12 20h9"/><path d="M16.5 3.5a2.12 2.12 0 0 1 3 3L7 19l-4 1 1-4Z"/>',
  trash: '<polyline points="3 6 5 6 21 6"/><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/><line x1="10" y1="11" x2="10" y2="17"/><line x1="14" y1="11" x2="14" y2="17"/>',
  login: '<path d="M15 3h4a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2h-4"/><polyline points="10 17 15 12 10 7"/><line x1="15" y1="12" x2="3" y2="12"/>',
  star: '<polygon points="12 2 15.09 8.26 22 9.27 17 14.14 18.18 21.02 12 17.77 5.82 21.02 7 14.14 2 9.27 8.91 8.26 12 2"/>',
  x: '<line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/>',
  chevronDown: '<polyline points="6 9 12 15 18 9"/>',
  search: '<circle cx="11" cy="11" r="8"/><line x1="21" y1="21" x2="16.65" y2="16.65"/>',
  split: '<rect x="3" y="3" width="18" height="18" rx="2"/><line x1="3" y1="12" x2="21" y2="12"/>',
  terminal: '<polyline points="4 17 10 11 4 5"/><line x1="12" y1="19" x2="20" y2="19"/>',
  zap: '<polygon points="13 2 3 14 12 14 11 22 21 10 12 10 13 2"/>',
  server: '<rect x="2" y="2" width="20" height="8" rx="2"/><rect x="2" y="14" width="20" height="8" rx="2"/><line x1="6" y1="6" x2="6.01" y2="6"/><line x1="6" y1="18" x2="6.01" y2="18"/>',
  keyboard: '<rect x="2" y="4" width="20" height="16" rx="2"/><line x1="6" y1="8" x2="6" y2="8"/><line x1="10" y1="8" x2="10" y2="8"/><line x1="14" y1="8" x2="14" y2="8"/><line x1="18" y1="8" x2="18" y2="8"/><line x1="6" y1="12" x2="6" y2="12"/><line x1="10" y1="12" x2="10" y2="12"/><line x1="14" y1="12" x2="14" y2="12"/><line x1="18" y1="12" x2="18" y2="12"/><line x1="7" y1="16" x2="17" y2="16"/>',
  settings: '<path d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z"/><circle cx="12" cy="12" r="3"/>',
  info: '<circle cx="12" cy="12" r="10"/><line x1="12" y1="16" x2="12" y2="12"/><line x1="12" y1="8" x2="12.01" y2="8"/>',
  help: '<circle cx="12" cy="12" r="10"/><path d="M9.09 9a3 3 0 0 1 5.83 1c0 2-3 3-3 3"/><line x1="12" y1="17" x2="12.01" y2="17"/>',
  lock: '<rect x="3" y="11" width="18" height="11" rx="2"/><path d="M7 11V7a5 5 0 0 1 10 0v4"/>',
  alert: '<path d="M10.29 3.86 1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0z"/><line x1="12" y1="9" x2="12" y2="13"/><line x1="12" y1="17" x2="12.01" y2="17"/>',
  copy: '<rect x="9" y="9" width="13" height="13" rx="2" ry="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/>',
};
const FILLED_ICONS = new Set(['play', 'star']);

/** Returns inline SVG markup for a named icon. Stroke icons inherit
 * currentColor; the few filled marks (play/star) fill with it instead. */
function icon(name) {
  const body = ICONS[name] || '';
  const paint = FILLED_ICONS.has(name)
    ? 'fill="currentColor" stroke="none"'
    : 'fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"';
  return `<svg class="ico" viewBox="0 0 24 24" aria-hidden="true" ${paint}>${body}</svg>`;
}

/** Fills every `[data-icon]` placeholder in static markup with its icon. */
function initIcons() {
  for (const el of document.querySelectorAll('[data-icon]')) {
    el.innerHTML = icon(el.dataset.icon);
  }
}

/** Builds an illustrated empty-state row (icon + title + hint + optional CTA)
 * for a card grid. `action` = { label, onClick } renders a primary button. */
function emptyState(iconName, title, hint, action) {
  const li = document.createElement('li');
  li.className = 'empty-state';
  li.innerHTML = `<div class="es-icon">${icon(iconName)}</div>
    <div class="es-title"></div>
    <div class="es-hint"></div>`;
  li.querySelector('.es-title').textContent = title;
  const hintEl = li.querySelector('.es-hint');
  if (hint) hintEl.textContent = hint; else hintEl.remove();
  if (action) {
    const btn = document.createElement('button');
    btn.className = 'accent-btn';
    btn.innerHTML = `${icon('plus')}<span>${action.label}</span>`;
    btn.addEventListener('click', action.onClick);
    li.appendChild(btn);
  }
  return li;
}

/** Pinned threads first (in pin order), then the rest in creation order. */
function orderedThreads() {
  return state.threads
    .map((t, i) => ({ t, i }))
    .sort((a, b) => (b.t.pinned - a.t.pinned) || (a.i - b.i))
    .map((x) => x.t);
}

function togglePin(id) {
  const t = threadById(id);
  if (t) {
    t.pinned = !t.pinned;
    renderThreads();
  }
}

// Rename triggers on a double click, detected manually: the first click
// switches the pane's thread, which re-renders the list and replaces the
// <li> — WebKit then resets its click counter, so a native dblclick never
// fires. Two clicks on the same thread within 400ms count instead.
const listClick = { id: null, time: 0 };
$('#thread-list').addEventListener('click', (ev) => {
  if (ev.target.closest('button, input')) return;
  const li = ev.target.closest('.thread-item');
  if (!li) return;
  const now = Date.now();
  if (listClick.id === li.dataset.id && now - listClick.time < 400) {
    listClick.id = null;
    const t = threadById(li.dataset.id);
    // The li from this event may already be detached by the re-render the
    // first click caused — rename the live element for this thread instead.
    const liveLi = document.querySelector(`.thread-item[data-id="${li.dataset.id}"]`);
    if (t && liveLi) startRename(t, liveLi);
    return;
  }
  listClick.id = li.dataset.id;
  listClick.time = now;
});

/** Swaps a thread-item's title span for an inline `<input>`; Enter commits,
 * Escape cancels, blur commits. Empty text clears the custom title so
 * auto-titling (task 1) resumes. */
function startRename(t, li) {
  if (state.renaming) return;
  state.renaming = t.id;
  const titleEl = li.querySelector('.title');
  const input = document.createElement('input');
  input.className = 'title-input';
  input.value = t.title;
  input.autocomplete = 'off';
  let done = false;
  const finish = (commit) => {
    if (done) return;
    done = true;
    state.renaming = null;
    if (commit) {
      const trimmed = input.value.trim();
      if (trimmed) {
        t.title = trimmed;
        t.customTitle = true;
      } else {
        t.customTitle = false;
      }
    }
    renderThreads();
  };
  input.addEventListener('mousedown', (ev) => ev.stopPropagation());
  input.addEventListener('click', (ev) => ev.stopPropagation());
  input.addEventListener('keydown', (ev) => {
    ev.stopPropagation();
    if (ev.key === 'Enter') {
      ev.preventDefault();
      finish(true);
    } else if (ev.key === 'Escape') {
      ev.preventDefault();
      finish(false);
    }
  });
  input.addEventListener('blur', () => finish(true));
  titleEl.replaceWith(input);
  input.focus();
  input.select();
}

function renderThreads() {
  // A rename is in progress: its inline <input> lives outside the normal
  // render cycle, so skip re-rendering (it would otherwise wipe the input
  // out from under the user on every unrelated state change).
  if (state.renaming) return;
  const list = $('#thread-list');
  list.innerHTML = '';
  if (!state.threads.length) {
    const li = document.createElement('li');
    li.className = 'thread-empty';
    li.innerHTML = `<div class="es-icon">${icon('terminal')}</div>
      <p class="es-title">スレッドがありません</p>
      <p class="es-hint">「＋ 新規」で作成できます。</p>`;
    list.appendChild(li);
  }
  for (const t of orderedThreads()) {
    const li = document.createElement('li');
    const paneIdx = state.panes.findIndex((p) => p.threadId === t.id);
    const isFocused = paneIdx === state.focusedPane && paneIdx >= 0;
    li.className = `thread-item ${t.kind}${paneIdx >= 0 ? ' shown' : ''}${isFocused ? ' focused' : ''}${t.pinned ? ' pinned' : ''}${t.activity ? ' activity' : ''}`;
    li.dataset.id = t.id;
    li.innerHTML = `
      <span class="kind">${t.kind === 'ssh' ? 'SSH' : '❯'}</span>
      <span class="activity-dot" title="新しい出力があります"></span>
      <span class="title"></span>
      <span class="pane-mark">${paneIdx >= 0 && state.panes.length > 1 ? PANE_MARK[paneIdx] : ''}</span>
      <button class="pin" title="${t.pinned ? '固定を解除' : '左ペインに固定'}">${pinIcon(t.pinned)}</button>
      <button class="close" title="スレッドを終了">${icon('x')}</button>`;
    li.querySelector('.title').textContent = t.title;
    li.querySelector('.pin').addEventListener('click', (ev) => {
      ev.stopPropagation();
      togglePin(t.id);
    });
    li.querySelector('.close').addEventListener('click', (ev) => {
      ev.stopPropagation();
      closeThread(t.id);
    });
    li.addEventListener('click', () => assignThread(state.focusedPane, t.id));
    list.appendChild(li);
  }
  // Keep every pane's dropdown button in sync with the thread list; rebuild
  // the open menu (if any) so it reflects the latest threads/pins.
  state.panes.forEach((pane) => {
    const t = threadById(pane.threadId);
    pane.ddIcon.innerHTML = t && t.pinned ? pinIcon(true) : '';
    pane.ddLabel.textContent = t ? t.title : (state.threads.length ? '' : '(スレッドなし)');
    pane.kindEl.textContent = t ? (t.kind === 'ssh' ? 'SSH' : 'ローカル') : '';
    if (!pane.ddMenu.classList.contains('hidden')) buildPaneMenu(pane);
  });
}

/** Sends text to the focused pane's thread; `run` appends Enter. */
function sendToActive(text, run) {
  const thread = focusedThread();
  if (!thread) {
    toast('アクティブなターミナルがありません', true);
    return;
  }
  invoke('session_write', { id: thread.id, data: run ? text + '\r' : text }).catch((e) => toast(String(e), true));
  thread.term.focus();
}

/* ---------------- workflows ---------------- */

async function refreshWorkflows() {
  try {
    state.workflows = await invoke('list_workflows');
  } catch (e) {
    toast(`ワークフローの読み込みに失敗: ${e}`, true);
  }
  renderWorkflows();
  renderQuickbar();
}

/** Normalized shortcut string from a keydown event, e.g. "ctrl+shift+g".
 * Requires a ctrl/alt/meta modifier so a shortcut never fires while plainly
 * typing. Returns null for modifier-only or unmodified keys. */
function shortcutFromEvent(ev) {
  const k = ev.key;
  if (['Control', 'Alt', 'Shift', 'Meta', 'Dead'].includes(k)) return null;
  if (!(ev.ctrlKey || ev.altKey || ev.metaKey)) return null;
  const mods = [];
  if (ev.ctrlKey) mods.push('ctrl');
  if (ev.altKey) mods.push('alt');
  if (ev.shiftKey) mods.push('shift');
  if (ev.metaKey) mods.push('meta');
  let key = k.toLowerCase();
  if (key === ' ') key = 'space';
  return [...mods, key].join('+');
}

const SC_LABEL = { ctrl: 'Ctrl', alt: 'Alt', shift: 'Shift', meta: 'Meta' };
function prettyShortcut(s) {
  if (!s) return '';
  return s.split('+')
    .map((p) => SC_LABEL[p] || (p.length === 1 ? p.toUpperCase() : p[0].toUpperCase() + p.slice(1)))
    .join('+');
}

/** The workflow whose registered shortcut matches this event, if any. */
function workflowForShortcut(ev) {
  const sc = shortcutFromEvent(ev);
  if (!sc) return null;
  return state.workflows.find((w) => w.shortcut && w.shortcut === sc) || null;
}

// Combos the app itself owns (all Ctrl+Shift+…), which a workflow shortcut
// could never win, so binding them is pointless/confusing.
const RESERVED_SHORTCUTS = new Set([
  ...['p', 't', 'w', 'd', 'c', 'v', 'f', 'arrowup', 'arrowdown'].map((k) => `ctrl+shift+${k}`),
  // Command-block navigation (OSC 133).
  'ctrl+alt+arrowup', 'ctrl+alt+arrowdown',
]);

/** Returns a reason string if `sc` is a bad workflow shortcut, else null. */
function shortcutConflict(sc, excludeId) {
  if (RESERVED_SHORTCUTS.has(sc)) return 'アプリのショートカットと重複しています';
  // A plain Ctrl+<letter> (no shift/alt/meta) is a core shell key (Ctrl+C
  // interrupt, Ctrl+D EOF, Ctrl+Z suspend, …); stealing it would break the
  // terminal.
  if (/^ctrl\+[a-z0-9]$/.test(sc)) return 'シェルが使うキーのため割り当てできません';
  const dup = state.workflows.find((w) => w.id !== excludeId && w.shortcut === sc);
  if (dup) return `「${dup.name}」に割り当て済みです`;
  return null;
}

/** Renders the shell-view quick-launch bar from workflows flagged show_button. */
function renderQuickbar() {
  const bar = $('#wf-quickbar');
  bar.innerHTML = '';
  const items = state.workflows.filter((w) => w.show_button);
  bar.classList.toggle('hidden', !items.length);
  for (const w of items) {
    const btn = document.createElement('button');
    btn.className = 'wf-quick-btn';
    btn.title = (w.description || w.command) + (w.shortcut ? `  (${prettyShortcut(w.shortcut)})` : '');
    btn.innerHTML = `${icon('play')}<span class="qb-name"></span>`;
    btn.querySelector('.qb-name').textContent = w.name;
    if (w.shortcut) {
      const kbd = document.createElement('kbd');
      kbd.textContent = prettyShortcut(w.shortcut);
      btn.appendChild(kbd);
    }
    btn.addEventListener('click', () => runWorkflow(w, true));
    bar.appendChild(btn);
  }
}

function renderWorkflows() {
  const q = $('#wf-search').value.trim().toLowerCase();
  const list = $('#wf-list');
  list.innerHTML = '';
  const items = state.workflows.filter((w) =>
    !q || [w.name, w.description, w.command, (w.tags || []).join(' ')].join(' ').toLowerCase().includes(q));
  if (!items.length) {
    list.appendChild(q
      ? emptyState('search', '一致するワークフローがありません', `「${q}」に一致する項目は見つかりませんでした。`)
      : emptyState('zap', 'ワークフローがまだありません',
          'よく使うコマンドをテンプレート化すると、ワンキーや検索から実行できます。',
          { label: 'ワークフローを登録', onClick: () => editWorkflow(null) }));
    return;
  }
  for (const w of items) {
    const li = document.createElement('li');
    li.className = 'card';
    li.innerHTML = `
      <h4></h4><div class="desc"></div><code></code>
      <div class="wf-badges"></div>
      <div class="row">
        <button class="accent-btn run">${icon('play')}実行</button>
        <button class="ghost-btn insert">${icon('insert')}挿入</button>
        <button class="ghost-btn edit">${icon('edit')}編集</button>
        <button class="danger-btn del">${icon('trash')}削除</button>
      </div>`;
    li.querySelector('h4').textContent = w.name;
    li.querySelector('.desc').textContent = w.description || '';
    li.querySelector('code').textContent = w.command;
    // Tags, shortcut and the quick-button flag all flow in one wrapping meta
    // row so the card reads as a single block rather than stacked ribbons.
    const badges = li.querySelector('.wf-badges');
    for (const tag of w.tags || []) {
      const s = document.createElement('span');
      s.className = 'tag';
      s.textContent = tag;
      badges.appendChild(s);
    }
    if (w.shortcut) {
      const kbd = document.createElement('kbd');
      kbd.textContent = prettyShortcut(w.shortcut);
      badges.appendChild(kbd);
    }
    if (w.show_button) {
      const b = document.createElement('span');
      b.className = 'wf-badge';
      b.innerHTML = `${icon('star')}クイックボタン`;
      badges.appendChild(b);
    }
    li.querySelector('.run').addEventListener('click', () => runWorkflow(w, true));
    li.querySelector('.insert').addEventListener('click', () => runWorkflow(w, false));
    li.querySelector('.edit').addEventListener('click', () => editWorkflow(w));
    li.querySelector('.del').addEventListener('click', async () => {
      if (!(await confirmModal(`「${w.name}」を削除しますか?`))) return;
      await invoke('delete_workflow', { id: w.id });
      refreshWorkflows();
    });
    list.appendChild(li);
  }
}

/** Fills placeholders (prompting the user when the template has any) and
 * sends the command to the focused pane's thread. */
async function runWorkflow(w, run) {
  const placeholders = await invoke('workflow_placeholders', { command: w.command });
  let values = {};
  if (placeholders.length) {
    values = await promptPlaceholders(w, placeholders);
    if (values === null) return; // cancelled
  }
  const filled = await invoke('fill_workflow', { command: w.command, values });
  sendToActive(filled, run);
}

function editWorkflow(w) {
  const isNew = !w;
  openModal({
    title: isNew ? 'ワークフローを登録' : 'ワークフローを編集',
    okLabel: '保存',
    body: [
      fieldSection('基本情報'),
      field('名前', 'name', w?.name || '', { required: true }),
      field('説明', 'description', w?.description || ''),
      field('タグ (カンマ区切り)', 'tags', (w?.tags || []).join(', ')),
      fieldSection('コマンド'),
      fieldTextarea('テンプレート ( {{名前}} / {{名前:既定値}} でプレースホルダ )', 'command', w?.command || '', { required: true }),
      fieldSection('起動方法'),
      fieldShortcut('ショートカット (任意・Ctrl / Alt / Meta 必須)', 'shortcut', w?.shortcut || '', { excludeId: w?.id || '' }),
      fieldCheckbox('シェル表示にクイックボタンを表示', 'show_button', !!w?.show_button),
    ],
    onOk: async (values) => {
      await invoke('save_workflow', {
        workflow: {
          id: w?.id || '',
          name: values.name.trim(),
          description: values.description.trim(),
          command: values.command,
          tags: values.tags.split(',').map((t) => t.trim()).filter(Boolean),
          shortcut: values.shortcut || '',
          show_button: !!values.show_button,
        },
      });
      refreshWorkflows();
    },
  });
}

function promptPlaceholders(w, placeholders) {
  return new Promise((resolve) => {
    openModal({
      title: `${w.name} — パラメータ入力`,
      okLabel: '実行',
      body: placeholders.map((p) => field(p.name, `ph_${p.name}`, p.default ?? '')),
      onOk: (values) => {
        const out = {};
        for (const p of placeholders) out[p.name] = values[`ph_${p.name}`];
        resolve(out);
      },
      onCancel: () => resolve(null),
    });
  });
}

/* ---------------- SSH hosts ---------------- */

async function refreshHosts() {
  try {
    state.hosts = await invoke('list_ssh_hosts');
  } catch (e) {
    toast(`SSH ホストの読み込みに失敗: ${e}`, true);
  }
  renderHosts();
}

const AUTH_LABEL = { password: 'パスワード', key: '秘密鍵', agent: 'ssh-agent', keypassword: '秘密鍵+パスワード' };

function renderHosts() {
  const q = $('#host-search').value.trim().toLowerCase();
  const list = $('#host-list');
  list.innerHTML = '';
  const items = state.hosts.filter((h) =>
    !q || [h.name, h.host, h.username].join(' ').toLowerCase().includes(q));
  if (!items.length) {
    list.appendChild(q
      ? emptyState('search', '一致するホストがありません', `「${q}」に一致する接続先は見つかりませんでした。`)
      : emptyState('server', 'SSH 接続先がまだありません',
          '接続先を登録すると、新規スレッド作成時や一覧からワンクリックで接続できます。',
          { label: 'SSH ホストを登録', onClick: () => editHost(null) }));
    return;
  }
  for (const h of items) {
    const li = document.createElement('li');
    li.className = 'card';
    li.innerHTML = `
      <h4></h4>
      <div class="host-conn"></div>
      <div class="host-badges"><span class="auth-badge">${icon('lock')}<span class="auth-label"></span></span></div>
      <div class="row">
        <button class="accent-btn connect">${icon('login')}接続</button>
        <button class="ghost-btn edit">${icon('edit')}編集</button>
        <button class="danger-btn del">${icon('trash')}削除</button>
      </div>`;
    li.querySelector('h4').textContent = h.name || `${h.username}@${h.host}`;
    li.querySelector('.host-conn').textContent = `${h.username}@${h.host}:${h.port}`;
    li.querySelector('.auth-label').textContent = AUTH_LABEL[h.auth_method] || h.auth_method;
    li.querySelector('.connect').addEventListener('click', () => newSshThread(h));
    li.querySelector('.edit').addEventListener('click', () => editHost(h));
    li.querySelector('.del').addEventListener('click', async () => {
      if (!(await confirmModal(`「${h.name || h.host}」を削除しますか?`))) return;
      await invoke('delete_ssh_host', { id: h.id });
      refreshHosts();
    });
    list.appendChild(li);
  }
}

function editHost(h) {
  const isNew = !h;
  // Fingerprint the user approved during a connection test's TOFU prompt, held
  // across the two-press "test → trust & re-test" flow. Never persisted.
  let testTrustFingerprint = null;
  // Builds an SshHost from the form (the test-only secret field is excluded,
  // so it is never persisted).
  const hostFrom = (values) => ({
    id: h?.id || '',
    name: values.name.trim(),
    host: values.host.trim(),
    port: parseInt(values.port, 10) || 22,
    username: values.username.trim(),
    auth_method: values.auth_method,
    key_path: values.key_path.trim(),
  });
  openModal({
    title: isNew ? 'SSH ホストを登録' : 'SSH ホストを編集',
    okLabel: '保存',
    body: [
      fieldSection('接続先'),
      field('表示名', 'name', h?.name || ''),
      field('ホスト', 'host', h?.host || '', { required: true, placeholder: 'example.com' }),
      field('ポート', 'port', String(h?.port ?? 22), { type: 'number' }),
      field('ユーザー名', 'username', h?.username || '', { required: true }),
      fieldSection('認証'),
      fieldSelect('認証方式', 'auth_method', h?.auth_method || 'password', [
        ['password', 'パスワード'], ['key', '秘密鍵ファイル'], ['agent', 'ssh-agent'],
        ['keypassword', '秘密鍵 + パスワード'],
      ]),
      field('秘密鍵パス (認証方式: 秘密鍵 / 秘密鍵+パスワード)', 'key_path', h?.key_path || '', { placeholder: '~/.ssh/id_ed25519' }),
      fieldSection('接続テスト (保存されません)'),
      field('パスワード', 'test_password', '', { type: 'password' }),
      field('鍵パスフレーズ', 'test_passphrase', '', { type: 'password' }),
    ],
    // The connection test uses the SAME fail-closed TOFU handshake as a real
    // connect: on first contact with an unknown host it does NOT send the
    // password — it reports the fingerprint and, on a second press, re-tests
    // trusting exactly that fingerprint (which is required to exercise auth).
    extraActions: [{
      label: '接続テスト',
      className: 'ghost-btn',
      onClick: async ({ values, setStatus, btn }) => {
        if (!values.host.trim() || !values.username.trim()) {
          setStatus('ホストとユーザー名を入力してください', 'err');
          return;
        }
        const host = hostFrom(values);
        const args = { host, expectedFingerprint: testTrustFingerprint };
        // Send whichever secrets the chosen method uses.
        const pw = values.test_password || null;
        const pass = values.test_passphrase || null;
        if (host.auth_method === 'password') args.password = pw;
        else if (host.auth_method === 'key') args.passphrase = pass;
        else if (host.auth_method === 'keypassword') { args.password = pw; args.passphrase = pass; }
        // 'agent' needs no secret.
        btn.disabled = true;
        setStatus('接続テスト中…', '');
        try {
          const rep = await invoke('test_ssh_connection', args);
          const key = rep.host_key_known
            ? '既知のホスト鍵と一致'
            : `承認したホスト鍵で認証(未保存): ${rep.key_type} ${rep.fingerprint}`;
          setStatus(`✓ 接続成功・認証OK / ${key}`, 'ok');
          testTrustFingerprint = null;
          btn.textContent = '接続テスト';
        } catch (e) {
          const msg = String(e);
          const unknown = !testTrustFingerprint && msg.match(/^UNKNOWN_HOST_KEY:([^:]+):(.+)$/);
          if (unknown) {
            // First contact — no credential was sent. Show the fingerprint;
            // pressing the button again trusts it for the auth test only.
            testTrustFingerprint = unknown[2];
            btn.textContent = 'この鍵を信頼して認証テスト';
            setStatus(
              `到達可能・新しいホスト鍵 ${unknown[1]} ${unknown[2]} — 認証もテストするにはもう一度押してください(保存はされません)`,
              'ok',
            );
          } else if (msg.match(/^FINGERPRINT_MISMATCH:/)) {
            testTrustFingerprint = null;
            btn.textContent = '接続テスト';
            setStatus('承認した鍵と異なる鍵が提示されました(MITM の可能性)。中止しました', 'err');
          } else {
            testTrustFingerprint = null;
            btn.textContent = '接続テスト';
            setStatus(`✗ 接続テスト失敗: ${msg}`, 'err');
          }
        } finally {
          btn.disabled = false;
        }
      },
    }],
    onOk: async (values) => {
      await invoke('save_ssh_host', { host: hostFrom(values) });
      refreshHosts();
    },
  });
}

/** Returns {password?, passphrase?} or null when cancelled. Secrets are used
 * for the one connection only and never persisted. The combined
 * "秘密鍵 + パスワード" method collects both a key passphrase and a server
 * password. */
function promptSshSecrets(host) {
  if (host.auth_method === 'agent') return Promise.resolve({});
  const isKey = host.auth_method === 'key';
  const isCombined = host.auth_method === 'keypassword';
  return new Promise((resolve) => {
    const body = [];
    if (isCombined) {
      body.push(field('鍵のパスフレーズ (無い場合は空欄)', 'passphrase', '', { type: 'password' }));
      body.push(field(`${host.username} のパスワード`, 'password', '', { type: 'password' }));
    } else {
      body.push(field(isKey ? 'パスフレーズ (無い場合は空欄)' : `${host.username} のパスワード`,
        'secret', '', { type: 'password', autofocus: true }));
    }
    openModal({
      title: `${host.name || host.host} — ${isCombined ? '鍵パスフレーズ + パスワード' : (isKey ? '鍵のパスフレーズ' : 'パスワード')}`,
      okLabel: '接続',
      body,
      onOk: (values) => {
        if (isCombined) {
          resolve({ passphrase: values.passphrase || null, password: values.password || null });
        } else if (isKey) {
          resolve({ passphrase: values.secret || null });
        } else {
          resolve({ password: values.secret });
        }
      },
      onCancel: () => resolve(null),
    });
  });
}

/* ---------------- terminal profiles (Windows Terminal style) ---------------- */

async function refreshProfiles() {
  try {
    state.profiles = await invoke('list_profiles');
  } catch (e) {
    toast(`プロファイルの読み込みに失敗: ${e}`, true);
  }
  renderProfiles();
  renderProfileSettingOptions();
}

function isDefaultProfile(p) {
  const def = state.settings.default_profile_id;
  // A default id that no longer matches any profile (deleted) falls back to
  // the first profile, so exactly one profile always reads as default.
  const known = def && state.profiles.some((x) => x.id === def);
  return known ? p.id === def : state.profiles[0]?.id === p.id;
}

function renderProfiles() {
  const list = $('#profile-list');
  list.innerHTML = '';
  if (!state.profiles.length) {
    list.appendChild(emptyState('terminal', 'プロファイルがありません',
      '起動するシェルをプロファイルとして登録できます。',
      { label: 'プロファイルを追加', onClick: () => editProfile(null) }));
    return;
  }
  for (const p of state.profiles) {
    const li = document.createElement('li');
    li.className = 'card';
    const def = isDefaultProfile(p);
    li.innerHTML = `
      <h4><span class="star"></span><span class="pname"></span></h4>
      <div class="meta"></div>
      <div class="row">
        <button class="accent-btn launch">${icon('play')}起動</button>
        <button class="ghost-btn setdefault">${icon('star')}既定に</button>
        <button class="ghost-btn edit">${icon('edit')}編集</button>
        <button class="danger-btn del">${icon('trash')}削除</button>
      </div>`;
    li.querySelector('.star').innerHTML = def ? icon('star') : '';
    li.querySelector('.pname').textContent = p.name;
    const cmd = p.command || 'システム既定シェル';
    const argsText = (p.args || []).length ? ' ' + p.args.join(' ') : '';
    li.querySelector('.meta').textContent = cmd + argsText + (p.cwd ? `  ·  ${p.cwd}` : '');
    li.querySelector('.launch').addEventListener('click', () => newLocalThread({ profileId: p.id }));
    const setDefaultBtn = li.querySelector('.setdefault');
    setDefaultBtn.disabled = def;
    setDefaultBtn.addEventListener('click', () => setDefaultProfile(p.id));
    li.querySelector('.edit').addEventListener('click', () => editProfile(p));
    li.querySelector('.del').addEventListener('click', async () => {
      if (state.profiles.length <= 1) return toast('最後のプロファイルは削除できません', true);
      if (!(await confirmModal(`プロファイル「${p.name}」を削除しますか?`))) return;
      try {
        await invoke('delete_profile', { id: p.id });
        // Clear the configured default if it pointed at the deleted profile,
        // so settings don't reference a nonexistent profile.
        if (state.settings.default_profile_id === p.id) {
          state.settings = { ...state.settings, default_profile_id: '' };
          await invoke('save_settings', { settings: state.settings });
        }
        refreshProfiles();
      } catch (e) {
        toast(`削除に失敗: ${e}`, true);
      }
    });
    list.appendChild(li);
  }
}

async function setDefaultProfile(id) {
  state.settings = { ...state.settings, default_profile_id: id };
  await invoke('save_settings', { settings: state.settings });
  renderProfiles();
  renderProfileSettingOptions();
  toast('既定のプロファイルを変更しました');
}

function editProfile(p) {
  const isNew = !p;
  openModal({
    title: isNew ? 'プロファイルを追加' : 'プロファイルを編集',
    okLabel: '保存',
    body: [
      field('表示名', 'name', p?.name || '', { required: true }),
      field('実行ファイル (空欄 = OS 既定シェル)', 'command', p?.command || '', { placeholder: '/bin/zsh, powershell.exe …' }),
      field('引数 (スペース区切り)', 'args', (p?.args || []).join(' ')),
      field('作業ディレクトリ (空欄 = ホーム)', 'cwd', p?.cwd || '', { placeholder: '~/work' }),
    ],
    onOk: async (values) => {
      await invoke('save_profile', {
        profile: {
          id: p?.id || '',
          name: values.name.trim(),
          command: values.command.trim(),
          args: values.args.trim() ? values.args.trim().split(/\s+/) : [],
          cwd: values.cwd.trim(),
        },
      });
      refreshProfiles();
    },
  });
}

/* ---------------- settings ---------------- */

function renderProfileSettingOptions() {
  const sel = $('#settings-form').elements.default_profile_id;
  sel.innerHTML = '';
  for (const p of state.profiles) {
    const o = document.createElement('option');
    o.value = p.id;
    o.textContent = p.name;
    sel.appendChild(o);
  }
  sel.value = state.settings.default_profile_id || state.profiles[0]?.id || '';
}

async function loadSettings() {
  try {
    state.settings = await invoke('get_settings');
  } catch (e) {
    // Keep the built-in defaults from `state.settings` rather than aborting
    // boot() and leaving a blank app.
    toast(`設定の読み込みに失敗、既定値を使用します: ${e}`, true);
  }
  const f = $('#settings-form').elements;
  f.font_size.value = state.settings.font_size;
  f.font_family.value = state.settings.font_family;
  f.scrollback.value = state.settings.scrollback;
  f.theme.value = state.settings.theme === 'light' ? 'light' : 'dark';
  f.shell_integration.checked = state.settings.shell_integration !== false;
  applyTheme();
}

$('#settings-form').addEventListener('submit', async (ev) => {
  ev.preventDefault();
  const f = ev.target;
  state.settings = {
    ...state.settings,
    font_size: parseInt(f.elements.font_size.value, 10) || 14,
    default_profile_id: f.elements.default_profile_id.value,
    font_family: f.elements.font_family.value.trim(),
    scrollback: parseInt(f.elements.scrollback.value, 10) || 10000,
    theme: f.elements.theme.value === 'light' ? 'light' : 'dark',
    shell_integration: f.elements.shell_integration.checked,
  };
  await invoke('save_settings', { settings: state.settings });
  applyTheme();
  for (const t of state.threads) {
    t.term.options.fontSize = state.settings.font_size;
    t.term.options.fontFamily = fontFamilyStack();
    t.term.options.scrollback = state.settings.scrollback;
    t.fit.fit();
  }
  renderProfiles();
  toast('設定を保存しました');
});

/* ---------------- command palette ---------------- */

const palette = {
  open: false,
  items: [],
  selected: 0,
};

function paletteEntries() {
  const entries = [
    { kind: 'action', label: '新しいローカルスレッド', detail: 'Ctrl+Shift+T', run: () => newLocalThread() },
    {
      kind: 'action',
      label: state.panes.length < 2 ? 'ペインを上下分割' : 'ペイン分割を解除',
      detail: 'Ctrl+Shift+D',
      run: toggleSplit,
    },
    { kind: 'action', label: 'ワークフローを登録', detail: '', run: () => editWorkflow(null) },
    { kind: 'action', label: 'SSH ホストを登録', detail: '', run: () => editHost(null) },
    { kind: 'action', label: 'キーボードショートカット一覧', detail: 'Ctrl+Shift+/', run: openShortcuts },
  ];
  for (const p of state.profiles) {
    entries.push({
      kind: 'action',
      label: `新規スレッド: ${p.name}`,
      detail: p.command || 'システム既定シェル',
      run: () => newLocalThread({ profileId: p.id }),
    });
  }
  for (const t of state.threads) {
    entries.push({
      kind: 'thread',
      label: `表示: ${t.title}`,
      detail: t.kind === 'ssh' ? 'SSH スレッド' : 'ローカルスレッド',
      run: () => { setView('shell'); assignThread(state.focusedPane, t.id); },
    });
  }
  for (const h of state.hosts) {
    entries.push({
      kind: 'ssh',
      label: `SSH: ${h.name || h.host}`,
      detail: `${h.username}@${h.host}:${h.port}`,
      run: () => newSshThread(h),
    });
  }
  for (const w of state.workflows) {
    entries.push({
      kind: 'wf',
      label: w.name,
      detail: w.command,
      run: () => runWorkflow(w, true),
      insert: () => runWorkflow(w, false),
    });
  }
  return entries;
}

/** Subsequence fuzzy match; returns a score (lower = better) or -1. */
function fuzzyScore(query, text) {
  if (!query) return 0;
  const q = query.toLowerCase();
  const t = text.toLowerCase();
  let qi = 0;
  let score = 0;
  let last = -1;
  for (let ti = 0; ti < t.length && qi < q.length; ti++) {
    if (t[ti] === q[qi]) {
      score += last >= 0 ? ti - last - 1 : ti;
      last = ti;
      qi++;
    }
  }
  return qi === q.length ? score : -1;
}

function openPalette() {
  palette.open = true;
  $('#palette').classList.remove('hidden');
  const input = $('#palette-input');
  input.value = '';
  updatePalette();
  input.focus();
}

function closePalette() {
  palette.open = false;
  $('#palette').classList.add('hidden');
  focusedThread()?.term.focus();
}

function updatePalette() {
  const q = $('#palette-input').value.trim();
  palette.items = paletteEntries()
    .map((e) => ({ e, s: fuzzyScore(q, `${e.label} ${e.detail}`) }))
    .filter((x) => x.s >= 0)
    .sort((a, b) => a.s - b.s)
    .slice(0, 40)
    .map((x) => x.e);
  palette.selected = 0;
  renderPalette();
}

function renderPalette() {
  const list = $('#palette-results');
  list.innerHTML = '';
  palette.items.forEach((e, i) => {
    const li = document.createElement('li');
    li.className = i === palette.selected ? 'selected' : '';
    li.innerHTML = `<span class="kind-badge ${e.kind}">${
      { wf: 'CMD', ssh: 'SSH', action: 'ACT', thread: 'THR' }[e.kind]
    }</span><span class="label"></span><span class="detail"></span>`;
    li.querySelector('.label').textContent = e.label;
    li.querySelector('.detail').textContent = e.detail;
    li.addEventListener('click', () => {
      closePalette();
      e.run();
    });
    list.appendChild(li);
  });
}

$('#palette-input').addEventListener('input', updatePalette);
$('#palette-input').addEventListener('keydown', (ev) => {
  if (ev.key === 'Escape') return closePalette();
  if (ev.key === 'ArrowDown' || (ev.key === 'n' && ev.ctrlKey)) {
    palette.selected = Math.min(palette.selected + 1, palette.items.length - 1);
    renderPalette();
    ev.preventDefault();
  } else if (ev.key === 'ArrowUp' || (ev.key === 'p' && ev.ctrlKey)) {
    palette.selected = Math.max(palette.selected - 1, 0);
    renderPalette();
    ev.preventDefault();
  } else if (ev.key === 'Enter') {
    const item = palette.items[palette.selected];
    if (item) {
      closePalette();
      if (ev.shiftKey && item.insert) item.insert();
      else item.run();
    }
    ev.preventDefault();
  }
});

$('#palette').addEventListener('mousedown', (ev) => {
  if (ev.target === $('#palette')) closePalette();
});

/* ---------------- generic modal ---------------- */

let modalCtx = null;

function field(label, name, value, opts = {}) {
  return { label, name, value, tag: 'input', ...opts };
}
function fieldTextarea(label, name, value, opts = {}) {
  return { label, name, value, tag: 'textarea', ...opts };
}
function fieldSelect(label, name, value, options) {
  return { label, name, value, tag: 'select', options };
}
function fieldCheckbox(label, name, checked) {
  return { label, name, checked, tag: 'checkbox' };
}
function fieldShortcut(label, name, value, opts = {}) {
  return { label, name, value, tag: 'shortcut', ...opts };
}
/** A non-input section header that groups the fields that follow it. */
function fieldSection(label) {
  return { tag: 'section', label };
}

/** Reads the current value of every field in the modal body. Checkboxes read
 * as booleans; shortcut inputs read their normalized combo (dataset), not the
 * pretty display value. */
function collectModalValues() {
  const values = {};
  for (const el of $('#modal-body').querySelectorAll('input, textarea, select')) {
    if (el.type === 'checkbox') values[el.name] = el.checked;
    else if ('shortcut' in el.dataset) values[el.name] = el.dataset.shortcut;
    else values[el.name] = el.value;
  }
  return values;
}

function openModal({ title, okLabel, body, onOk, onCancel, extraActions }) {
  $('#modal-title').textContent = title;
  $('#modal-ok').textContent = okLabel || 'OK';
  const box = $('#modal-body');
  box.innerHTML = '';
  for (const f of body) {
    if (f.tag === 'section') {
      const sec = document.createElement('div');
      sec.className = 'modal-section';
      sec.textContent = f.label;
      box.appendChild(sec);
      continue;
    }
    const label = document.createElement('label');
    label.textContent = f.label;
    let el;
    if (f.tag === 'textarea') {
      el = document.createElement('textarea');
      el.value = f.value;
    } else if (f.tag === 'select') {
      el = document.createElement('select');
      for (const [v, text] of f.options) {
        const o = document.createElement('option');
        o.value = v;
        o.textContent = text;
        el.appendChild(o);
      }
      el.value = f.value;
    } else if (f.tag === 'checkbox') {
      el = document.createElement('input');
      el.type = 'checkbox';
      el.checked = !!f.checked;
      label.classList.add('checkbox-row'); // checkbox sits inline with its label
    } else if (f.tag === 'shortcut') {
      // A read-only input that captures a key combo on keydown; Backspace /
      // Delete / Escape clears it. The normalized combo lives in dataset,
      // the pretty form in the value.
      el = document.createElement('input');
      el.type = 'text';
      el.readOnly = true;
      el.dataset.shortcut = f.value || '';
      el.value = prettyShortcut(f.value || '');
      el.placeholder = 'クリックしてキーを押す (Backspace で消去)';
      el.addEventListener('keydown', (ev) => {
        ev.preventDefault();
        if (['Backspace', 'Delete', 'Escape'].includes(ev.key)) {
          el.dataset.shortcut = '';
          el.value = '';
          return;
        }
        const sc = shortcutFromEvent(ev);
        if (!sc) return;
        const conflict = shortcutConflict(sc, f.excludeId || '');
        if (conflict) {
          toast(`${prettyShortcut(sc)}: ${conflict}`, true);
          return; // reject — keep the previous value
        }
        el.dataset.shortcut = sc;
        el.value = prettyShortcut(sc);
      });
    } else {
      el = document.createElement('input');
      el.type = f.type || 'text';
      el.value = f.value;
      if (f.placeholder) el.placeholder = f.placeholder;
      // Keep passwords/passphrases out of the browser's autofill store.
      if (el.type === 'password') el.autocomplete = 'new-password';
    }
    el.name = f.name;
    if (f.required) el.required = true;
    label.appendChild(el);
    box.appendChild(label);
  }
  const err = document.createElement('div');
  err.className = 'modal-error';
  box.appendChild(err);
  const status = document.createElement('div');
  status.className = 'modal-status';
  box.appendChild(status);
  const setStatus = (msg, kind) => {
    status.textContent = msg || '';
    status.className = 'modal-status' + (kind ? ` ${kind}` : '');
  };

  // Extra action buttons (e.g. "接続テスト") live in the action bar, pushed to
  // the left of Cancel/OK. Rebuilt each open.
  const actions = $('#modal .modal-actions');
  actions.querySelectorAll('.modal-extra').forEach((b) => b.remove());
  for (const a of extraActions || []) {
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = `modal-extra ${a.className || 'ghost-btn'}`;
    btn.textContent = a.label;
    btn.addEventListener('click', () =>
      a.onClick({ values: collectModalValues(), setStatus, btn }));
    actions.insertBefore(btn, actions.firstChild);
  }

  modalCtx = { onOk, onCancel, err };
  $('#modal').classList.remove('hidden');
  // Focus the first field, or the OK button for a body-less confirm dialog, so
  // keyboard focus starts inside the modal (Escape/Enter and the focus trap
  // depend on it).
  const first = box.querySelector('input, textarea, select');
  (first || $('#modal-ok')).focus();
}

function closeModal(cancelled) {
  if (cancelled && modalCtx?.onCancel) modalCtx.onCancel();
  modalCtx = null;
  $('#modal').classList.add('hidden');
  // Drop any password/passphrase values from the DOM immediately rather than
  // letting them linger in hidden inputs until the next modal opens.
  $('#modal-body').innerHTML = '';
  focusedThread()?.term.focus();
}

async function submitModal() {
  if (!modalCtx) return;
  const values = collectModalValues();
  let valid = true;
  for (const el of $('#modal-body').querySelectorAll('input, textarea, select')) {
    if (el.required && !el.value.trim()) valid = false;
  }
  if (!valid) {
    modalCtx.err.textContent = '必須項目を入力してください';
    return;
  }
  const ctx = modalCtx;
  try {
    await ctx.onOk?.(values);
    modalCtx = null;
    $('#modal').classList.add('hidden');
    $('#modal-body').innerHTML = ''; // clear secrets from the DOM (see closeModal)
  } catch (e) {
    ctx.err.textContent = String(e);
  }
}

function confirmModal(message, okLabel = '削除') {
  return new Promise((resolve) => {
    openModal({
      title: message,
      okLabel,
      body: [],
      onOk: () => resolve(true),
      onCancel: () => resolve(false),
    });
  });
}

$('#modal-ok').addEventListener('click', submitModal);
$('#modal-cancel').addEventListener('click', () => closeModal(true));
$('#modal').addEventListener('mousedown', (ev) => {
  if (ev.target === $('#modal')) closeModal(true);
});
$('#modal').addEventListener('keydown', (ev) => {
  if (ev.key === 'Escape') {
    ev.preventDefault();
    closeModal(true);
    return;
  }
  // Enter submits — but NOT when a button (Cancel/OK/接続テスト) is focused, so
  // Enter on Cancel cancels instead of saving, and not inside a textarea.
  if (ev.key === 'Enter' && ev.target.tagName !== 'TEXTAREA' && ev.target.tagName !== 'BUTTON') {
    ev.preventDefault();
    submitModal();
    return;
  }
  // Keep Tab focus within the modal.
  if (ev.key === 'Tab') {
    const f = [...$('#modal').querySelectorAll('input, textarea, select, button')]
      .filter((el) => !el.disabled && el.offsetParent !== null);
    if (!f.length) return;
    const first = f[0];
    const last = f[f.length - 1];
    if (ev.shiftKey && document.activeElement === first) {
      ev.preventDefault();
      last.focus();
    } else if (!ev.shiftKey && document.activeElement === last) {
      ev.preventDefault();
      first.focus();
    }
  }
});

/* ---------------- toast ---------------- */

let toastTimer = null;

function toast(message, isError = false) {
  const el = $('#toast');
  el.className = isError ? 'error' : '';
  el.innerHTML = `<span class="toast-ico">${icon(isError ? 'alert' : 'info')}</span><span class="toast-msg"></span>`;
  el.querySelector('.toast-msg').textContent = message;
  el.classList.remove('hidden');
  // Restart the slide-in each time so repeated toasts re-animate.
  el.style.animation = 'none';
  void el.offsetWidth;
  el.style.animation = '';
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => el.classList.add('hidden'), isError ? 5000 : 2500);
}

/* ---------------- sidebars & shortcuts ---------------- */

/** Header views. "シェル" shows the terminal workspace (thread list + panes);
 * each other tab (Workflows / SSH / 端末 / 設定) takes over the whole main
 * content area with its panel, instead of a narrow right sidebar. Exactly one
 * view is shown at a time. */
let currentView = 'shell';

function setView(name) {
  currentView = name;
  const isShell = name === 'shell';
  // In a tool view the terminal workspace is hidden (see #body.tool-view CSS)
  // and the panel container fills the window.
  $('#body').classList.toggle('tool-view', !isShell);
  $('#sidebar').classList.toggle('hidden', isShell);
  if (!isShell) {
    document.querySelectorAll('.panel').forEach((p) =>
      p.classList.toggle('active', p.id === `panel-${name}`));
  }
  document.querySelectorAll('.hdr-tab').forEach((b) =>
    b.classList.toggle('active', b.dataset.panel === name));
  if (isShell) {
    // Terminals were display:none while the tool view was up — refit/refocus
    // now that they are visible again.
    requestAnimationFrame(() => {
      for (const pane of state.panes) {
        const t = threadById(pane.threadId);
        if (t) t.fit.fit();
      }
      focusedThread()?.term.focus();
    });
  }
}

/** Clicking the already-active tool tab returns to the shell view. */
function toggleView(name) {
  setView(name !== 'shell' && currentView === name ? 'shell' : name);
}

for (const btn of document.querySelectorAll('.hdr-tab')) {
  btn.addEventListener('click', () => toggleView(btn.dataset.panel));
}

$('#toggle-threadbar').addEventListener('click', () => {
  $('#threadbar').classList.toggle('hidden');
});

$('#new-thread').addEventListener('click', () => newLocalThread());
$('#new-thread-menu').addEventListener('click', (ev) => {
  ev.stopPropagation();
  toggleProfileMenu(ev.currentTarget);
});
$('#split-toggle').addEventListener('click', toggleSplit);
$('#palette-btn').addEventListener('click', openPalette);
$('#wf-add').addEventListener('click', () => editWorkflow(null));
$('#host-add').addEventListener('click', () => editHost(null));
$('#profile-add').addEventListener('click', () => editProfile(null));
$('#wf-search').addEventListener('input', renderWorkflows);
$('#host-search').addEventListener('input', renderHosts);

/* ---------------- profile picker menu (new-thread ▾) ---------------- */

function toggleProfileMenu(anchor) {
  const menu = $('#profile-menu');
  if (!menu.classList.contains('hidden')) return closeProfileMenu();
  menu.innerHTML = '';
  for (const p of state.profiles) {
    const item = document.createElement('button');
    item.className = 'popup-item';
    item.innerHTML = `<span class="pi-name"></span><span class="pi-cmd"></span>`;
    item.querySelector('.pi-name').textContent =
      (isDefaultProfile(p) ? '★ ' : '') + p.name;
    item.querySelector('.pi-cmd').textContent = p.command || 'システム既定シェル';
    item.addEventListener('click', () => {
      closeProfileMenu();
      newLocalThread({ profileId: p.id });
    });
    menu.appendChild(item);
  }
  // SSH connections: creating a new thread can open one of the saved hosts.
  if (state.hosts.length) {
    const sshSep = document.createElement('div');
    sshSep.className = 'popup-sep';
    menu.appendChild(sshSep);
    const sshLabel = document.createElement('div');
    sshLabel.className = 'popup-label';
    sshLabel.textContent = 'SSH 接続';
    menu.appendChild(sshLabel);
    for (const h of state.hosts) {
      const item = document.createElement('button');
      item.className = 'popup-item';
      item.innerHTML = `<span class="pi-name"></span><span class="pi-cmd"></span>`;
      item.querySelector('.pi-name').textContent = `⇄ ${h.name || h.host}`;
      item.querySelector('.pi-cmd').textContent = `${h.username}@${h.host}:${h.port}`;
      item.addEventListener('click', () => {
        closeProfileMenu();
        newSshThread(h);
      });
      menu.appendChild(item);
    }
  }
  const sep = document.createElement('div');
  sep.className = 'popup-sep';
  menu.appendChild(sep);
  const manage = document.createElement('button');
  manage.className = 'popup-item muted';
  manage.textContent = '⚙ プロファイルを管理…';
  manage.addEventListener('click', () => {
    closeProfileMenu();
    openSidebarPanel('profiles');
  });
  menu.appendChild(manage);

  const r = anchor.getBoundingClientRect();
  menu.style.top = `${r.bottom + 4}px`;
  menu.style.left = `${Math.max(8, r.right - 240)}px`;
  menu.classList.remove('hidden');
  setTimeout(() => window.addEventListener('mousedown', closeProfileMenuOnce), 0);
}

function closeProfileMenu() {
  $('#profile-menu').classList.add('hidden');
  window.removeEventListener('mousedown', closeProfileMenuOnce);
}
function closeProfileMenuOnce(ev) {
  if (!$('#profile-menu').contains(ev.target)) closeProfileMenu();
}

function openSidebarPanel(name) {
  setView(name);
}

/* ---------------- terminal right-click menu (workflow launcher) ---------------- */

/** Right-clicking a terminal pane opens a workflow launcher at the cursor.
 * Click runs the workflow in that pane; Shift+click only inserts the filled
 * command without executing it (same distinction as the palette). */
function openTermContextMenu(x, y) {
  const menu = $('#term-context-menu');
  menu.innerHTML = '';

  const label = document.createElement('div');
  label.className = 'popup-label';
  label.textContent = 'ワークフローを実行';
  menu.appendChild(label);

  if (!state.workflows.length) {
    const empty = document.createElement('div');
    empty.className = 'popup-empty';
    empty.textContent = 'ワークフローが未登録です';
    menu.appendChild(empty);
  }
  for (const w of state.workflows) {
    const item = document.createElement('button');
    item.className = 'popup-item';
    item.setAttribute('role', 'menuitem');
    item.innerHTML = `
      <span class="pi-line">${icon('play')}<span class="pi-name"></span></span>
      <span class="pi-cmd"></span>`;
    item.querySelector('.pi-name').textContent = w.name;
    if (w.shortcut) {
      const kbd = document.createElement('kbd');
      kbd.textContent = prettyShortcut(w.shortcut);
      item.querySelector('.pi-line').appendChild(kbd);
    }
    item.querySelector('.pi-cmd').textContent = w.command;
    item.title = (w.description || w.command) + ' — Shift+クリックで実行せず挿入';
    item.addEventListener('click', (ev) => {
      closeTermContextMenu();
      runWorkflow(w, !ev.shiftKey);
    });
    menu.appendChild(item);
  }

  const sep = document.createElement('div');
  sep.className = 'popup-sep';
  menu.appendChild(sep);
  const manage = document.createElement('button');
  manage.className = 'popup-item muted';
  manage.setAttribute('role', 'menuitem');
  manage.textContent = state.workflows.length ? '⚙ ワークフローを管理…' : '＋ ワークフローを登録…';
  manage.addEventListener('click', () => {
    closeTermContextMenu();
    if (state.workflows.length) setView('workflows');
    else editWorkflow(null);
  });
  menu.appendChild(manage);

  // Position at the cursor, clamped inside the window (unhide first so the
  // menu has measurable dimensions).
  menu.classList.remove('hidden');
  menu.style.left = `${Math.max(8, Math.min(x, window.innerWidth - menu.offsetWidth - 8))}px`;
  menu.style.top = `${Math.max(8, Math.min(y, window.innerHeight - menu.offsetHeight - 8))}px`;
  // Focus the first row so ↑/↓/Enter/Escape work immediately.
  menu.querySelector('button.popup-item')?.focus();
  setTimeout(() => window.addEventListener('mousedown', termCtxOutsideClick), 0);
}

function closeTermContextMenu() {
  $('#term-context-menu').classList.add('hidden');
  window.removeEventListener('mousedown', termCtxOutsideClick);
}

function termCtxOutsideClick(ev) {
  if (!$('#term-context-menu').contains(ev.target)) closeTermContextMenu();
}

$('#terminals').addEventListener('contextmenu', (ev) => {
  const body = ev.target.closest('.pane-body');
  if (!body) return; // quickbar/search bar keep the native menu
  ev.preventDefault();
  // Run in the pane that was right-clicked, not whichever had focus.
  const idx = state.panes.findIndex((p) => p.body === body);
  if (idx >= 0 && idx !== state.focusedPane) focusPane(idx);
  openTermContextMenu(ev.clientX, ev.clientY);
});

$('#term-context-menu').addEventListener('keydown', (ev) => {
  const items = [...ev.currentTarget.querySelectorAll('button.popup-item')];
  const idx = items.indexOf(document.activeElement);
  if (ev.key === 'Escape') {
    ev.preventDefault();
    closeTermContextMenu();
    focusedThread()?.term.focus();
  } else if (ev.key === 'ArrowDown') {
    ev.preventDefault();
    (items[idx + 1] || items[0])?.focus(); // wrap around
  } else if (ev.key === 'ArrowUp') {
    ev.preventDefault();
    (items[idx - 1] || items[items.length - 1])?.focus();
  }
});

// A stale menu position makes no sense after a resize — just close it.
window.addEventListener('resize', closeTermContextMenu);

/* ---------------- window controls (custom titlebar) ---------------- */

async function refreshMaxIcon() {
  const maxBtn = document.querySelector('#win-controls .win-btn[data-win="maximize"]');
  if (!maxBtn) return;
  const isMax = await appWindow.isMaximized();
  // Restore (overlapping squares) vs maximize (single square).
  maxBtn.textContent = isMax ? '❐' : '□';
  maxBtn.title = isMax ? '元に戻す' : '最大化';
}

for (const btn of document.querySelectorAll('.win-btn')) {
  btn.addEventListener('click', async () => {
    const action = btn.dataset.win;
    if (action === 'minimize') await appWindow.minimize();
    else if (action === 'maximize') { await appWindow.toggleMaximize(); refreshMaxIcon(); }
    else if (action === 'close') {
      // Closing kills every live shell/SSH session — confirm when any are open.
      if (state.threads.length &&
          !(await confirmModal(`${state.threads.length} 個のセッションが実行中です。終了しますか?`, '終了'))) {
        return;
      }
      await appWindow.close();
    }
  });
}

// Double-clicking the empty part of the titlebar toggles maximize (native feel).
$('#topbar').addEventListener('dblclick', (ev) => {
  if (ev.target.closest('button, select, input, .win-controls')) return;
  appWindow.toggleMaximize().then(refreshMaxIcon);
});

appWindow.onResized(() => refreshMaxIcon());

// Tag the body with the OS so CSS can place window controls natively
// (traffic lights on the left for macOS, min/max/close on the right elsewhere).
(function detectPlatform() {
  const ua = navigator.userAgent;
  const os = /Mac/i.test(ua) ? 'macos' : /Win/i.test(ua) ? 'windows' : 'linux';
  document.body.classList.add(`platform-${os}`);
})();

window.addEventListener('keydown', (ev) => {
  const mod = (ev.ctrlKey || ev.metaKey) && ev.shiftKey;
  const key = ev.key.toLowerCase();
  if (mod && key === 'p') {
    ev.preventDefault();
    palette.open ? closePalette() : openPalette();
  } else if (mod && key === 't') {
    ev.preventDefault();
    newLocalThread();
  } else if (mod && key === 'w') {
    ev.preventDefault();
    const t = focusedThread();
    if (t) closeThread(t.id);
  } else if (mod && key === 'd') {
    ev.preventDefault();
    toggleSplit();
  } else if (mod && key === 'c') {
    ev.preventDefault();
    copyFocusedSelection();
  } else if (mod && key === 'v') {
    ev.preventDefault();
    pasteIntoFocused();
  } else if (mod && key === 'f') {
    ev.preventDefault();
    termSearch.open ? closeTermSearch() : openTermSearch();
  } else if (mod && (key === '/' || key === '?')) {
    ev.preventDefault();
    shortcutsOverlay.open ? closeShortcuts() : openShortcuts();
  } else if (ev.ctrlKey && ev.altKey && key === 'arrowup') {
    ev.preventDefault();
    blockJump(-1);
  } else if (ev.ctrlKey && ev.altKey && key === 'arrowdown') {
    ev.preventDefault();
    blockJump(1);
  } else if (mod && key === 'arrowup') {
    ev.preventDefault();
    focusPane(0);
  } else if (mod && key === 'arrowdown') {
    ev.preventDefault();
    focusPane(state.panes.length - 1);
  } else if (ev.key === 'Escape' && palette.open) {
    closePalette();
  } else if (ev.key === 'Escape' && termSearch.open) {
    closeTermSearch();
  } else if (!palette.open && !modalCtx) {
    // Registered workflow shortcut (runs on the focused terminal).
    const wf = workflowForShortcut(ev);
    if (wf) {
      ev.preventDefault();
      runWorkflow(wf, true);
    }
  }
});

/* ---------------- copy & paste ---------------- */

async function copyFocusedSelection() {
  const thread = focusedThread();
  const sel = thread?.term.getSelection();
  if (!sel) return;
  try {
    await navigator.clipboard.writeText(sel);
    toast('コピーしました');
  } catch (e) {
    toast(`コピーに失敗: ${e}`, true);
  }
}

async function pasteIntoFocused() {
  const thread = focusedThread();
  if (!thread) return;
  let text;
  try {
    text = await navigator.clipboard.readText();
  } catch (e) {
    toast(`貼り付けに失敗: ${e}`, true);
    return;
  }
  if (!text) return;
  // Multi-line clipboard content can auto-run commands. Confirm first, and
  // route through term.paste() so bracketed-paste (\e[200~) wraps it when the
  // shell/editor requested it — writing straight to the PTY bypassed that.
  if (text.trimEnd().includes('\n')) {
    const lines = text.trimEnd().split('\n').length;
    if (!(await confirmModal(`${lines} 行のテキストを貼り付けますか?`, '貼り付け'))) return;
  }
  thread.term.paste(text);
  thread.term.focus();
}

/* ---------------- in-terminal search (Ctrl+Shift+F) ---------------- */

const termSearch = { open: false };

function openTermSearch() {
  termSearch.open = true;
  $('#term-search').classList.remove('hidden');
  const input = $('#term-search-input');
  input.focus();
  input.select();
}

function closeTermSearch() {
  termSearch.open = false;
  $('#term-search').classList.add('hidden');
  focusedThread()?.term.focus();
}

function termFindNext(incremental) {
  const thread = focusedThread();
  thread?.search.findNext($('#term-search-input').value, { incremental });
}

function termFindPrevious() {
  const thread = focusedThread();
  thread?.search.findPrevious($('#term-search-input').value, { incremental: false });
}

$('#term-search-input').addEventListener('input', () => termFindNext(true));
$('#term-search-input').addEventListener('keydown', (ev) => {
  if (ev.key === 'Escape') {
    ev.preventDefault();
    closeTermSearch();
  } else if (ev.key === 'Enter') {
    ev.preventDefault();
    if (ev.shiftKey) termFindPrevious();
    else termFindNext(false);
  }
});
$('#term-search-prev').addEventListener('click', () => termFindPrevious());
$('#term-search-next').addEventListener('click', () => termFindNext(false));
$('#term-search-close').addEventListener('click', closeTermSearch);

/* ---------------- command-block toolbar: copy / fold (OSC 133) ---------------- */

/** The block's [startLine, endLine] row range in the terminal buffer.
 * `end` is the row right before the next block's prompt, or the current
 * write position if this is the newest known block (no next marker yet). */
function blockRange(term, blocks, block) {
  const idx = blocks.indexOf(block);
  const start = block.marker.line;
  const next = blocks[idx + 1];
  let end;
  if (next && !next.marker.isDisposed) {
    end = next.marker.line - 1;
  } else {
    const buf = term.buffer.active;
    end = buf.baseY + buf.cursorY;
  }
  return { start, end: Math.max(start, end) };
}

/** Plain-text contents of a block (prompt line through its last output row),
 * trimmed of trailing blank lines. */
function blockText(term, blocks, block) {
  const { start, end } = blockRange(term, blocks, block);
  const buf = term.buffer.active;
  const lines = [];
  for (let i = start; i <= end; i++) {
    const line = buf.getLine(i);
    if (line) lines.push(line.translateToString(true));
  }
  while (lines.length && !lines[lines.length - 1]) lines.pop();
  return lines.join('\n');
}

/** Toggles a visual fold over a block's output rows (command line stays
 * visible). xterm.js has no concept of hiding buffer rows — this overlays an
 * opaque "N 行を折りたたみ済み" panel on xterm's 'top' decoration layer, which
 * paints above the text canvas. The underlying rows are untouched (scrollback
 * size, selection and search still see them); only the visible paint is
 * covered. */
function toggleFold(term, blocks, block) {
  if (block.collapsed) {
    block.foldDeco?.dispose();
    block.foldDeco = null;
    block.collapsed = false;
    return;
  }
  if (!block.bodyMarker || block.bodyMarker.isDisposed) return;
  const { end } = blockRange(term, blocks, block);
  const startLine = block.bodyMarker.line;
  const rows = end - startLine + 1;
  if (rows <= 1) return; // nothing worth hiding
  const deco = term.registerDecoration({
    marker: block.bodyMarker, anchor: 'left', x: 0, width: term.cols,
    height: rows, layer: 'top',
  });
  if (!deco) return;
  deco.onRender((el) => {
    el.classList.add('term-block-fold');
    if (el.dataset.bound) return;
    el.dataset.bound = '1';
    el.textContent = `▸ ${rows} 行を折りたたみ済み — クリックで展開`;
    el.addEventListener('click', () => toggleFold(term, blocks, block));
  });
  block.foldDeco = deco;
  block.collapsed = true;
}

/** Renders the small always-on-hover toolbar (copy / fold / exit chip)
 * pinned to a finished block's prompt-line right edge. */
function renderBlockToolbar(term, blocks, block, exit) {
  // Wide enough (in cells) for copy + fold + chip side by side — xterm's
  // decoration box defaults to 1 cell, which would force our flex row to
  // wrap. `layer: 'top'` keeps the buttons clickable above the text canvas.
  const deco = term.registerDecoration({ marker: block.marker, width: 10, layer: 'top' });
  if (!deco) return;
  deco.onRender((el) => {
    // xterm resets style.left on every re-render (scroll/resize) — reassert
    // the right-edge pin unconditionally, but only build content once.
    el.style.left = 'auto';
    el.style.right = '10px';
    if (el.dataset.bound) return;
    el.dataset.bound = '1';
    el.classList.add('term-block-toolbar');
    const ok = exit === 0;
    el.innerHTML = `
      <button type="button" class="term-block-btn copy" title="ブロックをコピー">${icon('copy')}</button>
      <button type="button" class="term-block-btn fold" title="出力を折りたたむ">${icon('chevronDown')}</button>
      <span class="term-block-chip ${ok ? 'ok' : 'err'}" title="${ok ? 'コマンド成功 (exit 0)' : `コマンド失敗 (exit ${exit})`}">${ok ? '✓' : `✗ ${Number.isNaN(exit) ? '?' : exit}`}</span>`;
    el.querySelector('.copy').addEventListener('click', async (ev) => {
      ev.stopPropagation();
      try {
        await navigator.clipboard.writeText(blockText(term, blocks, block));
        toast('ブロックをコピーしました');
      } catch (e) {
        toast(`コピーに失敗: ${e}`, true);
      }
    });
    const foldBtn = el.querySelector('.fold');
    if (!block.bodyMarker) {
      foldBtn.disabled = true;
      foldBtn.title = '出力がありません';
    } else {
      foldBtn.addEventListener('click', (ev) => {
        ev.stopPropagation();
        toggleFold(term, blocks, block);
        foldBtn.classList.toggle('active', block.collapsed);
        foldBtn.title = block.collapsed ? '出力を展開する' : '出力を折りたたむ';
      });
    }
  });
}

/* ---------------- command-block navigation (OSC 133) ---------------- */

/** Scrolls the focused terminal to the previous (-1) / next (+1) command
 * block's prompt line. No-op when the shell emits no OSC 133 markers. */
function blockJump(dir) {
  const thread = focusedThread();
  if (!thread || !thread.blocks?.length) return;
  const lines = thread.blocks
    .filter((b) => !b.marker.isDisposed)
    .map((b) => b.marker.line)
    .sort((a, b) => a - b);
  if (!lines.length) return;
  const cur = thread.term.buffer.active.viewportY;
  let target;
  if (dir < 0) {
    // Last block line strictly above the viewport top.
    target = [...lines].reverse().find((l) => l < cur);
  } else {
    target = lines.find((l) => l > cur);
  }
  if (target !== undefined) thread.term.scrollToLine(target);
}

/* ---------------- keyboard shortcut cheat sheet ---------------- */

const IS_MAC = /Mac/i.test(navigator.userAgent);

// The app's built-in shortcuts, grouped for the help overlay. Workflow
// shortcuts are user-defined (set in the workflow editor) so they're
// described rather than enumerated.
const SHORTCUTS = [
  { group: '全般', items: [
    ['Ctrl+Shift+P', 'コマンドパレットを開く'],
    ['Ctrl+Shift+T', '新しいローカルスレッド'],
    ['Ctrl+Shift+W', '表示中のスレッドを終了'],
    ['Ctrl+Shift+/', 'このショートカット一覧'],
  ] },
  { group: 'ペイン', items: [
    ['Ctrl+Shift+D', '上下に分割 / 分割を解除'],
    ['Ctrl+Shift+↑', '上のペインにフォーカス'],
    ['Ctrl+Shift+↓', '下のペインにフォーカス'],
  ] },
  { group: 'ターミナル', items: [
    ['Ctrl+Shift+C', '選択範囲をコピー'],
    ['Ctrl+Shift+V', 'クリップボードを貼り付け'],
    ['Ctrl+Shift+F', 'ターミナル内を検索'],
    ['Ctrl+Alt+↑', '前のコマンドブロックへ (要シェル統合)'],
    ['Ctrl+Alt+↓', '次のコマンドブロックへ (要シェル統合)'],
  ] },
  { group: 'ワークフロー', items: [
    ['任意のキー', 'ワークフロー編集画面で登録したショートカットで実行'],
    ['右クリック', 'ターミナル上のメニューから実行 (Shift+クリックで挿入のみ)'],
  ] },
];

/** Renders a "Ctrl+Shift+P" style combo as separate <kbd> chips, using mac
 * glyphs on macOS. Non-combo strings (e.g. 任意のキー) become a single chip. */
function kbdCombo(combo) {
  const map = IS_MAC
    ? { Ctrl: '⌘', Shift: '⇧', Alt: '⌥', Meta: '⌘' }
    : { Ctrl: 'Ctrl', Shift: 'Shift', Alt: 'Alt', Meta: 'Meta' };
  return combo.split('+')
    .map((t) => `<kbd>${map[t] || t}</kbd>`)
    .join('<span class="sc-plus">+</span>');
}

function renderShortcuts() {
  const body = $('#sc-body');
  body.innerHTML = '';
  for (const g of SHORTCUTS) {
    const sec = document.createElement('div');
    sec.className = 'sc-group';
    const h = document.createElement('div');
    h.className = 'sc-group-title';
    h.textContent = g.group;
    sec.appendChild(h);
    for (const [combo, desc] of g.items) {
      const row = document.createElement('div');
      row.className = 'sc-row';
      row.innerHTML = `<span class="sc-keys">${kbdCombo(combo)}</span><span class="sc-desc"></span>`;
      row.querySelector('.sc-desc').textContent = desc;
      sec.appendChild(row);
    }
    body.appendChild(sec);
  }
}

const shortcutsOverlay = { open: false };

function openShortcuts() {
  shortcutsOverlay.open = true;
  renderShortcuts();
  $('#shortcuts').classList.remove('hidden');
  $('#sc-close').focus();
}

function closeShortcuts() {
  shortcutsOverlay.open = false;
  $('#shortcuts').classList.add('hidden');
  focusedThread()?.term.focus();
}

$('#help-btn').addEventListener('click', openShortcuts);
$('#sc-close').addEventListener('click', closeShortcuts);
$('#shortcuts').addEventListener('mousedown', (ev) => {
  if (ev.target === $('#shortcuts')) closeShortcuts();
});
$('#shortcuts').addEventListener('keydown', (ev) => {
  if (ev.key === 'Escape') { ev.preventDefault(); closeShortcuts(); }
});

/* ---------------- backend events & boot ---------------- */

listen('session:data', (ev) => {
  const { id, data } = ev.payload;
  const thread = threadById(id);
  if (!thread) return;
  thread.term.write(b64ToBytes(data));
  // Flag activity only for threads not currently shown in any pane; avoid a
  // re-render on every single chunk once the flag is already set.
  if (!thread.activity && !state.panes.some((p) => p.threadId === id)) {
    thread.activity = true;
    renderThreads();
  }
});

listen('session:exit', (ev) => {
  const { id, code } = ev.payload;
  const thread = threadById(id);
  if (!thread) return;
  // A vanishing SSH thread can look like data loss, so surface it — a dropped
  // connection (code -1 from the backend) is flagged as an error.
  if (thread.kind === 'ssh') {
    if (code === -1) toast(`「${thread.title}」の接続が切断されました`, true);
    else toast(`「${thread.title}」が終了しました (code ${code})`);
  }
  closeThread(id, { kill: false });
});

(async function boot() {
  // Paint every static [data-icon] placeholder in the chrome first.
  initIcons();
  // The refresh/loadSettings helpers each swallow their own backend errors
  // (keeping defaults), so one failing store can't leave a blank window.
  await loadSettings();
  // Profiles must load before the first thread so the default profile applies.
  await Promise.all([refreshWorkflows(), refreshHosts(), refreshProfiles()]);
  const pane = createPane();
  $('#terminals').appendChild(pane.root);
  state.panes.push(pane);
  focusPane(0);
  updateSplitUi();
  setView('shell');
  refreshMaxIcon();
  await newLocalThread();
})().catch((e) => toast(`起動処理でエラー: ${e}`, true));
