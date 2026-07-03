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

const $ = (sel) => document.querySelector(sel);

const state = {
  threads: [],         // { id, title, kind, term, fit, hostEl }
  panes: [],           // { root, head, select, kindEl, closeBtn, body, threadId }
  focusedPane: 0,
  workflows: [],
  hosts: [],
  settings: { font_size: 14, shell: '' },
};

/* ---------------- terminal threads ---------------- */

const TERM_THEME = {
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
};

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

async function newLocalThread({ paneIdx = state.focusedPane } = {}) {
  const { cols, rows } = measureCells();
  try {
    const info = await invoke('create_local_session', { cols, rows });
    createThread(info, paneIdx);
  } catch (e) {
    toast(`シェルの起動に失敗: ${e}`, true);
  }
}

async function newSshThread(host) {
  const secrets = await promptSshSecrets(host);
  if (secrets === null) return; // cancelled
  const { cols, rows } = measureCells();
  toast(`${host.name || host.host} へ接続中…`);
  try {
    const info = await invoke('create_ssh_session', {
      hostId: host.id,
      password: secrets.password ?? null,
      passphrase: secrets.passphrase ?? null,
      cols, rows,
    });
    createThread(info, state.focusedPane);
    toast('接続しました');
  } catch (e) {
    toast(`SSH 接続失敗: ${e}`, true);
  }
}

function createThread(info, paneIdx) {
  const hostEl = document.createElement('div');
  hostEl.className = 'term-host';
  $('#thread-parking').appendChild(hostEl);

  const term = new Terminal({
    fontSize: state.settings.font_size,
    fontFamily: '"Cascadia Code", "JetBrains Mono", Consolas, "Noto Sans Mono CJK JP", monospace',
    theme: TERM_THEME,
    cursorBlink: true,
    allowProposedApi: true,
    scrollback: 10000,
  });
  const fit = new FitAddon.FitAddon();
  term.loadAddon(fit);
  term.loadAddon(new WebLinksAddon.WebLinksAddon());
  // App-level shortcuts must win over the shell (Ctrl+K etc. stay usable
  // inside the terminal, so app shortcuts all use Ctrl+Shift). Returning
  // false keeps xterm from consuming the event; the actual action runs in
  // the window-level keydown handler the event bubbles up to.
  term.attachCustomKeyEventHandler((ev) => {
    const mod = (ev.ctrlKey || ev.metaKey) && ev.shiftKey;
    return !(mod && ['p', 't', 'w', 'd', 'arrowup', 'arrowdown'].includes(ev.key.toLowerCase()));
  });
  term.open(hostEl);

  term.onData((data) => invoke('session_write', { id: info.id, data }).catch(() => {}));
  term.onResize(({ cols, rows }) =>
    invoke('session_resize', { id: info.id, cols, rows }).catch(() => {}));

  const thread = { id: info.id, title: info.title, kind: info.kind, term, fit, hostEl };
  state.threads.push(thread);
  assignThread(paneIdx, info.id);
  renderThreads();
}

function closeThread(id, { kill = true } = {}) {
  const idx = state.threads.findIndex((t) => t.id === id);
  if (idx < 0) return;
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
  root.innerHTML = `
    <div class="pane-head">
      <select class="pane-thread" title="このペインに表示するスレッド"></select>
      <span class="pane-kind"></span>
      <div class="spacer"></div>
      <button class="pane-close icon-btn" title="このペインを閉じる (スレッドは動き続けます)">▬</button>
    </div>
    <div class="pane-body"></div>`;
  const pane = {
    root,
    select: root.querySelector('.pane-thread'),
    kindEl: root.querySelector('.pane-kind'),
    closeBtn: root.querySelector('.pane-close'),
    body: root.querySelector('.pane-body'),
    threadId: null,
  };
  pane.select.addEventListener('change', () => {
    focusPane(state.panes.indexOf(pane));
    assignThread(state.panes.indexOf(pane), pane.select.value || null);
  });
  pane.closeBtn.addEventListener('click', () => removePane(state.panes.indexOf(pane)));
  root.addEventListener('mousedown', () => focusPane(state.panes.indexOf(pane)));
  new ResizeObserver(() => {
    const t = threadById(pane.threadId);
    if (t) t.fit.fit();
  }).observe(pane.body);
  return pane;
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
    pane.body.appendChild(thread.hostEl);
    requestAnimationFrame(() => {
      thread.fit.fit();
      thread.term.refresh(0, thread.term.rows - 1);
      if (paneIdx === state.focusedPane) thread.term.focus();
    });
  } else {
    const empty = document.createElement('div');
    empty.className = 'pane-empty';
    empty.innerHTML = '<p>表示するスレッドがありません</p>';
    const btn = document.createElement('button');
    btn.className = 'accent-btn';
    btn.textContent = '＋ 新しいスレッド';
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
  $('#split-toggle').textContent = split ? '⬓ 分割解除' : '⬒ 分割';
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

function renderThreads() {
  const list = $('#thread-list');
  list.innerHTML = '';
  if (!state.threads.length) {
    list.innerHTML = '<li class="empty">スレッドがありません</li>';
  }
  for (const t of state.threads) {
    const li = document.createElement('li');
    const paneIdx = state.panes.findIndex((p) => p.threadId === t.id);
    const isFocused = paneIdx === state.focusedPane && paneIdx >= 0;
    li.className = `thread-item ${t.kind}${paneIdx >= 0 ? ' shown' : ''}${isFocused ? ' focused' : ''}`;
    li.innerHTML = `
      <span class="kind">${t.kind === 'ssh' ? 'SSH' : '❯'}</span>
      <span class="title"></span>
      <span class="pane-mark">${paneIdx >= 0 && state.panes.length > 1 ? PANE_MARK[paneIdx] : ''}</span>
      <span class="close" title="スレッドを終了">✕</span>`;
    li.querySelector('.title').textContent = t.title;
    li.addEventListener('click', (ev) => {
      if (ev.target.classList.contains('close')) closeThread(t.id);
      else assignThread(state.focusedPane, t.id);
    });
    list.appendChild(li);
  }
  // Keep every pane's dropdown in sync with the thread list.
  state.panes.forEach((pane) => {
    pane.select.innerHTML = '';
    for (const t of state.threads) {
      const o = document.createElement('option');
      o.value = t.id;
      o.textContent = `${t.kind === 'ssh' ? '[SSH] ' : ''}${t.title}`;
      pane.select.appendChild(o);
    }
    if (!state.threads.length) {
      const o = document.createElement('option');
      o.value = '';
      o.textContent = '(スレッドなし)';
      pane.select.appendChild(o);
    }
    pane.select.value = pane.threadId || '';
    const t = threadById(pane.threadId);
    pane.kindEl.textContent = t ? (t.kind === 'ssh' ? 'SSH' : 'ローカル') : '';
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
  state.workflows = await invoke('list_workflows');
  renderWorkflows();
}

function renderWorkflows() {
  const q = $('#wf-search').value.trim().toLowerCase();
  const list = $('#wf-list');
  list.innerHTML = '';
  const items = state.workflows.filter((w) =>
    !q || [w.name, w.description, w.command, (w.tags || []).join(' ')].join(' ').toLowerCase().includes(q));
  if (!items.length) {
    list.innerHTML = '<li class="empty">ワークフローがありません</li>';
    return;
  }
  for (const w of items) {
    const li = document.createElement('li');
    li.className = 'card';
    li.innerHTML = `
      <h4></h4><div class="desc"></div><code></code>
      <div class="tags"></div>
      <div class="row">
        <button class="accent-btn run">▶ 実行</button>
        <button class="ghost-btn insert">挿入</button>
        <button class="ghost-btn edit">編集</button>
        <button class="danger-btn del">削除</button>
      </div>`;
    li.querySelector('h4').textContent = w.name;
    li.querySelector('.desc').textContent = w.description || '';
    li.querySelector('code').textContent = w.command;
    li.querySelector('.tags').innerHTML = (w.tags || [])
      .map(() => '<span class="tag"></span>').join('');
    li.querySelectorAll('.tag').forEach((el, i) => (el.textContent = w.tags[i]));
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
      field('名前', 'name', w?.name || '', { required: true }),
      field('説明', 'description', w?.description || ''),
      fieldTextarea('コマンド ( {{名前}} / {{名前:既定値}} でプレースホルダ )', 'command', w?.command || '', { required: true }),
      field('タグ (カンマ区切り)', 'tags', (w?.tags || []).join(', ')),
    ],
    onOk: async (values) => {
      await invoke('save_workflow', {
        workflow: {
          id: w?.id || '',
          name: values.name.trim(),
          description: values.description.trim(),
          command: values.command,
          tags: values.tags.split(',').map((t) => t.trim()).filter(Boolean),
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
  state.hosts = await invoke('list_ssh_hosts');
  renderHosts();
}

const AUTH_LABEL = { password: 'パスワード', key: '秘密鍵', agent: 'ssh-agent' };

function renderHosts() {
  const q = $('#host-search').value.trim().toLowerCase();
  const list = $('#host-list');
  list.innerHTML = '';
  const items = state.hosts.filter((h) =>
    !q || [h.name, h.host, h.username].join(' ').toLowerCase().includes(q));
  if (!items.length) {
    list.innerHTML = '<li class="empty">SSH ホストが未登録です</li>';
    return;
  }
  for (const h of items) {
    const li = document.createElement('li');
    li.className = 'card';
    li.innerHTML = `
      <h4></h4>
      <div class="meta"></div>
      <div class="row">
        <button class="accent-btn connect">⇄ 接続</button>
        <button class="ghost-btn edit">編集</button>
        <button class="danger-btn del">削除</button>
      </div>`;
    li.querySelector('h4').textContent = h.name || `${h.username}@${h.host}`;
    li.querySelector('.meta').textContent =
      `${h.username}@${h.host}:${h.port} · ${AUTH_LABEL[h.auth_method] || h.auth_method}`;
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
  openModal({
    title: isNew ? 'SSH ホストを登録' : 'SSH ホストを編集',
    okLabel: '保存',
    body: [
      field('表示名', 'name', h?.name || ''),
      field('ホスト', 'host', h?.host || '', { required: true, placeholder: 'example.com' }),
      field('ポート', 'port', String(h?.port ?? 22), { type: 'number' }),
      field('ユーザー名', 'username', h?.username || '', { required: true }),
      fieldSelect('認証方式', 'auth_method', h?.auth_method || 'password', [
        ['password', 'パスワード'], ['key', '秘密鍵ファイル'], ['agent', 'ssh-agent'],
      ]),
      field('秘密鍵パス (認証方式: 秘密鍵)', 'key_path', h?.key_path || '', { placeholder: '~/.ssh/id_ed25519' }),
    ],
    onOk: async (values) => {
      await invoke('save_ssh_host', {
        host: {
          id: h?.id || '',
          name: values.name.trim(),
          host: values.host.trim(),
          port: parseInt(values.port, 10) || 22,
          username: values.username.trim(),
          auth_method: values.auth_method,
          key_path: values.key_path.trim(),
        },
      });
      refreshHosts();
    },
  });
}

/** Returns {password?, passphrase?} or null when cancelled. Secrets are used
 * for the one connection only and never persisted. */
function promptSshSecrets(host) {
  if (host.auth_method === 'agent') return Promise.resolve({});
  return new Promise((resolve) => {
    const isKey = host.auth_method === 'key';
    openModal({
      title: `${host.name || host.host} — ${isKey ? '鍵のパスフレーズ' : 'パスワード'}`,
      okLabel: '接続',
      body: [
        field(isKey ? 'パスフレーズ (無い場合は空欄)' : `${host.username} のパスワード`,
          'secret', '', { type: 'password', autofocus: true }),
      ],
      onOk: (values) => resolve(isKey
        ? { passphrase: values.secret || null }
        : { password: values.secret }),
      onCancel: () => resolve(null),
    });
  });
}

/* ---------------- settings ---------------- */

async function loadSettings() {
  state.settings = await invoke('get_settings');
  const f = $('#settings-form');
  f.elements.font_size.value = state.settings.font_size;
  f.elements.shell.value = state.settings.shell;
}

$('#settings-form').addEventListener('submit', async (ev) => {
  ev.preventDefault();
  const f = ev.target;
  state.settings = {
    font_size: parseInt(f.elements.font_size.value, 10) || 14,
    shell: f.elements.shell.value.trim(),
  };
  await invoke('save_settings', { settings: state.settings });
  for (const t of state.threads) {
    t.term.options.fontSize = state.settings.font_size;
    t.fit.fit();
  }
  toast('設定を保存しました (シェル変更は新しいスレッドから)');
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
  ];
  for (const t of state.threads) {
    entries.push({
      kind: 'thread',
      label: `表示: ${t.title}`,
      detail: t.kind === 'ssh' ? 'SSH スレッド' : 'ローカルスレッド',
      run: () => assignThread(state.focusedPane, t.id),
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

function openModal({ title, okLabel, body, onOk, onCancel }) {
  $('#modal-title').textContent = title;
  $('#modal-ok').textContent = okLabel || 'OK';
  const box = $('#modal-body');
  box.innerHTML = '';
  for (const f of body) {
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
    } else {
      el = document.createElement('input');
      el.type = f.type || 'text';
      el.value = f.value;
      if (f.placeholder) el.placeholder = f.placeholder;
    }
    el.name = f.name;
    if (f.required) el.required = true;
    label.appendChild(el);
    box.appendChild(label);
  }
  const err = document.createElement('div');
  err.className = 'modal-error';
  box.appendChild(err);

  modalCtx = { onOk, onCancel, err };
  $('#modal').classList.remove('hidden');
  const first = box.querySelector('input, textarea, select');
  if (first) first.focus();
}

function closeModal(cancelled) {
  if (cancelled && modalCtx?.onCancel) modalCtx.onCancel();
  modalCtx = null;
  $('#modal').classList.add('hidden');
  focusedThread()?.term.focus();
}

async function submitModal() {
  if (!modalCtx) return;
  const values = {};
  let valid = true;
  for (const el of $('#modal-body').querySelectorAll('input, textarea, select')) {
    values[el.name] = el.value;
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
  } catch (e) {
    ctx.err.textContent = String(e);
  }
}

function confirmModal(message) {
  return new Promise((resolve) => {
    openModal({
      title: message,
      okLabel: '削除',
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
  if (ev.key === 'Escape') closeModal(true);
  if (ev.key === 'Enter' && ev.target.tagName !== 'TEXTAREA') {
    ev.preventDefault();
    submitModal();
  }
});

/* ---------------- toast ---------------- */

let toastTimer = null;

function toast(message, isError = false) {
  const el = $('#toast');
  el.textContent = message;
  el.className = isError ? 'error' : '';
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => el.classList.add('hidden'), isError ? 5000 : 2500);
}

/* ---------------- sidebars & shortcuts ---------------- */

for (const btn of document.querySelectorAll('#sidebar-tabs button')) {
  btn.addEventListener('click', () => {
    document.querySelectorAll('#sidebar-tabs button').forEach((b) => b.classList.remove('active'));
    document.querySelectorAll('.panel').forEach((p) => p.classList.remove('active'));
    btn.classList.add('active');
    $(`#panel-${btn.dataset.panel}`).classList.add('active');
  });
}

$('#toggle-sidebar').addEventListener('click', () => {
  $('#sidebar').classList.toggle('hidden');
});

$('#toggle-threadbar').addEventListener('click', () => {
  $('#threadbar').classList.toggle('hidden');
});

$('#new-thread').addEventListener('click', () => newLocalThread());
$('#split-toggle').addEventListener('click', toggleSplit);
$('#palette-btn').addEventListener('click', openPalette);
$('#wf-add').addEventListener('click', () => editWorkflow(null));
$('#host-add').addEventListener('click', () => editHost(null));
$('#wf-search').addEventListener('input', renderWorkflows);
$('#host-search').addEventListener('input', renderHosts);

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
  } else if (mod && key === 'arrowup') {
    ev.preventDefault();
    focusPane(0);
  } else if (mod && key === 'arrowdown') {
    ev.preventDefault();
    focusPane(state.panes.length - 1);
  } else if (ev.key === 'Escape' && palette.open) {
    closePalette();
  }
});

/* ---------------- backend events & boot ---------------- */

listen('session:data', (ev) => {
  const { id, data } = ev.payload;
  threadById(id)?.term.write(b64ToBytes(data));
});

listen('session:exit', (ev) => {
  const { id } = ev.payload;
  if (threadById(id)) closeThread(id, { kill: false });
});

(async function boot() {
  await loadSettings();
  await Promise.all([refreshWorkflows(), refreshHosts()]);
  const pane = createPane();
  $('#terminals').appendChild(pane.root);
  state.panes.push(pane);
  focusPane(0);
  updateSplitUi();
  await newLocalThread();
})();
