/* Senju Term frontend.
 *
 * Terminal sessions ("threads") live independently of where they are shown:
 * the left sidebar lists every thread, the center area holds one or two
 * stacked panes, and each pane chooses which thread it displays — Warp-style
 * session management. Workflows (custom commands), SSH hosts and settings
 * live in the right sidebar. Talks to the Rust backend exclusively through
 * Tauri invoke/events. */
'use strict';

const { invoke, Channel } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const appWindow = window.__TAURI__.window.getCurrentWindow();

const $ = (sel) => document.querySelector(sel);

const state = {
  threads: [],         // { id, title, kind, pinned, term, fit, hostEl, search, customTitle, activity }
  panes: [],           // { root, head, select, kindEl, closeBtn, body, threadId }
  focusedPane: 0,
  splitDir: 'v',       // 'v' = top/bottom stack, 'h' = left/right columns
  broadcast: false,    // when split: keystrokes fan out to every visible pane
  workflows: [],
  hosts: [],
  profiles: [],
  launchSets: [],      // { id, name, items: [{ profile_id, ssh_host_id, workflow_id }] }
  history: [],         // { command, kind, at } — cross-session command history
  // Tasks found in the focused thread's working directory. Cached rather than
  // fetched on demand so the palette, which builds its list synchronously, has
  // something to offer; `thread` records whose cwd it describes so a stale
  // cache is never shown against the wrong shell.
  tasks: { thread: '', cwd: '', groups: [] },
  // Whether the task pane is showing beside the panes, and how wide it is.
  // Deliberately not persisted, like the thread bar's own toggle and the
  // splitter's ratio: both are per-sitting choices about how much of the
  // window the terminal gets. The width does survive closing and reopening
  // the pane, so a size dragged once is not thrown away on the next toggle.
  taskPane: false,
  taskPaneWidth: 320,
  // Built-in dev servers. `running` mirrors the backend (which owns the real
  // sockets), `presets` is the saved-settings store, `log` is a capped ring of
  // recent requests — it exists only to be looked at, so it is never persisted.
  servers: { running: [], presets: [], log: [] },
  settings: { font_size: 14, shell: '', default_profile_id: '', font_family: '', scrollback: 10000, theme: 'dark', language: 'ja', term_theme: '', term_colors: {}, restore_session: true, gpu_rendering: true, shell_integration: true, notify_long_commands: true, notify_threshold_secs: 10, right_click: '', external_terminal: '', palette_keys: {} },
  renaming: null,      // thread id currently being renamed inline in the sidebar
  dist: null,          // { channel, installer, inAppUpdates, hasExternalManager }
};

/* ---------------- i18n (日本語 / English) ----------------
 * Every user-facing string lives here with its ja/en pair side by side so the
 * two stay in sync. `tr(key, params)` picks the current language and fills
 * `{name}` placeholders. Static markup carries `data-i18n` / `data-i18n-ph`
 * (placeholder) / `data-i18n-title` / `data-i18n-aria` attributes that
 * `applyStaticI18n()` populates; dynamic strings call `tr()` directly. Named
 * `tr` (not `t`) because `t` is used as a thread loop variable throughout. */
const STRINGS = {
  // Window controls & top bar
  'win.close': { ja: '閉じる', en: 'Close' },
  'win.min': { ja: '最小化', en: 'Minimize' },
  'win.max': { ja: '最大化', en: 'Maximize' },
  'win.restore': { ja: '元に戻す', en: 'Restore' },
  'topbar.toggleThreadbar': { ja: 'スレッド一覧を切替', en: 'Toggle thread list' },
  'tab.shell': { ja: 'シェル', en: 'Shell' },
  'tab.workflows': { ja: 'ワークフロー', en: 'Workflows' },
  'tab.hosts': { ja: 'SSH', en: 'SSH' },
  'tab.profiles': { ja: 'プロファイル', en: 'Profiles' },
  'tab.launchsets': { ja: '起動セット', en: 'Launch Sets' },
  'tab.settings': { ja: '設定', en: 'Settings' },
  'topbar.split.title': { ja: 'ペインを上下分割 (Ctrl+Shift+D)', en: 'Split pane vertically (Ctrl+Shift+D)' },
  'topbar.split': { ja: '上下分割', en: 'Split down' },
  'topbar.splitOff': { ja: '分割解除', en: 'Unsplit' },
  'topbar.splitH.title': { ja: 'ペインを左右分割 (Ctrl+Shift+E)', en: 'Split pane horizontally (Ctrl+Shift+E)' },
  'topbar.splitH': { ja: '左右分割', en: 'Split right' },
  'topbar.broadcast.title': { ja: '入力を表示中の全ペインへ送信 (Ctrl+Shift+B)', en: 'Send keystrokes to every visible pane (Ctrl+Shift+B)' },
  'topbar.broadcast': { ja: '一斉入力', en: 'Broadcast' },
  'topbar.tasks.title': { ja: 'ターミナルの横にタスク一覧を開く — 作業ディレクトリの package.json・Cargo.toml・Makefile などから実行できるコマンドを読み取ります', en: 'Open the task list beside the terminal — the commands declared by package.json, Cargo.toml, Makefile and friends in the working directory' },
  'topbar.tasks': { ja: 'タスク', en: 'Tasks' },
  'topbar.palette.title': { ja: 'コマンドパレット ({key})', en: 'Command palette ({key})' },
  'topbar.palette': { ja: 'パレット', en: 'Palette' },
  'topbar.help.title': { ja: 'キーボードショートカット (Ctrl+Shift+/)', en: 'Keyboard shortcuts (Ctrl+Shift+/)' },

  // Thread bar
  'threadbar.title': { ja: 'スレッド', en: 'Threads' },
  'threadbar.new.title': { ja: '新しいスレッド (既定プロファイル / Ctrl+Shift+T)', en: 'New thread (default profile / Ctrl+Shift+T)' },
  'threadbar.new': { ja: '新規', en: 'New' },
  'threadbar.newMenu.title': { ja: 'プロファイルを選択', en: 'Choose profile' },
  'thread.none': { ja: '(スレッドなし)', en: '(no thread)' },
  'thread.emptyTitle': { ja: 'スレッドがありません', en: 'No threads' },
  'thread.emptyHint': { ja: '「＋ 新規」で作成できます。', en: 'Create one with “+ New”.' },
  'thread.activity.title': { ja: '新しい出力があります', en: 'New output' },
  'thread.logging.title': { ja: 'セッションを記録中: {path}', en: 'Logging this session to {path}' },
  'toast.logFailed': { ja: 'セッションログを開けませんでした (記録なしで続行): {e}', en: 'Could not open the session log (continuing without one): {e}' },
  'thread.unpin': { ja: '固定を解除', en: 'Unpin' },
  'thread.pin': { ja: '左ペインに固定', en: 'Pin to the left pane' },
  'thread.close.title': { ja: 'スレッドを終了', en: 'Close thread' },
  'thread.confirmClosePinned': { ja: '固定中の「{title}」を終了しますか?', en: 'Close the pinned “{title}”?' },
  'thread.disconnected': { ja: '「{title}」の接続が切断されました', en: '“{title}” was disconnected' },
  'thread.exited': { ja: '「{title}」が終了しました (code {code})', en: '“{title}” exited (code {code})' },
  'kind.local': { ja: 'ローカル', en: 'Local' },

  // Panes
  'pane.threadBtn.title': { ja: 'このペインに表示するスレッド', en: 'Thread shown in this pane' },
  'pane.close.title': { ja: 'このペインを閉じる (スレッドは動き続けます)', en: 'Close this pane (the thread keeps running)' },
  'pane.emptyTitle': { ja: 'このペインは空です', en: 'This pane is empty' },
  'pane.emptyHint': { ja: '新しいスレッドを作成するか、左の一覧から選択してください。', en: 'Create a new thread, or pick one from the list on the left.' },
  'pane.newThread': { ja: '新しいスレッド', en: 'New thread' },

  // Terminal search
  'termsearch.ph': { ja: '検索…', en: 'Search…' },
  'termsearch.prev.title': { ja: '前を検索 (Shift+Enter)', en: 'Find previous (Shift+Enter)' },
  'termsearch.next.title': { ja: '次を検索 (Enter)', en: 'Find next (Enter)' },
  'termsearch.close.title': { ja: '閉じる (Esc)', en: 'Close (Esc)' },
  'termsearch.noMatch': { ja: '一致する文字列が見つかりません', en: 'No matches found' },

  // Common actions
  'common.run': { ja: '実行', en: 'Run' },
  'common.edit': { ja: '編集', en: 'Edit' },
  'common.delete': { ja: '削除', en: 'Delete' },
  'common.save': { ja: '保存', en: 'Save' },
  'common.cancel': { ja: 'キャンセル', en: 'Cancel' },
  'common.connect': { ja: '接続', en: 'Connect' },
  'common.ok': { ja: 'OK', en: 'OK' },
  'common.launch': { ja: '起動', en: 'Launch' },
  'common.register': { ja: '登録', en: 'Add' },
  'common.add': { ja: '追加', en: 'Add' },
  'common.moveUp': { ja: '上へ移動', en: 'Move up' },
  'common.moveDown': { ja: '下へ移動', en: 'Move down' },
  'common.duplicate': { ja: '複製', en: 'Duplicate' },
  'section.basic': { ja: '基本情報', en: 'Basics' },
  'confirm.delete': { ja: '「{name}」を削除しますか?', en: 'Delete “{name}”?' },
  'toast.deleteFailed': { ja: '削除に失敗: {e}', en: 'Failed to delete: {e}' },

  // Workflows panel
  'panel.workflows.title': { ja: 'ワークフロー', en: 'Workflows' },
  'taskpane.title': { ja: 'タスク', en: 'Tasks' },
  'tasks.refresh': { ja: '再読み込み', en: 'Refresh' },
  'tasks.run': { ja: '実行', en: 'Run' },
  'tasks.hint': { ja: 'クリックで実行 / Shift+クリックで入力欄に挿入', en: 'Click to run · Shift-click to insert without running' },
  'tasks.cwd': { ja: '作業ディレクトリ: {dir}', en: 'Working directory: {dir}' },
  'tab.servers': { ja: 'サーバー', en: 'Servers' },
  'panel.servers.title': { ja: 'サーバー', en: 'Servers' },
  'panel.servers.desc': { ja: '任意のディレクトリを組み込みサーバーで公開します。ファイルを変更するとブラウザが自動でリロードします。待ち受けは 127.0.0.1 のみで、外部からは接続できません。', en: 'Serves any directory over a built-in HTTP server, reloading the browser when a file changes. It listens on 127.0.0.1 only and is not reachable from other machines.' },
  'srv.field.root': { ja: '公開するディレクトリ', en: 'Directory to serve' },
  'srv.root.ph': { ja: '/path/to/site', en: '/path/to/site' },
  'srv.noRoot': { ja: '公開するディレクトリを指定してください。', en: 'Pick a directory to serve.' },
  'srv.browse': { ja: '選択', en: 'Browse' },
  'srv.useCwd': { ja: '現在地', en: 'Current' },
  'srv.useCwd.none': { ja: 'シェルの作業ディレクトリがまだ分かりません(シェル統合が必要です)。', en: 'The shell has not reported a working directory yet (shell integration required).' },
  'srv.field.port': { ja: 'ポート', en: 'Port' },
  'srv.field.hotReload': { ja: 'ホットリロード', en: 'Hot reload' },
  'srv.field.spa': { ja: 'SPA モード', en: 'SPA mode' },
  'srv.field.openBrowser': { ja: '起動後にブラウザを開く', en: 'Open the browser once it is up' },
  'srv.start': { ja: '起動', en: 'Start' },
  'srv.stop': { ja: '停止', en: 'Stop' },
  'srv.open': { ja: '開く', en: 'Open' },
  'srv.copy': { ja: 'URL をコピー', en: 'Copy URL' },
  'srv.savePreset': { ja: 'この設定を保存', en: 'Save this setup' },
  'srv.portHint': { ja: 'ポートに 0 を指定すると、空いているポートを自動で割り当てます。', en: 'Port 0 asks the OS for a free port.' },
  'srv.runningTitle': { ja: '起動中', en: 'Running' },
  'srv.presetsTitle': { ja: '保存した設定', en: 'Saved setups' },
  'srv.logTitle': { ja: 'アクセスログ', en: 'Access log' },
  'srv.logClear': { ja: '消去', en: 'Clear' },
  'srv.empty.running': { ja: '起動中のサーバーはありません', en: 'No server is running' },
  'srv.empty.running.hint': { ja: 'ディレクトリを選んで「起動」を押すと、ここに URL が出ます。', en: 'Pick a directory and press Start; the URL appears here.' },
  'srv.empty.presets': { ja: '保存した設定はありません', en: 'No saved setups' },
  'srv.empty.presets.hint': { ja: 'よく使う公開設定を保存しておくと、一覧から 1 クリックで起動できます。', en: 'Save a setup you use often and it starts from this list in one click.' },
  'srv.empty.log': { ja: 'まだリクエストはありません。', en: 'No requests yet.' },
  'srv.badge.hot': { ja: 'ホットリロード', en: 'Hot reload' },
  'srv.badge.spa': { ja: 'SPA', en: 'SPA' },
  'srv.uptime': { ja: '稼働 {t}', en: 'up {t}' },
  'srv.started': { ja: '{url} で起動しました', en: 'Serving at {url}' },
  'srv.stopped': { ja: 'サーバーを停止しました', en: 'Server stopped' },
  'srv.reloaded': { ja: '{path} の変更でリロードしました', en: 'Reloaded: {path} changed' },
  'srv.watchLimited': { ja: 'ディレクトリ数が多いため、監視は {n} 件までに制限しました。この範囲外のファイルを変更しても自動リロードされません。', en: 'The tree is large, so only {n} directories are watched. Changes outside that set will not reload the page.' },
  'srv.watchFailed': { ja: 'ファイル監視を開始できませんでした。サーバーは動作していますが、自動リロードは無効です: {e}', en: 'Could not watch the directory, so auto reload is off (the server itself is running): {e}' },
  'srv.copied': { ja: 'URL をコピーしました', en: 'URL copied' },
  'srv.preset.editTitle.new': { ja: 'サーバー設定を保存', en: 'Save a server setup' },
  'srv.preset.editTitle.edit': { ja: 'サーバー設定を編集', en: 'Edit server setup' },
  'srv.preset.name': { ja: '名前', en: 'Name' },
  'srv.preset.deleteConfirm': { ja: '「{name}」を削除しますか?', en: 'Delete "{name}"?' },
  'srv.autoPort': { ja: '自動', en: 'auto' },
  'palette.startServer': { ja: 'サーバー起動: {name}', en: 'Start server: {name}' },
  'palette.stopServer': { ja: 'サーバー停止: {url}', en: 'Stop server: {url}' },
  'tasks.empty.noThread.title': { ja: 'シェルがありません', en: 'No shell open' },
  'tasks.empty.ssh.title': { ja: 'SSH は未対応', en: 'SSH not supported yet' },
  'tasks.empty.noCwd.title': { ja: '作業ディレクトリが不明です', en: 'Working directory unknown' },
  'tasks.empty.none.title': { ja: 'タスクはありません', en: 'No tasks here' },
  'tasks.empty.noThread': { ja: 'シェルを開くと、その作業ディレクトリのタスクがここに並びます。', en: 'Open a shell and the tasks in its working directory will appear here.' },
  'tasks.empty.ssh': { ja: 'SSH スレッドは未対応です。作業ディレクトリを知るにはリモート側のシェル統合が必要で、まだ入れていません。', en: 'SSH threads are not supported yet: knowing the working directory needs shell integration on the remote side, which is not in place.' },
  'tasks.empty.noCwd': { ja: '作業ディレクトリをまだ受け取っていません。シェルでコマンドを 1 回実行すると分かります。シェルが bash / zsh / fish / PowerShell で、設定のシェル統合が有効である必要があります (cmd.exe は非対応です)。', en: 'No working directory reported yet. Run one command in the shell and it will arrive. The shell must be bash / zsh / fish / PowerShell with shell integration enabled in Settings (cmd.exe is not supported).' },
  'tasks.empty.none': { ja: 'このディレクトリには、読み取れるタスク定義がありませんでした。', en: 'No task definitions were found in this directory.' },
  'palette.task': { ja: 'タスク: {name}', en: 'Task: {name}' },
  'palette.tasksOpen': { ja: 'タスク一覧を開く', en: 'Open the task list' },
  'palette.tasksClose': { ja: 'タスク一覧を閉じる', en: 'Close the task list' },
  'panel.workflows.desc': { ja: 'よく使うコマンドをテンプレート化。検索・ショートカット・クイックボタンから実行できます。', en: 'Turn frequently used commands into templates. Run them from search, shortcuts, or quick buttons.' },
  'panel.workflows.search.ph': { ja: 'コマンドを検索…', en: 'Search commands…' },
  'wf.quickButton': { ja: 'クイックボタン', en: 'Quick button' },
  'wf.drag': { ja: 'ドラッグして並べ替え', en: 'Drag to reorder' },
  'toast.reorderFailed': { ja: '並べ替えに失敗: {e}', en: 'Failed to reorder: {e}' },
  'wf.tree.label': { ja: 'ワークフローのグループ', en: 'Workflow groups' },
  'wf.tree.title': { ja: 'グループ', en: 'Groups' },
  'wf.tree.all': { ja: 'すべて', en: 'All' },
  'wf.tree.ungrouped': { ja: 'グループなし', en: 'Ungrouped' },
  'wf.tree.expandAll': { ja: 'すべて展開', en: 'Expand all' },
  'wf.tree.collapseAll': { ja: 'すべて折りたたむ', en: 'Collapse all' },
  'wf.tree.newGroup': { ja: 'グループを作成', en: 'New group' },
  'wf.group.toggle': { ja: '開閉', en: 'Expand / collapse' },
  'wf.group.moveUp': { ja: 'グループを上へ移動', en: 'Move group up' },
  'wf.group.moveDown': { ja: 'グループを下へ移動', en: 'Move group down' },
  'wf.group.more': { ja: 'グループのメニュー', en: 'Group menu' },
  'wf.group.addWorkflow': { ja: 'このグループにワークフローを追加', en: 'Add a workflow to this group' },
  'wf.group.newSub': { ja: 'サブグループを作成', en: 'Create subgroup' },
  'wf.group.rename': { ja: '名前を変更', en: 'Rename' },
  'wf.group.new.title': { ja: 'グループを作成', en: 'New group' },
  'wf.group.newSub.title': { ja: 'サブグループを作成', en: 'New subgroup' },
  'wf.group.rename.title': { ja: 'グループ名を変更', en: 'Rename group' },
  'wf.group.name': { ja: 'グループ名', en: 'Group name' },
  'wf.group.next': { ja: '次へ', en: 'Next' },
  'wf.group.new.hint': { ja: 'グループはワークフローを登録した時点で作られます。次の画面で、このグループに入れる最初のワークフローを登録してください。', en: 'A group exists once it has a workflow. On the next screen, register the first workflow to go in this group.' },
  'wf.group.name.empty': { ja: 'グループ名を入力してください', en: 'Enter a group name' },
  'wf.group.name.slash': { ja: 'グループ名に / は使えません (階層は別途サブグループで作成します)', en: 'A group name cannot contain / (nest with subgroups instead)' },
  'wf.group.renameFailed': { ja: 'グループ名の変更に失敗: {e}', en: 'Failed to rename the group: {e}' },
  'wf.moved': { ja: '「{name}」を「{group}」に移動しました', en: 'Moved “{name}” to {group}' },
  'wf.moveFailed': { ja: '移動に失敗: {e}', en: 'Failed to move: {e}' },
  'wf.loadFailed': { ja: 'ワークフローの読み込みに失敗: {e}', en: 'Failed to load workflows: {e}' },
  'wf.emptyTitle': { ja: 'ワークフローがまだありません', en: 'No workflows yet' },
  'wf.emptyHint': { ja: 'よく使うコマンドをテンプレート化すると、ワンキーや検索から実行できます。', en: 'Templatize frequently used commands to run them with one key or from search.' },
  'wf.emptyCta': { ja: 'ワークフローを登録', en: 'Add workflow' },
  'wf.export': { ja: '書き出し', en: 'Export' },
  'wf.import': { ja: '取り込み', en: 'Import' },
  'wf.export.done': { ja: '{count} 件のワークフローを書き出しました', en: 'Exported {count} workflows' },
  'wf.export.failed': { ja: '書き出しに失敗: {e}', en: 'Failed to export: {e}' },
  'wf.import.title': { ja: 'ワークフローを取り込み', en: 'Import Workflows' },
  'wf.import.ok': { ja: '取り込む', en: 'Import' },
  'wf.import.hint': { ja: '取り込むワークフローにチェックを付けてください。既存とショートカットが重複する場合、ショートカットは外して取り込まれます。', en: 'Check the workflows to import. A shortcut that conflicts with an existing one is dropped on import.' },
  'wf.import.dup': { ja: '(登録済み)', en: '(already added)' },
  'wf.import.none': { ja: '取り込めるワークフローが見つかりませんでした', en: 'No importable workflows found' },
  'wf.import.done': { ja: '{count} 件のワークフローを取り込みました', en: 'Imported {count} workflows' },
  'wf.import.failed': { ja: '取り込みに失敗: {e}', en: 'Failed to import: {e}' },
  'wf.searchNoMatchTitle': { ja: '一致するワークフローがありません', en: 'No matching workflows' },
  'search.noMatch': { ja: '「{q}」に一致する項目は見つかりませんでした。', en: 'No items matched “{q}”.' },
  'wf.editTitle.new': { ja: 'ワークフローを登録', en: 'Add Workflow' },
  'wf.editTitle.edit': { ja: 'ワークフローを編集', en: 'Edit Workflow' },
  'wf.field.name': { ja: '名前', en: 'Name' },
  'wf.field.desc': { ja: '説明', en: 'Description' },
  'wf.field.group': { ja: 'グループ (階層は / 区切り。例: Deploy/Staging)', en: 'Group (nest with /, e.g. Deploy/Staging)' },
  'wf.group.ph': { ja: '未入力 = 未分類', en: 'Blank = uncategorized' },
  'wf.field.tags': { ja: 'タグ (カンマ区切り)', en: 'Tags (comma-separated)' },
  'wf.section.command': { ja: 'コマンド', en: 'Command' },
  'wf.field.template': { ja: 'テンプレート ( {{名前}} / {{名前:既定値}} / {{名前|選択肢1,選択肢2}} でプレースホルダ )', en: 'Template ( use {{name}} / {{name:default}} / {{name|choice1,choice2}} for placeholders )' },
  'wf.builtins.hint': { ja: '組み込み変数 (入力なしで自動展開): {{$cwd}} 作業ディレクトリ · {{$host}} {{$user}} {{$port}} 接続先 (SSH) · {{$thread}} スレッド名 · {{$date}} {{$time}} {{$datetime}} {{$timestamp}} 日時 · {{$selection}} 選択テキスト · {{$clipboard}} クリップボード (参照時のみ読み取り)', en: 'Built-in variables (filled without prompting): {{$cwd}} working directory · {{$host}} {{$user}} {{$port}} SSH destination · {{$thread}} thread name · {{$date}} {{$time}} {{$datetime}} {{$timestamp}} · {{$selection}} selected text · {{$clipboard}} clipboard (read only when referenced)' },
  'wf.section.launch': { ja: '起動方法', en: 'How to run' },
  'wf.field.shortcut': { ja: 'ショートカット (任意・Ctrl / Alt / Meta 必須)', en: 'Shortcut (optional; requires Ctrl / Alt / Meta)' },
  'wf.field.showButton': { ja: 'シェル表示にクイックボタンを表示', en: 'Show a quick button in the shell view' },
  'wf.paramTitle': { ja: '{name} — パラメータ入力', en: '{name} — Parameters' },
  'wf.duplicate.title': { ja: 'このワークフローを複製', en: 'Duplicate this workflow' },
  'wf.copyName': { ja: '{name} のコピー', en: '{name} (copy)' },
  'wf.duplicated': { ja: '「{name}」を作成しました', en: 'Created “{name}”' },
  'wf.duplicateFailed': { ja: '複製に失敗: {e}', en: 'Failed to duplicate: {e}' },

  // Shortcut conflicts
  'sc.conflict.reserved': { ja: 'アプリのショートカットと重複しています', en: 'Conflicts with an app shortcut' },
  'sc.conflict.shell': { ja: 'シェルが使うキーのため割り当てできません', en: 'Can’t be assigned — this key is used by the shell' },
  'sc.conflict.paletteOpen': { ja: 'コマンドパレットを開くキーに割り当て済みです', en: 'Already used to open the command palette' },
  'sc.conflict.dup': { ja: '「{name}」に割り当て済みです', en: 'Already assigned to “{name}”' },
  'shortcut.ph': { ja: 'クリックしてキーを押す (Backspace で消去)', en: 'Click and press a key (Backspace to clear)' },

  // SSH hosts panel
  'panel.hosts.title': { ja: 'SSH 接続', en: 'SSH Connections' },
  'panel.hosts.desc': { ja: '接続先を登録。新規スレッド作成時のメニューや下のリストから接続できます。グループにまとめると、一斉接続と一斉実行ができます。', en: 'Register destinations. Connect from the new-thread menu or the list below. Group hosts to connect to, or run a command on, all of them at once.' },
  'panel.hosts.search.ph': { ja: 'ホストを検索…', en: 'Search hosts…' },
  'host.loadFailed': { ja: 'SSH ホストの読み込みに失敗: {e}', en: 'Failed to load SSH hosts: {e}' },
  'auth.password': { ja: 'パスワード', en: 'Password' },
  'auth.key': { ja: '秘密鍵', en: 'Private key' },
  'auth.agent': { ja: 'ssh-agent', en: 'ssh-agent' },
  'auth.keyfile': { ja: '秘密鍵ファイル', en: 'Private key file' },
  'auth.keypassword': { ja: '秘密鍵+パスワード', en: 'Key + password' },
  'auth.keypassword2': { ja: '秘密鍵 + パスワード', en: 'Key + password' },
  'host.searchNoMatchTitle': { ja: '一致するホストがありません', en: 'No matching hosts' },
  'host.searchNoMatch': { ja: '「{q}」に一致する接続先は見つかりませんでした。', en: 'No destinations matched “{q}”.' },
  'host.emptyTitle': { ja: 'SSH 接続先がまだありません', en: 'No SSH destinations yet' },
  'host.emptyHint': { ja: '接続先を登録すると、新規スレッド作成時や一覧からワンクリックで接続できます。', en: 'Register a destination to connect from the new-thread menu or the list with one click.' },
  'host.emptyCta': { ja: 'SSH ホストを登録', en: 'Add SSH host' },
  'host.import': { ja: '~/.ssh/config を取り込み', en: 'Import ~/.ssh/config' },
  'host.import.title': { ja: 'SSH config から取り込み', en: 'Import from SSH config' },
  'host.import.ok': { ja: '取り込む', en: 'Import' },
  'host.import.hint': { ja: '取り込む接続先にチェックを付けてください。Host のワイルドカード・Match・Include は対象外です。', en: 'Check the destinations to import. Wildcard Host patterns, Match, and Include are not supported.' },
  'host.import.none': { ja: '~/.ssh/config に取り込める接続先が見つかりませんでした', en: 'No importable destinations found in ~/.ssh/config' },
  'host.import.dup': { ja: '(登録済み)', en: '(already added)' },
  'host.import.done': { ja: '{count} 件の接続先を取り込みました', en: 'Imported {count} destinations' },
  'host.import.failed': { ja: 'SSH config の読み込みに失敗: {e}', en: 'Failed to read SSH config: {e}' },
  'host.editTitle.new': { ja: 'SSH ホストを登録', en: 'Add SSH Host' },
  'host.editTitle.edit': { ja: 'SSH ホストを編集', en: 'Edit SSH Host' },
  'host.section.dest': { ja: '接続先', en: 'Destination' },
  'host.field.name': { ja: '表示名', en: 'Display name' },
  'host.field.host': { ja: 'ホスト', en: 'Host' },
  'host.field.port': { ja: 'ポート', en: 'Port' },
  'host.field.username': { ja: 'ユーザー名', en: 'Username' },
  'host.section.auth': { ja: '認証', en: 'Authentication' },
  'host.section.forwards': { ja: 'ポートフォワード', en: 'Port forwarding' },
  'host.field.forwards': { ja: 'ポートフォワード(1 行に 1 件)', en: 'Port forwards, one per line' },
  'host.field.forwards.hint': { ja: '接続時に自動開始し、切断で停止します。結果はターミナルに表示されます。\n・ローカル (-L): 「8080:localhost:80」(接頭辞なしでも -L)\n・リモート (-R): 「-R 9000:localhost:3000」— サーバー側のポート:こちらのホスト:こちらのポート\n・動的 (-D): 「-D 1080」— SOCKS5 プロキシ\nリスナーは必ずループバックに限定されます(-L / -D はこの PC の 127.0.0.1、-R はサーバーの 127.0.0.1)。', en: 'Started when the session connects, stopped when it ends. Results are shown in the terminal.\n• Local (-L): "8080:localhost:80" (the prefix may be omitted)\n• Remote (-R): "-R 9000:localhost:3000" — server_port:local_host:local_port\n• Dynamic (-D): "-D 1080" — a SOCKS5 proxy\nListeners are always bound to loopback (127.0.0.1 on this machine for -L/-D, on the server for -R).' },
  'host.section.jump': { ja: '多段接続(踏み台 / ProxyJump)', en: 'Multi-hop (jump hosts / ProxyJump)' },
  'host.field.jump': { ja: '経由する踏み台(接続順)', en: 'Jump hosts (in connection order)' },
  'host.field.jump.hint': { ja: '登録済みの SSH ホストを踏み台として上から順に経由し、このホストへ接続します(ssh -J 相当)。各踏み台は 127.0.0.1 上のトンネルを段階的に開いて接続され、ホスト鍵は段ごとに検証されます。認証情報は接続時に段ごとに入力し、保存されません。', en: 'Connect to this host through the selected saved hosts, top to bottom (like ssh -J). Each hop is reached over a 127.0.0.1 tunnel opened on the previous hop, and every hop\'s host key is verified. Credentials are entered per hop at connect time and never saved.' },
  'host.jump.add': { ja: '踏み台を追加', en: 'Add jump host' },
  'host.jump.pick': { ja: '(ホストを選択)', en: '(pick a host)' },
  'host.section.label': { ja: 'ラベル', en: 'Labels' },
  'host.section.log': { ja: 'セッションログ', en: 'Session log' },
  'host.section.reconnect': { ja: '再接続', en: 'Reconnect' },
  'host.field.autoReconnect': { ja: '接続が切れたら自動で再接続する', en: 'Reconnect automatically when the connection drops' },
  'host.field.autoReconnect.hint': { ja: '回線断などでセッションが途中で切れたとき、同じスレッドのまま 2・4・8・16・30 秒の間隔で最大 5 回つなぎ直します。exit で終了したときと、アプリの起動時(セッション復元)は対象外です。保存済みの資格情報があれば使い、無ければ最初の再試行で 1 回だけ入力を求めます。ホスト鍵の問題があれば自動では続けません。', en: 'When the session drops mid-way (a network blip, say), the same thread reconnects at 2, 4, 8, 16 and 30 s, up to five times. A clean exit and app startup (session restore) are not covered. Saved credentials are reused; otherwise the first retry asks once. Any host-key problem stops the retries.' },
  'reconnect.start': { ja: '[Senju] 接続が切れました。{sec} 秒後に再接続します ({n}/{max})', en: '[Senju] Connection lost. Reconnecting in {sec}s ({n}/{max})' },
  'reconnect.retry': { ja: '[Senju] 再接続できませんでした: {msg} — {sec} 秒後に再試行 ({n}/{max})', en: '[Senju] Reconnect failed: {msg} — retrying in {sec}s ({n}/{max})' },
  'reconnect.ok': { ja: '[Senju] 再接続しました', en: '[Senju] Reconnected' },
  'reconnect.gaveUp': { ja: '[Senju] 再接続をあきらめました: {reason}', en: '[Senju] Gave up reconnecting: {reason}' },
  'reconnect.reason.max': { ja: '{max} 回失敗', en: '{max} failures' },
  'reconnect.reason.cancelled': { ja: '資格情報の入力がキャンセルされました', en: 'credential prompt cancelled' },
  'reconnect.reason.hostKey': { ja: 'ホスト鍵に問題があります。SSH パネルから手動で接続してください', en: 'host key problem — connect manually from the SSH panel' },
  'reconnect.reason.hostGone': { ja: 'ホストの登録が削除されています', en: 'the host record was deleted' },
  'reconnect.badge.title': { ja: '再接続中 ({n}/{max})', en: 'Reconnecting ({n}/{max})' },
  'toast.reconnected': { ja: '「{title}」に再接続しました', en: 'Reconnected “{title}”' },
  'toast.reconnectGaveUp': { ja: '「{title}」の再接続をあきらめました: {reason}', en: 'Gave up reconnecting “{title}”: {reason}' },
  'host.knownHosts': { ja: 'ホスト鍵', en: 'Host keys' },
  'host.knownHosts.title': { ja: '記録済みのホスト鍵 (~/.ssh/known_hosts)', en: 'Recorded host keys (~/.ssh/known_hosts)' },
  'host.knownHosts.hint': { ja: '接続時に照合するサーバー鍵の記録です。サーバーを再インストールした等で鍵が正当に変わったときは、その行を削除すると次回の接続が初回扱い (フィンガープリントの確認) になります。鍵が変わった理由に心当たりが無い場合は削除しないでください — 中間者攻撃の可能性があります。', en: 'The server keys checked on every connection. If a server\'s key changed legitimately (a reinstall, say), remove its line and the next connection is treated as a first contact, with a fingerprint prompt. Do not remove a line if you cannot explain why the key changed — that is what a man-in-the-middle looks like.' },
  'host.knownHosts.empty': { ja: 'まだ記録されたホスト鍵はありません。', en: 'No host keys recorded yet.' },
  'host.knownHosts.hashed': { ja: '(ハッシュ化されたホスト名)', en: '(hashed host name)' },
  'host.knownHosts.unparsed': { ja: '(鍵を解析できません)', en: '(key does not parse)' },
  'host.knownHosts.line': { ja: '{line} 行目', en: 'line {line}' },
  'host.knownHosts.ok': { ja: '選択した行を削除', en: 'Remove selected' },
  'host.knownHosts.none': { ja: '削除する行を選んでください', en: 'Pick the lines to remove' },
  'host.knownHosts.removed': { ja: 'known_hosts から {n} 行削除しました', en: 'Removed {n} line(s) from known_hosts' },
  'host.keygen': { ja: '鍵ペアを生成', en: 'Generate key pair' },
  'host.keygen.title': { ja: 'SSH 鍵ペアを生成 (Ed25519)', en: 'Generate an SSH key pair (Ed25519)' },
  'host.keygen.name': { ja: 'ファイル名 (~/.ssh/ に保存)', en: 'File name (saved under ~/.ssh/)' },
  'host.keygen.comment': { ja: 'コメント (鍵の識別用・公開鍵の末尾に付きます)', en: 'Comment (labels the key; appended to the public key)' },
  'host.keygen.passphrase': { ja: 'パスフレーズ (任意・保存されません)', en: 'Passphrase (optional; never stored)' },
  'host.keygen.passphrase2': { ja: 'パスフレーズ (確認)', en: 'Passphrase (again)' },
  'host.keygen.mismatch': { ja: 'パスフレーズが一致しません', en: 'The passphrases do not match' },
  'host.keygen.hint': { ja: '秘密鍵は OpenSSH 形式で ~/.ssh/<名前> に権限 0600 で保存し、公開鍵は <名前>.pub に保存します。既存のファイルは上書きしません。', en: 'The private key is written in OpenSSH format to ~/.ssh/<name> with mode 0600, the public key to <name>.pub. Existing files are never overwritten.' },
  'host.keygen.ok': { ja: '生成', en: 'Generate' },
  'host.keygen.done.title': { ja: '鍵ペアを生成しました', en: 'Key pair generated' },
  'host.keygen.done.paths': { ja: '秘密鍵: {priv}\n公開鍵: {pub}', en: 'Private key: {priv}\nPublic key: {pub}' },
  'host.keygen.done.fp': { ja: 'フィンガープリント: {fp}', en: 'Fingerprint: {fp}' },
  'host.keygen.done.pub': { ja: '公開鍵 (サーバーの ~/.ssh/authorized_keys へ追加する 1 行)', en: 'Public key (the line to add to the server\'s ~/.ssh/authorized_keys)' },
  'host.keygen.done.copy': { ja: '公開鍵をコピー', en: 'Copy public key' },
  'host.keygen.copied': { ja: '公開鍵をコピーしました', en: 'Public key copied' },
  'host.keygen.done.next': { ja: '接続中のホストには、ホストカードの「公開鍵を配置」で authorized_keys へ追記できます。', en: 'For a host you are connected to, "Install public key" on its card appends it to authorized_keys.' },
  'host.install': { ja: '公開鍵を配置', en: 'Install public key' },
  'host.install.title': { ja: '{host} へ公開鍵を配置 (ssh-copy-id 相当)', en: 'Install a public key on {host} (like ssh-copy-id)' },
  'host.install.hint': { ja: '接続中のセッションの SFTP で、リモートの ~/.ssh/authorized_keys に公開鍵を追記します (~/.ssh は 0700、authorized_keys は 0600 に揃えます)。同じ鍵が既にあれば何もしません。', en: 'Appends the key to the remote ~/.ssh/authorized_keys over the open session\'s SFTP (~/.ssh is set to 0700, the file to 0600). Nothing happens if the key is already there.' },
  'host.install.key': { ja: '配置する公開鍵', en: 'Public key to install' },
  'host.install.switch': { ja: '配置後、このホストの認証方式を「秘密鍵」に切り替える', en: 'Afterwards, switch this host\'s auth method to the private key' },
  'host.install.ok': { ja: '配置', en: 'Install' },
  'host.install.needSession': { ja: 'このホストへ接続中のスレッドがありません。パスワード等で一度接続してから実行してください。', en: 'No open session to this host. Connect once (with a password, say) and try again.' },
  'host.install.noKeys': { ja: '~/.ssh に公開鍵 (.pub) がありません。先に「鍵ペアを生成」してください。', en: 'No public key (.pub) in ~/.ssh. Generate a key pair first.' },
  'host.install.added': { ja: '{host} の authorized_keys へ公開鍵を追加しました', en: 'Added the public key to authorized_keys on {host}' },
  'host.install.present': { ja: '{host} には同じ公開鍵が既にあります', en: '{host} already has that public key' },
  'host.install.switched': { ja: '認証方式を秘密鍵 ({path}) に切り替えました', en: 'Auth method switched to the private key ({path})' },
  'ssh.mismatch.title': { ja: 'ホスト鍵が記録と一致しません: {host}', en: 'Host key does not match the record: {host}' },
  'ssh.mismatch.body': { ja: 'known_hosts の {line} 行目に記録された鍵と、サーバーが提示した鍵 ({keyType} {fingerprint}) が異なります。中間者攻撃の可能性があるため接続を中止しました。\n\nサーバーを再インストールした等、鍵が変わった理由に心当たりがある場合だけ、記録を削除して接続し直してください。次の接続は初回扱いになり、新しいフィンガープリントの確認が出ます。', en: 'The key the server presented ({keyType} {fingerprint}) differs from the one recorded on line {line} of known_hosts. The connection was aborted: this is what a man-in-the-middle looks like.\n\nOnly if you know why the key changed (the server was reinstalled, say), remove the record and reconnect. The next connection is then a first contact, with a prompt for the new fingerprint.' },
  'ssh.mismatch.remove': { ja: '記録を削除して接続し直す', en: 'Remove the record and reconnect' },
  'host.field.logSessions': { ja: 'このホストへのセッションを記録する', en: 'Log sessions to this host' },
  'host.field.logSessions.hint': { ja: '接続するたびに、ターミナルに流れた内容を設定「セッションログ」のフォルダへ保存します。記録中のスレッドには一覧に ● が付きます。', en: 'Every session to this host is written to the folder set under Settings > Session logs. Threads being logged show a ● in the list.' },
  'host.field.group': { ja: 'グループ (階層は / 区切り。例: Prod/Web)', en: 'Group (nest with /, e.g. Prod/Web)' },
  'host.group.ph': { ja: '未入力 = 未分類', en: 'Blank = uncategorized' },
  'host.field.tags': { ja: 'タグ (カンマ区切り)', en: 'Tags (comma-separated)' },
  'host.field.color': { ja: '色ラベル', en: 'Color label' },
  'host.field.color.hint': { ja: 'カードとスレッド一覧に色が付き、接続中はペインがこの色で縁取られます。本番機を赤にしておくと、打つ前に気づけます。', en: 'Colors the card and its thread rows, and frames the pane while connected — mark production red and you notice before you type.' },
  'host.color.none': { ja: 'なし', en: 'None' },
  'host.color.red': { ja: '赤', en: 'Red' },
  'host.color.orange': { ja: '橙', en: 'Orange' },
  'host.color.yellow': { ja: '黄', en: 'Yellow' },
  'host.color.green': { ja: '緑', en: 'Green' },
  'host.color.blue': { ja: '青', en: 'Blue' },
  'host.color.purple': { ja: '紫', en: 'Purple' },
  'host.group.connectAll': { ja: 'すべて接続', en: 'Connect all' },
  'host.group.connectAll.title': { ja: 'このグループの未接続ホストへ順に接続します', en: 'Connect every host in this group that is not open yet, one after another' },
  'host.group.runAll': { ja: '一斉実行…', en: 'Run on all…' },
  'host.group.runAll.title': { ja: 'このグループの接続中ホストへ同じコマンドを送ります', en: 'Send one command to every connected host in this group' },
  'host.connectAll.allOpen': { ja: '{group} のホストはすべて接続済みです', en: 'Every host in {group} is already connected' },
  'host.connectAll.skipped': { ja: '接続済みの {n} 件はスキップします', en: 'Skipping {n} already connected' },
  'host.run.title': { ja: '{group} へ一斉実行', en: 'Run on {group}' },
  'host.run.command': { ja: 'コマンド', en: 'Command' },
  'host.run.targets': { ja: '送信先 (接続中のホストのみ)', en: 'Targets (connected hosts only)' },
  'host.run.notConnected': { ja: '未接続 — 送られません', en: 'not connected — will not receive it' },
  'host.run.none': { ja: '{group} に接続中のホストがありません。先に「すべて接続」してください', en: 'No connected host in {group}. Use “Connect all” first' },
  'host.run.ok': { ja: '実行', en: 'Run' },
  'host.run.noneChecked': { ja: '送信先を 1 つ以上選んでください', en: 'Pick at least one target' },
  'host.run.sent': { ja: '{n} 件のホストへ送信しました', en: 'Sent to {n} host(s)' },
  'ssh.jump.missing': { ja: '踏み台に指定されたホストが見つかりません。設定を確認してください', en: 'A configured jump host no longer exists — check the host settings.' },
  'ssh.jump.selfRef': { ja: '踏み台に接続先自身は指定できません', en: 'A host cannot be its own jump host.' },
  'secret.hop': { ja: '{role}: {name}', en: '{role}: {name}' },
  'secret.role.jump': { ja: '踏み台', en: 'Jump host' },
  'secret.role.target': { ja: '接続先', en: 'Target' },
  'secret.chainTitle': { ja: '{name} へ多段接続 — 認証情報', en: 'Multi-hop connect to {name} — credentials' },
  'host.field.authMethod': { ja: '認証方式', en: 'Auth method' },
  'host.field.keyPath': { ja: '秘密鍵パス (認証方式: 秘密鍵 / 秘密鍵+パスワード)', en: 'Private key path (for key / key+password auth)' },
  'host.section.test': { ja: '接続テスト (保存されません)', en: 'Connection test (not saved)' },
  'host.field.testPassword': { ja: 'パスワード', en: 'Password' },
  'host.field.testPassphrase': { ja: '鍵パスフレーズ', en: 'Key passphrase' },
  'host.test': { ja: '接続テスト', en: 'Test connection' },
  'host.test.needHostUser': { ja: 'ホストとユーザー名を入力してください', en: 'Enter host and username' },
  'host.test.running': { ja: '接続テスト中…', en: 'Testing connection…' },
  'host.test.keyMatch': { ja: '既知のホスト鍵と一致', en: 'Matches the known host key' },
  'host.test.keyApproved': { ja: '承認したホスト鍵で認証(未保存): {type} {fp}', en: 'Authenticated with the approved host key (unsaved): {type} {fp}' },
  'host.test.ok': { ja: '✓ 接続成功・認証OK / {key}', en: '✓ Connected & authenticated / {key}' },
  'host.test.trustBtn': { ja: 'この鍵を信頼して認証テスト', en: 'Trust this key & test auth' },
  'host.test.newKey': { ja: '到達可能・新しいホスト鍵 {type} {fp} — 認証もテストするにはもう一度押してください(保存はされません)', en: 'Reachable; new host key {type} {fp} — press again to also test auth (nothing is saved)' },
  'host.test.mitm': { ja: '承認した鍵と異なる鍵が提示されました(MITM の可能性)。中止しました', en: 'A different key than approved was presented (possible MITM). Aborted.' },
  'host.test.failed': { ja: '✗ 接続テスト失敗: {msg}', en: '✗ Connection test failed: {msg}' },

  // SSH connect flow
  'toast.connecting': { ja: '{name} へ接続中…', en: 'Connecting to {name}…' },
  'toast.connected': { ja: '接続しました', en: 'Connected' },
  'toast.shellStartFailed': { ja: 'シェルの起動に失敗: {e}', en: 'Failed to start shell: {e}' },
  'toast.sshFailed': { ja: 'SSH 接続失敗: {msg}', en: 'SSH connection failed: {msg}' },
  'ssh.mitmAborted': { ja: 'ホスト {host} の鍵に問題があります(記録済み/承認済みの鍵と一致しません)。中間者攻撃の可能性があるため接続を中止しました', en: 'Host key problem for {host} (does not match the recorded/approved key). Aborted the connection due to a possible man-in-the-middle attack.' },
  'ssh.unknownAborted': { ja: '未知のホスト鍵のため接続を中止しました', en: 'Aborted: unknown host key.' },
  'ssh.trustPrompt': { ja: '初回接続です。このホスト鍵を信頼して known_hosts に保存し、接続しますか? [ホスト: {host}:{port} / 鍵種別: {keyType} / フィンガープリント: {fingerprint}]', en: 'First connection. Trust this host key, save it to known_hosts, and connect? [Host: {host}:{port} / Key type: {keyType} / Fingerprint: {fingerprint}]' },
  'ssh.trustConnect': { ja: '信頼して接続', en: 'Trust & connect' },
  'secret.passphraseOpt': { ja: '鍵のパスフレーズ (無い場合は空欄)', en: 'Key passphrase (blank if none)' },
  'secret.passphraseOnly': { ja: 'パスフレーズ (無い場合は空欄)', en: 'Passphrase (blank if none)' },
  'secret.userPassword': { ja: '{user} のパスワード', en: 'Password for {user}' },
  'secret.title': { ja: '{name} — {kind}', en: '{name} — {kind}' },
  'secret.kind.combined': { ja: '鍵パスフレーズ + パスワード', en: 'Key passphrase + password' },
  'secret.kind.key': { ja: '鍵のパスフレーズ', en: 'Key passphrase' },
  'secret.kind.password': { ja: 'パスワード', en: 'Password' },

  // Profiles panel
  'panel.profiles.title': { ja: 'ターミナルプロファイル', en: 'Terminal Profiles' },
  'panel.profiles.desc': { ja: '起動するシェルをプロファイルとして管理します。★ が「＋ 新規」で起動する既定です。', en: 'Manage the shells you launch as profiles. ★ is the default launched by “+ New”.' },
  'profile.loadFailed': { ja: 'プロファイルの読み込みに失敗: {e}', en: 'Failed to load profiles: {e}' },
  'profile.emptyTitle': { ja: 'プロファイルがありません', en: 'No profiles' },
  'profile.emptyHint': { ja: '起動するシェルをプロファイルとして登録できます。', en: 'Register the shells you launch as profiles.' },
  'profile.emptyCta': { ja: 'プロファイルを追加', en: 'Add profile' },
  'profile.setDefault': { ja: '既定に', en: 'Set default' },
  'profile.systemDefault': { ja: 'システム既定シェル', en: 'System default shell' },
  'profile.cantDeleteLast': { ja: '最後のプロファイルは削除できません', en: 'Can’t delete the last profile' },
  'profile.confirmDelete': { ja: 'プロファイル「{name}」を削除しますか?', en: 'Delete profile “{name}”?' },
  'profile.defaultChanged': { ja: '既定のプロファイルを変更しました', en: 'Default profile changed' },
  'profile.editTitle.new': { ja: 'プロファイルを追加', en: 'Add Profile' },
  'profile.editTitle.edit': { ja: 'プロファイルを編集', en: 'Edit Profile' },
  'profile.field.command': { ja: '実行ファイル (空欄 = OS 既定シェル)', en: 'Executable (blank = OS default shell)' },
  'profile.field.args': { ja: '引数 (スペース区切り)', en: 'Arguments (space-separated)' },
  'profile.field.cwd': { ja: '作業ディレクトリ (空欄 = ホーム)', en: 'Working directory (blank = home)' },

  // Launch sets panel
  'panel.launchsets.title': { ja: '起動セット', en: 'Launch Sets' },
  'panel.launchsets.desc': { ja: 'いつも開くシェル・SSH 接続と、そこで実行するワークフローやコマンドをまとめて登録し、ワンクリックで一括起動できます。', en: 'Register the shells and SSH connections you always open, plus the workflows and commands to run in them, then launch it all with one click.' },
  'set.loadFailed': { ja: '起動セットの読み込みに失敗: {e}', en: 'Failed to load launch sets: {e}' },
  'set.deletedRef.ssh': { ja: 'SSH (削除済み)', en: 'SSH (deleted)' },
  'set.deletedRef.profile': { ja: 'プロファイル (削除済み)', en: 'Profile (deleted)' },
  'set.emptyTitle': { ja: '起動セットがまだありません', en: 'No launch sets yet' },
  'set.emptyHint': { ja: 'いつも開くシェル・SSH 接続と、そこで実行するワークフローやコマンドをまとめて登録すると、ワンクリックで一括起動できます。', en: 'Register the shells and SSH connections you always open, plus the workflows and commands to run in them, and launch it all with one click.' },
  'set.emptyCta': { ja: '起動セットを登録', en: 'Add launch set' },
  'set.itemsSummary': { ja: '{count} 項目: {summary}', en: '{count} items: {summary}' },
  'set.hostMissing': { ja: '登録済みの SSH ホストが見つかりません(削除済みの可能性があります)', en: 'The registered SSH host was not found (it may have been deleted)' },
  'set.profileMissing': { ja: '登録済みのプロファイルが見つかりません(削除済みの可能性があります)', en: 'The registered profile was not found (it may have been deleted)' },
  'set.launched': { ja: '「{name}」を起動しました', en: 'Launched “{name}”' },
  'set.noAutorun': { ja: '(ワークフローなし)', en: '(no workflow)' },
  'set.command.ph': { ja: 'コマンドを直接入力 (任意・{{名前}} 使用可)', en: 'Command to run directly (optional; {{name}} supported)' },
  'set.command.title': { ja: 'シェル起動後にそのまま実行するコマンド。ワークフローと併用した場合はワークフローの後に実行されます。', en: 'A command run as-is once the shell is up. When a workflow is also set, it runs after the workflow.' },
  'set.removeItem.title': { ja: 'この項目を削除', en: 'Remove this item' },
  'set.addItem': { ja: '項目を追加', en: 'Add item' },
  'set.editTitle.new': { ja: '起動セットを登録', en: 'Add Launch Set' },
  'set.editTitle.edit': { ja: '起動セットを編集', en: 'Edit Launch Set' },
  'set.section.itemsOrder': { ja: '起動する項目(上から順に開きます)', en: 'Items to launch (opened top to bottom)' },
  'set.field.items': { ja: '項目', en: 'Items' },
  'set.needOneItem': { ja: '少なくとも 1 つの項目を追加してください', en: 'Add at least one item' },
  'menu.localProfiles': { ja: 'ローカルプロファイル', en: 'Local profiles' },
  'menu.sshHosts': { ja: 'SSH ホスト', en: 'SSH hosts' },

  // Menus
  'menu.sshConnections': { ja: 'SSH 接続', en: 'SSH Connections' },
  'menu.launchSets': { ja: '起動セット', en: 'Launch Sets' },
  'menu.manageProfiles': { ja: '⚙ プロファイルを管理…', en: '⚙ Manage profiles…' },
  'menu.runWorkflow': { ja: 'ワークフローを実行', en: 'Run workflow' },
  'host.field.saveCreds': { ja: 'パスワード / パスフレーズを保存する', en: 'Save the password / passphrase' },
  'host.field.saveCreds.hint': { ja: '既定はオフです。オンにすると、接続が成功したときだけ入力した内容を設定ディレクトリの ssh-hosts.json に平文で保存し、次回以降は入力を省略します。設定の書き出しには含まれないため、書き出したファイルを共有しても漏れませんが、この PC の設定ディレクトリを読める人は読めます。オフに戻す、またはホストを削除すると保存済みの内容も消えます。', en: 'Off by default. When on, what you type is saved — only after the connection succeeds — in plain text in ssh-hosts.json in the config directory, and reused next time. Settings export leaves it out, so a shared export never carries it, but anyone who can read this machine\u0027s config directory can read it. Turning this off, or deleting the host, removes what was saved.' },
  'secret.saved': { ja: '保存済みの認証情報を使用します', en: 'Using saved credentials' },
  'settings.aiApiKey.savedPlaceholder': { ja: '保存済み(変更する場合のみ入力)', en: 'Saved — enter a new key only to replace it' },
  'settings.aiApiKey.clear': { ja: '保存済みのキーを削除する', en: 'Delete the saved key' },
  'sftp.upload.title': { ja: 'ファイルをアップロード (SFTP)', en: 'Upload a file (SFTP)' },
  'sftp.download.title': { ja: 'ファイルをダウンロード (SFTP)', en: 'Download a file (SFTP)' },
  'sftp.remotePath': { ja: 'リモートのパス', en: 'Remote path' },
  'sftp.remotePath.hint': { ja: '`~` と相対パスはサーバー側で解決されます(接続時のホームが基準)。既存のファイルは上書きされます。', en: '`~` and relative paths are resolved on the server (against the login home). An existing file is overwritten.' },
  'sftp.cancel': { ja: '中止', en: 'Cancel transfer' },
  'sftp.done': { ja: '転送しました({bytes}) — {path}', en: 'Transferred {bytes} — {path}' },
  'sftp.needSsh': { ja: 'ファイル転送は SSH スレッドでのみ利用できます', en: 'File transfer is only available on an SSH thread' },
  'menu.transfer': { ja: 'ファイル転送 (SFTP)', en: 'File transfer (SFTP)' },
  'menu.upload': { ja: 'ファイルをアップロード…', en: 'Upload files…' },
  'menu.uploadDir': { ja: 'フォルダをアップロード…', en: 'Upload a folder…' },
  'menu.download': { ja: 'リモートファイルを開く…', en: 'Browse remote files…' },
  'sftp.drop.title': { ja: 'アップロード先', en: 'Upload destination' },
  'sftp.drop.files': { ja: '{n} 件を転送します: {list}', en: 'Transferring {n} item(s): {list}' },
  'sftp.drop.dir': { ja: 'リモートのディレクトリ', en: 'Remote directory' },
  'sftp.drop.hint': { ja: 'フォルダはその名前でリモートに再作成されます(既存のディレクトリは再利用)。同名のファイルは上書きされます。`~` と相対パスはサーバー側で解決されます。', en: 'Folders are recreated under their own name (existing directories are reused). Files with the same name are overwritten. `~` and relative paths are resolved on the server.' },
  'sftp.drop.item': { ja: '{name}  →  {dir}', en: '{name}  →  {dir}' },
  'sftp.browser.title': { ja: 'リモートファイル (SFTP)', en: 'Remote files (SFTP)' },
  'sftp.browser.up': { ja: '↑ 上へ', en: '↑ Up' },
  'sftp.browser.upload': { ja: 'ここにアップロード', en: 'Upload here' },
  'sftp.browser.loading': { ja: '読み込み中…', en: 'Loading…' },
  'sftp.browser.empty': { ja: '(空のディレクトリ)', en: '(empty directory)' },
  'sftp.browser.count': { ja: '{n} 件 — ディレクトリをクリックで移動、ファイルをクリックでダウンロード', en: '{n} items — click a directory to enter it, a file to download it' },
  'common.close': { ja: '閉じる', en: 'Close' },
  'path.copy': { ja: 'パスをコピー', en: 'Copy path' },
  'path.copied': { ja: 'パスをコピーしました', en: 'Path copied' },
  'path.reveal': { ja: 'フォルダーで表示', en: 'Show in file manager' },
  'path.openTerminal': { ja: '外部ターミナルで開く', en: 'Open in external terminal' },
  'ctx.openTerminalCwd': { ja: '作業ディレクトリを外部ターミナルで開く', en: 'Open working directory in external terminal' },
  'path.remoteNote': { ja: 'リモートのパスです — この PC では開けません', en: 'Remote path — cannot be opened on this machine' },
  'menu.workflowsEmpty': { ja: 'ワークフローが未登録です', en: 'No workflows registered' },
  'menu.manageWorkflows': { ja: '⚙ ワークフローを管理…', en: '⚙ Manage workflows…' },
  'menu.registerWorkflow': { ja: '＋ ワークフローを登録…', en: '+ Add workflow…' },
  'menu.wfHint': { ja: '{desc} — Shift+クリックで実行せず挿入', en: '{desc} — Shift+click to insert without running' },

  // Palette
  'palette.aria': { ja: 'コマンドパレット', en: 'Command palette' },
  'palette.input.ph': { ja: 'コマンド・ワークフロー・SSH ホスト・履歴を検索…', en: 'Search commands, workflows, SSH hosts, history…' },
  'palette.results.aria': { ja: '検索結果', en: 'Results' },
  'palette.foot.select': { ja: '選択', en: 'Select' },
  'palette.foot.run': { ja: '実行', en: 'Run' },
  'palette.foot.insert': { ja: '挿入', en: 'Insert' },
  'palette.foot.close': { ja: '閉じる', en: 'Close' },
  'palette.foot.filters': { ja: '履歴の絞り込み: @ホスト  !failed / !ok  #ssh / #local', en: 'History filters: @host  !failed / !ok  #ssh / #local' },
  'palette.foot.save': { ja: 'ワークフローに保存', en: 'Save as workflow' },
  'block.save.title': { ja: 'このコマンドをワークフローとして保存', en: 'Save this command as a workflow' },
  'ctx.saveWorkflow': { ja: 'ワークフローとして保存: {cmd}', en: 'Save as workflow: {cmd}' },
  'wf.param.remember': { ja: '入力した値を記憶する (次回の既定値になります)', en: 'Remember these values (used as next time\'s defaults)' },
  'wf.param.remember.hint': { ja: '値はこの PC の設定ディレクトリに保存され、設定の書き出しには含まれません。チェックを外して実行すると記憶を消します。', en: 'Stored in this machine\'s config directory and left out of settings export. Run with the box unticked to forget them.' },
  'palette.newLocalThread': { ja: '新しいローカルスレッド', en: 'New local thread' },
  'palette.splitPane': { ja: 'ペインを上下分割', en: 'Split pane vertically' },
  'palette.splitPaneH': { ja: 'ペインを左右分割', en: 'Split pane horizontally' },
  'palette.unsplitPane': { ja: 'ペイン分割を解除', en: 'Unsplit pane' },
  'palette.broadcastOn': { ja: '一斉入力を有効化(全ペインへ送信)', en: 'Enable input broadcast (all panes)' },
  'palette.broadcastOff': { ja: '一斉入力を無効化', en: 'Disable input broadcast' },
  'toast.broadcastOn': { ja: '一斉入力: オン — 入力が表示中の全ペインへ送られます', en: 'Broadcast on — keystrokes go to every visible pane' },
  'toast.broadcastOff': { ja: '一斉入力: オフ', en: 'Broadcast off' },
  'toast.broadcastNeedsSplit': { ja: '一斉入力にはペイン分割が必要です(上下: Ctrl+Shift+D / 左右: Ctrl+Shift+E)', en: 'Broadcast needs a split view (Ctrl+Shift+D vertical / Ctrl+Shift+E horizontal)' },
  'palette.aiAssist': { ja: 'AI にコマンドを聞く', en: 'Ask AI for a command' },
  'palette.checkUpdate': { ja: 'アップデートを確認', en: 'Check for updates' },
  'update.unavailable': { ja: 'このビルドではアップデート機能が利用できません', en: 'Updates are not available in this build' },
  'update.checkFailed': { ja: 'アップデートの確認に失敗しました: {e}', en: 'Update check failed: {e}' },
  'update.none': { ja: 'お使いのバージョンは最新です', en: 'You are on the latest version' },
  'update.available': { ja: '新しいバージョン {version} が利用できます。今すぐダウンロードして適用しますか?', en: 'Version {version} is available. Download and install now?' },
  'update.install': { ja: 'アップデート', en: 'Update' },
  'update.downloading': { ja: 'アップデートをダウンロード中…', en: 'Downloading update…' },
  'update.installed': { ja: 'アップデートを適用しました。再起動して新しいバージョンに切り替えますか?', en: 'Update installed. Restart to switch to the new version?' },
  'update.relaunch': { ja: '再起動', en: 'Restart' },
  'update.failed': { ja: 'アップデートに失敗しました: {e}', en: 'Update failed: {e}' },
  'update.owner.msstore': { ja: 'Microsoft Store', en: 'the Microsoft Store' },
  'update.owner.macappstore': { ja: 'App Store', en: 'the App Store' },
  'update.owner.systemPackage': { ja: 'パッケージマネージャ(apt / dnf / Homebrew など)', en: 'your package manager (apt / dnf / Homebrew)' },
  'update.owner.development': { ja: '開発ビルド', en: 'this development build' },
  'update.managedElsewhere': { ja: 'このビルドのアップデートは {owner} が管理します', en: 'Updates for this build are managed by {owner}' },
  'update.openManager': { ja: 'このビルドのアップデートは {owner} が管理します。{owner} を開いて確認しますか?', en: 'Updates for this build are managed by {owner}. Open it to check?' },
  'update.openManager.ok': { ja: '開く', en: 'Open' },
  'settings.autoUpdate.managed': { ja: 'このビルドのアップデートは {owner} が管理するため、アプリ内での自動アップデートは行いません。', en: 'Updates for this build are managed by {owner}, so the app does not update itself.' },
  'settings.autoUpdate': { ja: '起動時にアップデートを確認', en: 'Check for updates on startup' },
  'settings.group.sessionLog': { ja: 'セッションログ', en: 'Session logs' },
  'settings.sessionLog.hint': { ja: 'ターミナルに流れた内容をファイルに残します (作業のエビデンス用)。SSH はホスト編集画面で「セッションを記録する」をオンにしたホストだけが対象です。日付ごとのフォルダに「ホスト名_時刻.log」として保存します。', en: 'Keeps what went through the terminal in a file, as a record of the work. SSH hosts are logged only when "Log sessions" is on in the host editor. Files go into a folder per day as "<host>_<time>.log".' },
  'settings.sessionLogDir': { ja: '保存先フォルダ (空欄 = 既定)', en: 'Folder (blank = default)' },
  'settings.sessionLog.open': { ja: 'フォルダを開く', en: 'Open folder' },
  'settings.sessionLog.openFailed': { ja: 'フォルダを開けませんでした: {e}', en: 'Could not open the folder: {e}' },
  'settings.sessionLogPlain': { ja: 'エスケープシーケンスを除き、読めるテキストとして保存', en: 'Strip escape sequences and save readable text' },
  'settings.sessionLogPlain.hint': { ja: 'オフにすると色や画面制御を含む生のバイト列をそのまま保存します (cat で端末に流すと再現できます)。', en: 'Off keeps the raw byte stream, colors and cursor control included (cat it to a terminal to replay).' },
  'settings.sessionLogLocal': { ja: 'ローカルシェルも記録する', en: 'Log local shells too' },
  'settings.autoUpdate.hint': { ja: 'GitHub Releases の署名付きアップデートを起動時に確認します。新しいバージョンがある時だけ通知します(適用するかは毎回確認)。', en: 'Checks GitHub Releases for a signed update at startup. Notifies only when a new version exists; installing always asks first.' },
  'ai.modal.title': { ja: 'AI コマンド支援', en: 'AI command assist' },
  'ai.privacyNote': { ja: '送信されるのはこの質問と OS/シェル名のみです。ターミナルの内容・履歴は送信されません。', en: 'Only this question and your OS/shell name are sent. Terminal contents and history are never sent.' },
  'ai.query.label': { ja: 'やりたいことを説明してください', en: 'Describe what you want to do' },
  'ai.ask': { ja: 'AI に聞く', en: 'Ask AI' },
  'ai.asking': { ja: 'AI に問い合わせ中…', en: 'Asking AI…' },
  'ai.noKey': { ja: 'AI 支援を使うには、設定「AI コマンド支援」で Anthropic API キーを登録してください', en: 'To use AI assist, add your Anthropic API key in Settings > AI command assist' },
  'ai.refused': { ja: 'AI がこのリクエストへの回答を控えました。表現を変えてもう一度お試しください', en: 'The AI declined to answer this request. Try rephrasing it.' },
  'ai.failed': { ja: 'AI への問い合わせに失敗しました: {e}', en: 'AI request failed: {e}' },
  'ai.result.title': { ja: 'AI の提案', en: 'AI suggestion' },
  'ai.result.command': { ja: 'コマンド(編集できます)', en: 'Command (editable)' },
  'ai.result.insert': { ja: 'ターミナルへ挿入', en: 'Insert into terminal' },
  'ai.result.copy': { ja: 'コピー', en: 'Copy' },
  'ai.result.copied': { ja: 'コマンドをコピーしました', en: 'Command copied' },
  'ai.result.insertNote': { ja: '挿入では実行されません。ターミナルで内容を確認して Enter で実行してください。', en: 'Insert does not run the command. Review it in the terminal and press Enter to run.' },
  'settings.group.ai': { ja: 'AI コマンド支援', en: 'AI command assist' },
  'settings.aiApiKey': { ja: 'Anthropic API キー(空欄 = 無効)', en: 'Anthropic API key (empty = disabled)' },
  'settings.group.backup': { ja: '設定の書き出し / 取り込み', en: 'Export / import settings' },
  'settings.export': { ja: '書き出し', en: 'Export' },
  'settings.import': { ja: '取り込み', en: 'Import' },
  'settings.backup.hint': { ja: '設定・ワークフロー・SSH ホスト・端末プロファイル・起動セット・サーバー設定を 1 つの JSON にまとめます。別のマシンへ移すとき用です。取り込みは id 単位の上書きなので、同じファイルを 2 回取り込んでも増えません。', en: 'Bundles settings, workflows, SSH hosts, terminal profiles, launch sets and server presets into one JSON file, for moving to another machine. Import upserts by id, so importing the same file twice adds nothing.' },
  'settings.backup.secrets': { ja: 'API キーと SSH の保存済みパスワード / パスフレーズは書き出しに含まれません。移行先では入力し直しになりますが、そのぶん書き出したファイルを共有・バックアップしても資格情報は漏れません。', en: 'The API key and any saved SSH passwords / passphrases are left out. You re-enter them on the other machine — and in exchange the exported file can be shared or backed up without carrying credentials.' },
  'settings.export.done': { ja: '設定を書き出しました', en: 'Settings exported' },
  'settings.export.failed': { ja: '設定を書き出せませんでした: {e}', en: 'Could not export settings: {e}' },
  'settings.import.confirm': { ja: 'このファイルを取り込みますか?', en: 'Import this file?' },
  'settings.import.summary': { ja: 'ワークフロー {workflows} / SSH ホスト {sshHosts} / プロファイル {profiles} / 起動セット {launchSets} / サーバー設定 {devServers}', en: 'Workflows {workflows} · SSH hosts {sshHosts} · profiles {profiles} · launch sets {launchSets} · server presets {devServers}' },
  'settings.import.settingsToo': { ja: 'アプリ設定(外観・ターミナルなど)も上書きされます。', en: 'App settings (appearance, terminal, …) will be overwritten too.' },
  'settings.import.done': { ja: '設定を取り込みました', en: 'Settings imported' },
  'settings.import.failed': { ja: '設定を取り込めませんでした: {e}', en: 'Could not import settings: {e}' },
  'settings.aiApiKey.hint': { ja: 'あなた自身の API キーで Anthropic API を直接呼び出します。送信されるのは入力した質問と OS/シェル名のみで、ターミナルの内容や履歴は送信されません。キーはローカルの設定ファイルに保存されます。', en: 'Calls the Anthropic API directly with your own key. Only your typed question and OS/shell name are sent — never terminal contents or history. The key is stored in the local settings file.' },
  'settings.aiModel': { ja: 'モデル(空欄 = claude-opus-5)', en: 'Model (empty = claude-opus-5)' },
  'palette.registerWorkflow': { ja: 'ワークフローを登録', en: 'Add workflow' },
  'palette.registerHost': { ja: 'SSH ホストを登録', en: 'Add SSH host' },
  'palette.registerSet': { ja: '起動セットを登録', en: 'Add launch set' },
  'palette.reloadSettings': { ja: '設定ファイルを再読み込み', en: 'Reload settings file' },
  'palette.showSettingsFile': { ja: '設定ファイルをフォルダーで表示', en: 'Show settings file in file manager' },
  'palette.settingsReloaded': { ja: '設定ファイルを再読み込みしました', en: 'Reloaded the settings file' },
  'palette.settingsFileFailed': { ja: '設定ファイルを表示できませんでした: {e}', en: 'Could not show the settings file: {e}' },
  'paletteKeys.invalid': { ja: 'settings.json の palette_keys に解釈できないキーがあります(無視しました): {keys}', en: 'Ignored unrecognized entries in settings.json palette_keys: {keys}' },
  'palette.shortcutsList': { ja: 'キーボードショートカット一覧', en: 'Keyboard shortcuts' },
  'palette.newThreadOf': { ja: '新規スレッド: {name}', en: 'New thread: {name}' },
  'palette.showThread': { ja: '表示: {title}', en: 'Show: {title}' },
  'thread.kind.ssh': { ja: 'SSH スレッド', en: 'SSH thread' },
  'thread.kind.local': { ja: 'ローカルスレッド', en: 'Local thread' },
  'palette.sshOf': { ja: 'SSH: {name}', en: 'SSH: {name}' },
  'palette.launchSet': { ja: '起動セット: {name}', en: 'Launch set: {name}' },

  // Generic modal
  'modal.cancel': { ja: 'キャンセル', en: 'Cancel' },
  'modal.requiredFields': { ja: '必須項目を入力してください', en: 'Please fill in the required fields' },

  // Settings panel
  'panel.settings.title': { ja: '設定', en: 'Settings' },
  'panel.settings.desc': { ja: '外観・ターミナル・起動の設定。変更は「保存」で反映され、開いているスレッドにも即時適用されます。', en: 'Appearance, terminal, and startup settings. Changes take effect on “Save” and apply immediately to open threads.' },
  'settings.group.appearance': { ja: '外観', en: 'Appearance' },
  'settings.theme': { ja: 'テーマ', en: 'Theme' },
  'settings.theme.dark': { ja: 'ダーク', en: 'Dark' },
  'settings.theme.light': { ja: 'ライト', en: 'Light' },
  'settings.termTheme': { ja: 'ターミナルの配色', en: 'Terminal colors' },
  'settings.termTheme.hint': { ja: 'ターミナル本体の配色(背景・文字・ANSI 16 色)。アプリの UI テーマとは独立に選べます。変更は開いているスレッドに即時プレビューされ、「保存」で確定します。', en: 'The terminal\'s own palette (background, text, 16 ANSI colors), independent of the UI theme. Changes preview live in open threads and are kept on Save.' },
  'settings.termTheme.follow': { ja: 'UI テーマに従う(ダーク / ライト)', en: 'Follow UI theme (dark / light)' },
  'settings.termTheme.custom': { ja: 'カスタム…', en: 'Custom…' },
  'tc.group.base': { ja: '基本', en: 'Base' },
  'tc.group.normal': { ja: 'ANSI 0–7', en: 'ANSI 0–7' },
  'tc.group.bright': { ja: 'ANSI 8–15(明るい)', en: 'ANSI 8–15 (bright)' },
  'tc.copyFrom': { ja: 'プリセットからコピー:', en: 'Copy from preset:' },
  'tc.reset': { ja: 'UI テーマの配色に戻す', en: 'Reset to UI theme palette' },
  'tc.background': { ja: '背景', en: 'Background' },
  'tc.foreground': { ja: '文字', en: 'Foreground' },
  'tc.cursor': { ja: 'カーソル', en: 'Cursor' },
  'tc.black': { ja: '黒', en: 'Black' },
  'tc.red': { ja: '赤', en: 'Red' },
  'tc.green': { ja: '緑', en: 'Green' },
  'tc.yellow': { ja: '黄', en: 'Yellow' },
  'tc.blue': { ja: '青', en: 'Blue' },
  'tc.magenta': { ja: 'マゼンタ', en: 'Magenta' },
  'tc.cyan': { ja: 'シアン', en: 'Cyan' },
  'tc.white': { ja: '白', en: 'White' },
  'tc.brightBlack': { ja: '明るい黒', en: 'Bright black' },
  'tc.brightRed': { ja: '明るい赤', en: 'Bright red' },
  'tc.brightGreen': { ja: '明るい緑', en: 'Bright green' },
  'tc.brightYellow': { ja: '明るい黄', en: 'Bright yellow' },
  'tc.brightBlue': { ja: '明るい青', en: 'Bright blue' },
  'tc.brightMagenta': { ja: '明るいマゼンタ', en: 'Bright magenta' },
  'tc.brightCyan': { ja: '明るいシアン', en: 'Bright cyan' },
  'tc.brightWhite': { ja: '明るい白', en: 'Bright white' },
  'settings.language': { ja: '言語', en: 'Language' },
  'settings.fontSize': { ja: 'フォントサイズ', en: 'Font size' },
  'settings.fontFamily': { ja: 'フォントファミリー (空欄 = 既定)', en: 'Font family (blank = default)' },
  'settings.group.terminal': { ja: 'ターミナル', en: 'Terminal' },
  'settings.scrollback': { ja: 'スクロールバック行数', en: 'Scrollback lines' },
  'settings.restoreSession': { ja: '前回のスレッド構成を復元', en: 'Restore previous threads on startup' },
  'settings.restoreSession.hint': { ja: '起動時に、前回開いていたローカルスレッド(プロファイル構成と順序)を再作成します。SSH 接続は認証が必要なため自動再接続せず、SSH パネルや新規メニューから接続します。', en: 'Recreates the local threads (profiles and order) you had open last time. SSH connections are never auto-reconnected (they need credentials) — connect from the SSH panel or the new-thread menu.' },
  'restore.sshSkipped': { ja: 'SSH スレッド {count} 件は認証が必要なため復元されませんでした(SSH パネルから接続してください)', en: 'Skipped restoring {count} SSH threads — they need authentication (connect from the SSH panel)' },
  'settings.gpuRendering': { ja: 'GPU レンダリング(WebGL)', en: 'GPU rendering (WebGL)' },
  'settings.rightClick': { ja: '右クリックの動作', en: 'Right-click action' },
  'settings.rightClick.auto': { ja: '自動 (OS 標準)', en: 'Auto (OS default)' },
  'settings.rightClick.copyPaste': { ja: 'コピー / 貼り付け (Windows Terminal 方式)', en: 'Copy & paste (Windows Terminal style)' },
  'settings.rightClick.menu': { ja: 'メニューを表示', en: 'Show menu' },
  'settings.externalTerminal': { ja: '外部ターミナル (空欄 = OS 既定)', en: 'External terminal (empty = OS default)' },
  'settings.externalTerminal.hint': { ja: '「外部ターミナルで開く」で起動するコマンド。{dir} は開くフォルダに置き換わります(例: wt.exe -d {dir} / pwsh.exe / "C:\\Program Files\\Alacritty\\alacritty.exe" --working-directory {dir})。空欄の場合、Windows では既定のターミナルアプリ、macOS ではターミナル.app、Linux では x-terminal-emulator などを使います。', en: 'Command launched by "Open in external terminal". {dir} is replaced by the folder to open (e.g. wt.exe -d {dir} / pwsh.exe / "C:\\Program Files\\Alacritty\\alacritty.exe" --working-directory {dir}). When empty, Windows uses its default terminal app, macOS uses Terminal.app, and Linux tries x-terminal-emulator and friends.' },
  'settings.rightClick.hint': { ja: 'コピー / 貼り付けは、選択範囲があればコピー(選択は解除)、無ければ貼り付けます。Shift+右クリックでは、この設定にかかわらず常にワークフローのメニューを開きます。自動は Windows / Linux ではコピー / 貼り付け、macOS ではメニューです。', en: 'Copy & paste copies the selection (and clears it) or, with nothing selected, pastes. Shift+right-click always opens the workflow menu regardless of this setting. Auto means copy & paste on Windows / Linux and the menu on macOS.' },
  'ctx.copy': { ja: 'コピー', en: 'Copy' },
  'ctx.paste': { ja: '貼り付け', en: 'Paste' },
  'settings.gpuRendering.hint': { ja: '描画を GPU で高速化します。表示が乱れる環境ではオフにしてください(変更は開いているスレッドにも即時適用)。', en: 'Accelerates rendering with the GPU. Turn it off if the display glitches on your machine (applies immediately to open threads).' },
  'settings.shellIntegration': { ja: 'シェル統合を自動で有効化(コマンドブロック用)', en: 'Auto-enable shell integration (for command blocks)' },
  'settings.shellIntegration.hint': { ja: 'bash / zsh / fish / PowerShell (pwsh・Windows PowerShell) の起動時に OSC 133 のプロンプト連携と OSC 7 の作業ディレクトリ通知を自動で組み込みます。rc ファイルや $PROFILE は変更されません。', en: 'Injects OSC 133 prompt hooks and OSC 7 working-directory reports into bash / zsh / fish / PowerShell (pwsh and Windows PowerShell) at launch. Your rc files and $PROFILE are not modified.' },
  'settings.notifyLong': { ja: '長時間コマンドの完了をデスクトップ通知', en: 'Desktop notification when a long command finishes' },
  'settings.notifyLong.hint': { ja: '非表示のスレッド、またはウィンドウが非アクティブな時にコマンドが終わると OS の通知を出します(要シェル統合)。表示中のスレッドを見ている時は通知しません。', en: 'Sends an OS notification when a command finishes in a hidden thread or while the window is unfocused (needs shell integration). Watching the thread in the foreground never notifies.' },
  'settings.notifyThreshold': { ja: '通知する最短実行時間(秒)', en: 'Minimum duration to notify (seconds)' },
  'notify.done.ok': { ja: '✓ コマンド完了 — {thread}', en: '✓ Command finished — {thread}' },
  'notify.done.err': { ja: '✗ コマンド失敗 (exit {exit}) — {thread}', en: '✗ Command failed (exit {exit}) — {thread}' },
  'notify.done.body': { ja: '{command}\n所要時間 {elapsed}', en: '{command}\nTook {elapsed}' },
  'notify.command.unknown': { ja: '(コマンド)', en: '(command)' },
  'settings.group.startup': { ja: '起動', en: 'Startup' },
  'settings.defaultProfile': { ja: '既定のターミナルプロファイル', en: 'Default terminal profile' },
  'settings.save': { ja: '保存', en: 'Save' },
  'settings.dataHint': { ja: '設定・登録データは JSON ファイルとしてアプリ設定ディレクトリに保存されます。', en: 'Settings and saved data are stored as JSON files in the app config directory.' },
  'settings.loadFailed': { ja: '設定の読み込みに失敗、既定値を使用します: {e}', en: 'Failed to load settings; using defaults: {e}' },
  'settings.saved': { ja: '設定を保存しました', en: 'Settings saved' },

  // Clipboard / quit / paste confirmations
  'confirm.sessionsRunning': { ja: '{count} 個のセッションが実行中です。終了しますか?', en: '{count} sessions are running. Quit?' },
  'confirm.quit': { ja: '終了', en: 'Quit' },
  'toast.noActiveTerm': { ja: 'アクティブなターミナルがありません', en: 'No active terminal' },
  'toast.copied': { ja: 'コピーしました', en: 'Copied' },
  'toast.copyFailed': { ja: 'コピーに失敗: {e}', en: 'Failed to copy: {e}' },
  'toast.pasteFailed': { ja: '貼り付けに失敗: {e}', en: 'Failed to paste: {e}' },
  'confirm.pasteLines': { ja: '{lines} 行のテキストを貼り付けますか?', en: 'Paste {lines} lines of text?' },
  'confirm.paste': { ja: '貼り付け', en: 'Paste' },

  // Command blocks
  'block.foldedRows': { ja: '▸ {rows} 行を折りたたみ済み — クリックで展開', en: '▸ {rows} rows folded — click to expand' },
  'block.copy.title': { ja: 'ブロックをコピー', en: 'Copy block' },
  'block.fold.title': { ja: '出力を折りたたむ', en: 'Fold output' },
  'block.expand.title': { ja: '出力を展開する', en: 'Expand output' },
  'block.noOutput.title': { ja: '出力がありません', en: 'No output' },
  'block.chip.ok': { ja: 'コマンド成功 (exit 0)', en: 'Command succeeded (exit 0)' },
  'block.chip.err': { ja: 'コマンド失敗 (exit {exit})', en: 'Command failed (exit {exit})' },
  'block.copied': { ja: 'ブロックをコピーしました', en: 'Block copied' },

  // Keyboard shortcut cheat sheet
  'sc.title': { ja: 'キーボードショートカット', en: 'Keyboard Shortcuts' },
  'sc.close': { ja: '閉じる', en: 'Close' },
  'sc.group.general': { ja: '全般', en: 'General' },
  'sc.item.palette.custom': { ja: 'コマンドパレットを開く(settings.json の palette_keys で変更済み)', en: 'Open command palette (customized via palette_keys in settings.json)' },
  'sc.item.palette': { ja: 'コマンドパレットを開く(ターミナル外では {mod}+P も可)', en: 'Open command palette ({mod}+P also works outside the terminal)' },
  'sc.item.newThread': { ja: '新しいローカルスレッド', en: 'New local thread' },
  'sc.item.closeThread': { ja: '表示中のスレッドを終了', en: 'Close the visible thread' },
  'sc.item.shortcuts': { ja: 'このショートカット一覧', en: 'This shortcut list' },
  'sc.group.pane': { ja: 'ペイン', en: 'Panes' },
  'sc.item.split': { ja: '上下に分割 / 分割を解除', en: 'Split vertically / unsplit' },
  'sc.item.splitH': { ja: '左右に分割 / 分割を解除', en: 'Split horizontally / unsplit' },
  'sc.item.broadcast': { ja: '一斉入力の切替(全ペインへ送信)', en: 'Toggle input broadcast (all panes)' },
  'sc.item.focusUp': { ja: '上(左右分割時は左)のペインにフォーカス', en: 'Focus the upper (or left) pane' },
  'sc.item.focusDown': { ja: '下(左右分割時は右)のペインにフォーカス', en: 'Focus the lower (or right) pane' },
  'sc.group.terminal': { ja: 'ターミナル', en: 'Terminal' },
  'sc.item.copy': { ja: '選択範囲をコピー', en: 'Copy selection' },
  'sc.item.paste': { ja: 'クリップボードを貼り付け', en: 'Paste clipboard' },
  'sc.item.copyWin': { ja: '選択範囲をコピー (選択がある時。無ければ Ctrl+C は中断 SIGINT)', en: 'Copy selection (with a selection; otherwise Ctrl+C is SIGINT)' },
  'sc.item.pasteAlt': { ja: 'クリップボードを貼り付け (別のキー)', en: 'Paste clipboard (alternate keys)' },
  'sc.item.copyAlt': { ja: '選択範囲をコピー (別のキー)', en: 'Copy selection (alternate keys)' },
  'sc.item.rightClickCopyPaste': { ja: '選択があればコピー、無ければ貼り付け (Shift+右クリックでメニュー)', en: 'Copy the selection, or paste when none (Shift+right-click for the menu)' },
  'sc.item.find': { ja: 'ターミナル内を検索', en: 'Search in terminal' },
  'sc.item.prevBlock': { ja: '前のコマンドブロックへ (要シェル統合)', en: 'Previous command block (needs shell integration)' },
  'sc.item.nextBlock': { ja: '次のコマンドブロックへ (要シェル統合)', en: 'Next command block (needs shell integration)' },
  'sc.group.workflows': { ja: 'ワークフロー', en: 'Workflows' },
  'sc.key.anyKey': { ja: '任意のキー', en: 'Any key' },
  'sc.item.wfShortcut': { ja: 'ワークフロー編集画面で登録したショートカットで実行', en: 'Run via the shortcut set in the workflow editor' },
  'sc.key.rightClick': { ja: '右クリック', en: 'Right-click' },
  'sc.item.rightClick': { ja: 'ターミナル上のメニューから実行 (Shift+クリックで挿入のみ)', en: 'Run from the terminal menu (Shift+click to insert only)' },

  // Boot
  'boot.error': { ja: '起動処理でエラー: {e}', en: 'Startup error: {e}' },
};

/** Translate `key` for the current language, filling `{name}` placeholders
 * from `params`. Falls back to Japanese, then the raw key. */
function tr(key, params) {
  const lang = state.settings.language === 'en' ? 'en' : 'ja';
  const entry = STRINGS[key];
  let s = entry ? (entry[lang] ?? entry.ja) : key;
  if (params) {
    for (const k in params) s = s.split(`{${k}}`).join(String(params[k]));
  }
  return s;
}

/** Populates every static element carrying an i18n attribute. Called at boot
 * and whenever the language changes. */
function applyStaticI18n() {
  document.documentElement.lang = state.settings.language === 'en' ? 'en' : 'ja';
  document.querySelectorAll('[data-i18n]').forEach((el) => { el.textContent = tr(el.dataset.i18n); });
  document.querySelectorAll('[data-i18n-ph]').forEach((el) => { el.placeholder = tr(el.dataset.i18nPh); });
  // `{key}` is the (possibly customized) palette-open key; no other title uses it.
  document.querySelectorAll('[data-i18n-title]').forEach((el) => { el.title = tr(el.dataset.i18nTitle, { key: paletteKeyLabel('open') }); });
  document.querySelectorAll('[data-i18n-aria]').forEach((el) => { el.setAttribute('aria-label', tr(el.dataset.i18nAria)); });
}

/** Re-applies the language across the whole UI: static markup plus every
 * data-driven view (rendered from state, so no reload needed). */
function applyLanguage() {
  applyStaticI18n();
  updateSplitUi();
  renderThreads();
  renderWorkflows();
  renderQuickbar();
  renderHosts();
  renderProfiles();
  renderLaunchSets();
  renderTasks();
  renderServers();
  renderProfileSettingOptions();
  applyAutoUpdateAvailability();
  // Terminal color scheme controls carry translated labels; rebuild them
  // keeping whatever the user has picked / edited so far.
  renderTermThemeOptions();
  {
    const editor = $('#term-colors-editor');
    const live = editor.querySelector('input[data-tc-key]') ? readTermColorEditor() : (state.settings.term_colors || {});
    renderTermColorEditor(live);
  }
  renderPaletteKeyUi();
  if (!$('#shortcuts').classList.contains('hidden')) renderShortcuts();
  if (palette.open) updatePalette();
}

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

/* Built-in terminal palettes, selectable independently of the UI theme.
 * Keys follow xterm's ITheme; cursorAccent / selectionBackground are derived
 * in `buildTermTheme` so each preset only lists the visible colors. */
const TERM_PRESETS = {
  'solarized-dark': { name: 'Solarized Dark', colors: {
    background: '#002b36', foreground: '#839496', cursor: '#93a1a1',
    black: '#073642', red: '#dc322f', green: '#859900', yellow: '#b58900',
    blue: '#268bd2', magenta: '#d33682', cyan: '#2aa198', white: '#eee8d5',
    brightBlack: '#586e75', brightRed: '#cb4b16', brightGreen: '#586e75',
    brightYellow: '#657b83', brightBlue: '#839496', brightMagenta: '#6c71c4',
    brightCyan: '#93a1a1', brightWhite: '#fdf6e3' } },
  'solarized-light': { name: 'Solarized Light', colors: {
    background: '#fdf6e3', foreground: '#657b83', cursor: '#586e75',
    black: '#073642', red: '#dc322f', green: '#859900', yellow: '#b58900',
    blue: '#268bd2', magenta: '#d33682', cyan: '#2aa198', white: '#eee8d5',
    brightBlack: '#002b36', brightRed: '#cb4b16', brightGreen: '#586e75',
    brightYellow: '#657b83', brightBlue: '#839496', brightMagenta: '#6c71c4',
    brightCyan: '#93a1a1', brightWhite: '#fdf6e3' } },
  dracula: { name: 'Dracula', colors: {
    background: '#282a36', foreground: '#f8f8f2', cursor: '#f8f8f2',
    black: '#21222c', red: '#ff5555', green: '#50fa7b', yellow: '#f1fa8c',
    blue: '#bd93f9', magenta: '#ff79c6', cyan: '#8be9fd', white: '#f8f8f2',
    brightBlack: '#6272a4', brightRed: '#ff6e6e', brightGreen: '#69ff94',
    brightYellow: '#ffffa5', brightBlue: '#d6acff', brightMagenta: '#ff92df',
    brightCyan: '#a4ffff', brightWhite: '#ffffff' } },
  nord: { name: 'Nord', colors: {
    background: '#2e3440', foreground: '#d8dee9', cursor: '#d8dee9',
    black: '#3b4252', red: '#bf616a', green: '#a3be8c', yellow: '#ebcb8b',
    blue: '#81a1c1', magenta: '#b48ead', cyan: '#88c0d0', white: '#e5e9f0',
    brightBlack: '#4c566a', brightRed: '#bf616a', brightGreen: '#a3be8c',
    brightYellow: '#ebcb8b', brightBlue: '#81a1c1', brightMagenta: '#b48ead',
    brightCyan: '#8fbcbb', brightWhite: '#eceff4' } },
  'gruvbox-dark': { name: 'Gruvbox Dark', colors: {
    background: '#282828', foreground: '#ebdbb2', cursor: '#ebdbb2',
    black: '#282828', red: '#cc241d', green: '#98971a', yellow: '#d79921',
    blue: '#458588', magenta: '#b16286', cyan: '#689d6a', white: '#a89984',
    brightBlack: '#928374', brightRed: '#fb4934', brightGreen: '#b8bb26',
    brightYellow: '#fabd2f', brightBlue: '#83a598', brightMagenta: '#d3869b',
    brightCyan: '#8ec07c', brightWhite: '#ebdbb2' } },
  'one-dark': { name: 'One Dark', colors: {
    background: '#282c34', foreground: '#abb2bf', cursor: '#528bff',
    black: '#282c34', red: '#e06c75', green: '#98c379', yellow: '#e5c07b',
    blue: '#61afef', magenta: '#c678dd', cyan: '#56b6c2', white: '#abb2bf',
    brightBlack: '#5c6370', brightRed: '#e06c75', brightGreen: '#98c379',
    brightYellow: '#e5c07b', brightBlue: '#61afef', brightMagenta: '#c678dd',
    brightCyan: '#56b6c2', brightWhite: '#ffffff' } },
  monokai: { name: 'Monokai', colors: {
    background: '#272822', foreground: '#f8f8f2', cursor: '#f8f8f0',
    black: '#272822', red: '#f92672', green: '#a6e22e', yellow: '#f4bf75',
    blue: '#66d9ef', magenta: '#ae81ff', cyan: '#a1efe4', white: '#f8f8f2',
    brightBlack: '#75715e', brightRed: '#f92672', brightGreen: '#a6e22e',
    brightYellow: '#f4bf75', brightBlue: '#66d9ef', brightMagenta: '#ae81ff',
    brightCyan: '#a1efe4', brightWhite: '#f9f8f5' } },
  'tokyo-night': { name: 'Tokyo Night', colors: {
    background: '#1a1b26', foreground: '#c0caf5', cursor: '#c0caf5',
    black: '#15161e', red: '#f7768e', green: '#9ece6a', yellow: '#e0af68',
    blue: '#7aa2f7', magenta: '#bb9af7', cyan: '#7dcfff', white: '#a9b1d6',
    brightBlack: '#414868', brightRed: '#f7768e', brightGreen: '#9ece6a',
    brightYellow: '#e0af68', brightBlue: '#7aa2f7', brightMagenta: '#bb9af7',
    brightCyan: '#7dcfff', brightWhite: '#c0caf5' } },
  'catppuccin-mocha': { name: 'Catppuccin Mocha', colors: {
    background: '#1e1e2e', foreground: '#cdd6f4', cursor: '#f5e0dc',
    black: '#45475a', red: '#f38ba8', green: '#a6e3a1', yellow: '#f9e2af',
    blue: '#89b4fa', magenta: '#f5c2e7', cyan: '#94e2d5', white: '#bac2de',
    brightBlack: '#585b70', brightRed: '#f38ba8', brightGreen: '#a6e3a1',
    brightYellow: '#f9e2af', brightBlue: '#89b4fa', brightMagenta: '#f5c2e7',
    brightCyan: '#94e2d5', brightWhite: '#a6adc8' } },
};

/** The editable color slots, in display order, with their i18n labels. */
const TERM_COLOR_KEYS = [
  ['background', 'tc.background'], ['foreground', 'tc.foreground'], ['cursor', 'tc.cursor'],
  ['black', 'tc.black'], ['red', 'tc.red'], ['green', 'tc.green'], ['yellow', 'tc.yellow'],
  ['blue', 'tc.blue'], ['magenta', 'tc.magenta'], ['cyan', 'tc.cyan'], ['white', 'tc.white'],
  ['brightBlack', 'tc.brightBlack'], ['brightRed', 'tc.brightRed'], ['brightGreen', 'tc.brightGreen'],
  ['brightYellow', 'tc.brightYellow'], ['brightBlue', 'tc.brightBlue'], ['brightMagenta', 'tc.brightMagenta'],
  ['brightCyan', 'tc.brightCyan'], ['brightWhite', 'tc.brightWhite'],
];

function hexToRgba(hex, alpha) {
  const m = /^#?([0-9a-f]{6})$/i.exec(hex || '');
  if (!m) return hex;
  const n = parseInt(m[1], 16);
  return `rgba(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255}, ${alpha})`;
}

/** Completes a palette (bg/fg/cursor + ANSI 16) into a full xterm theme:
 * missing slots come from the UI theme's palette, and the two derived
 * colors (cursor accent, selection) follow background / cursor. */
function buildTermTheme(colors) {
  const base = TERM_THEMES[state.settings.theme] || TERM_THEMES.dark;
  const out = { ...base };
  for (const [key] of TERM_COLOR_KEYS) {
    const v = colors && colors[key];
    if (typeof v === 'string' && /^#[0-9a-f]{6}$/i.test(v)) out[key] = v;
  }
  out.cursorAccent = out.background;
  out.selectionBackground = hexToRgba(out.cursor, 0.25);
  return out;
}

/** Resolves a term_theme id + custom colors to an xterm theme. Unknown ids
 * (a preset removed in a later version) follow the UI theme. */
function resolveTermTheme(id, custom) {
  if (id === 'custom') return buildTermTheme(custom || {});
  if (id && TERM_PRESETS[id]) return buildTermTheme(TERM_PRESETS[id].colors);
  return TERM_THEMES[state.settings.theme] || TERM_THEMES.dark;
}

function currentTermTheme() {
  return resolveTermTheme(state.settings.term_theme, state.settings.term_colors);
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

/* The backend registers a session (and starts pumping its output) before
 * `create_local_session` returns, but the frontend only learns the id when that
 * invoke resolves. Events emitted inside that window are queued ahead of the
 * invoke's own reply, so their callbacks run first — with no thread to write
 * to. Park them here instead of dropping them, which is what used to silently
 * eat a shell's banner or its first prompt (and, for a shell that died in the
 * window, left a thread nothing would ever close).
 *
 * Bounded: a session whose `createThread` never happens must not accumulate
 * output forever. */
const preCreate = new Map(); // session id -> { chunks: Uint8Array[], size, exit: number|null }
const PRE_CREATE_LIMIT = 256 * 1024;

function preCreateEntry(id) {
  let e = preCreate.get(id);
  if (!e) {
    e = { chunks: [], size: 0, exit: null };
    preCreate.set(id, e);
  }
  return e;
}

/** Replays anything that arrived for `id` before its thread existed, and
 * reports whether the session had already exited by then. */
function drainPreCreate(id, term) {
  const e = preCreate.get(id);
  preCreate.delete(id);
  if (!e) return { exited: null };
  for (const chunk of e.chunks) term.write(chunk);
  return { exited: e.exit };
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
    // The profile's name becomes the thread name (see createThread's
    // `origin.title`); the backend already echoes it as `info.title`, but
    // only an explicit title survives the shell's first OSC title.
    const p = resolveProfile(profileId);
    const name = p && typeof p.name === 'string' ? p.name.trim() : '';
    createThread(info, idx >= 0 ? idx : state.focusedPane,
      name ? { profileId: profileId || '', title: name } : { profileId: profileId || '' });
  } catch (e) {
    toast(tr('toast.shellStartFailed', { e }), true);
  }
}

/** Resolves a host's ProxyJump chain to concrete host objects in connection
 * order `[jump1, .., jumpN, target]`. Mirrors the backend's
 * `resolve_jump_chain` (the backend re-validates authoritatively); this side
 * only needs the ordered hosts to prompt for per-hop credentials. Returns
 * `{ hosts }` or `{ error }`. */
function resolveChainHosts(target) {
  const hosts = [];
  for (const id of target.jump_hosts || []) {
    if (id === target.id) return { error: tr('ssh.jump.selfRef') };
    const hop = state.hosts.find((h) => h.id === id);
    if (!hop) return { error: tr('ssh.jump.missing') };
    hosts.push(hop);
  }
  hosts.push(target);
  return { hosts };
}

/** Parses a backend host-key error. The wire format is `\x1f`-delimited
 * (unit-separator — impossible inside a hostname or fingerprint, so it's a
 * safe field separator even for IPv6 hosts) led by a tag. Returns a typed
 * object or null for non-host-key errors. */
function parseHostKeyError(msg) {
  const US = '\x1f';
  if (!msg.includes(US)) return null;
  const p = msg.split(US);
  switch (p[0]) {
    case 'UNKNOWN_HOST_KEY':
      return { tag: p[0], hostId: p[1], host: p[2], port: p[3], keyType: p[4], fingerprint: p[5] };
    case 'HOST_KEY_MISMATCH':
      return { tag: p[0], hostId: p[1], host: p[2], port: p[3], line: p[4], keyType: p[5], fingerprint: p[6] };
    case 'FINGERPRINT_MISMATCH':
      return { tag: p[0], hostId: p[1], host: p[2], port: p[3], keyType: p[4], expected: p[5], actual: p[6] };
    default:
      return null;
  }
}

async function newSshThread(host) {
  const chain = resolveChainHosts(host);
  if (chain.error) { toast(chain.error, true); return; }
  // Hops that already have saved credentials are not asked for. Only the
  // *existence* of each is reported here — the backend fills the secrets in
  // itself, so a saved password never enters this page.
  let saved = {};
  const optedIn = chain.hosts.filter((h) => h.save_credentials).map((h) => h.id);
  if (optedIn.length) {
    try {
      saved = await invoke('ssh_saved_credentials', { hostIds: optedIn });
    } catch (e) {
      saved = {}; // if we cannot tell, ask — the same as having none saved
    }
  }
  const secretsByHost = await promptChainSecrets(chain.hosts, saved);
  if (secretsByHost === null) return; // cancelled
  const { cols, rows } = measureCells();
  await connectSsh(host, chain.hosts, secretsByHost, cols, rows, {});
}

/** Connects to `target` (possibly through the jump hosts in `chainHosts`),
 * and on an "unknown host key" (TOFU) rejection for any hop, offers to trust
 * that specific hop and retries.
 *
 * `fingerprints` is a map of host id → the SHA256 the user approved for that
 * hop. It starts empty and accumulates one entry per approved hop across
 * retries. The approved string is captured straight from the parsed error —
 * never re-derived — and the backend records/accepts a hop's key only if the
 * key it sees on the retry has that exact fingerprint, which is what stops a
 * MITM (at any hop) from relaying the real key on the first handshake and
 * substituting its own on the second. Keying by host id means each hop is
 * approved independently.
 *
 * A `FINGERPRINT_MISMATCH` / `HOST_KEY_MISMATCH` is a fail-closed rejection:
 * warn and stop — no retry loop, so an attacker gets no repeated attempts.
 * The `!(hostId in fingerprints)` guard likewise prevents a re-reported hop
 * from looping. */
async function connectSsh(target, chainHosts, secretsByHost, cols, rows, fingerprints) {
  toast(tr('toast.connecting', { name: target.name || target.host }));
  try {
    const info = await invoke('create_ssh_session', {
      hostId: target.id,
      secrets: secretsByHost,
      fingerprints,
      cols, rows,
    });
    createThread(info, state.focusedPane, { sshHostId: target.id });
    toast(tr('toast.connected'));
  } catch (e) {
    const err = parseHostKeyError(String(e));
    if (err && (err.tag === 'FINGERPRINT_MISMATCH' || err.tag === 'HOST_KEY_MISMATCH')) {
      // Fail closed: no automatic retry. A HOST_KEY_MISMATCH names the
      // known_hosts line it clashed with, so the one recovery that is
      // legitimate — the server's key really did change — is offered as an
      // explicit, explained step: remove that record, then reconnect as a
      // first contact (which runs the TOFU prompt with the new fingerprint).
      const hop = chainHosts.find((h) => h.id === err.hostId);
      const label = (hop && (hop.name || hop.host)) || err.host;
      if (err.tag === 'HOST_KEY_MISMATCH' && /^\d+$/.test(err.line)
          && await confirmRemoveHostKey(label, err)) {
        try {
          await invoke('remove_known_hosts', { lines: [parseInt(err.line, 10)] });
        } catch (e2) {
          toast(String(e2), true);
          return;
        }
        // Fresh fingerprints map: nothing from the failed attempt carries over.
        await connectSsh(target, chainHosts, secretsByHost, cols, rows, {});
        return;
      }
      toast(tr('ssh.mitmAborted', { host: label }), true);
      return;
    }
    if (err && err.tag === 'UNKNOWN_HOST_KEY' && !(err.hostId in fingerprints)) {
      const hop = chainHosts.find((h) => h.id === err.hostId) || target;
      if (await confirmTrustHost(hop, err.keyType, err.fingerprint)) {
        await connectSsh(target, chainHosts, secretsByHost, cols, rows, {
          ...fingerprints,
          [err.hostId]: err.fingerprint,
        });
      } else {
        toast(tr('ssh.unknownAborted'), true);
      }
      return;
    }
    toast(tr('toast.sshFailed', { msg: String(e) }), true);
  }
}

/** Asks whether to drop the known_hosts record a connection just clashed
 * with. Cancel is the safe answer and the wording says why. */
function confirmRemoveHostKey(host, err) {
  return new Promise((resolve) => {
    openModal({
      title: tr('ssh.mismatch.title', { host }),
      okLabel: tr('ssh.mismatch.remove'),
      body: [fieldNote(tr('ssh.mismatch.body', { line: err.line, keyType: err.keyType, fingerprint: err.fingerprint }))],
      onOk: () => resolve(true),
      onCancel: () => resolve(false),
    });
  });
}

/** TOFU prompt shown the first time we connect to a host whose key isn't yet
 * recorded in known_hosts. Approving threads the exact fingerprint the user
 * saw back into the retried connection as `expectedFingerprint`, so the
 * backend only trusts a key that provably matches what was approved. */
function confirmTrustHost(host, keyType, fingerprint) {
  return new Promise((resolve) => {
    openModal({
      title: tr('ssh.trustPrompt', {
        host: host.name || host.host, port: host.port, keyType, fingerprint,
      }),
      okLabel: tr('ssh.trustConnect'),
      body: [],
      onOk: () => resolve(true),
      onCancel: () => resolve(false),
    });
  });
}

// cmd.exe (and other console apps that never call `title`) report their own
// full exe path as the window title (e.g. "C:\Windows\System32\cmd.exe") —
// shorten that to just the exe name so thread tabs stay compact. Unix shell
// titles use forward slashes and never hit this (no backslash), so they pass
// through unchanged.
function shortenExeTitle(title) {
  const idx = title.lastIndexOf('\\');
  return idx >= 0 ? title.slice(idx + 1) : title;
}

function createThread(info, paneIdx, origin = {}) {
  // Logging was asked for but the file could not be opened: the session is
  // fine, the record is not, and that is worth one line.
  if (info.log_error) toast(tr('toast.logFailed', { e: info.log_error }), true);
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
  // Absolute filesystem paths get their own links, whose click opens a menu
  // (copy / reveal) instead of acting — see `registerPathLinks`.
  registerPathLinks(term, info.kind === 'ssh');
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
    // Same for a palette-open key from settings.json (it need not be one of
    // the Ctrl+Shift keys listed below).
    if (ev.type === 'keydown' && paletteKeyIs('open', ev)) return false;
    // Windows Terminal's plain clipboard keys (Ctrl+V / Ctrl+C with a
    // selection / Shift+Insert / Ctrl+Insert): bubble to the window handler,
    // which does the copy/paste and preventDefaults the native paste event.
    if (winClipboardAction(ev, term)) return false;
    const key = ev.key.toLowerCase();
    // Ctrl+Alt+↑/↓ = command-block navigation; bubble to the window handler.
    if (ev.ctrlKey && ev.altKey && (key === 'arrowup' || key === 'arrowdown')) return false;
    const mod = (ev.ctrlKey || ev.metaKey) && ev.shiftKey;
    if (!mod) return true;
    // Ctrl+Shift+C only leaves the terminal (for the app to copy) when there
    // is a selection; with nothing selected it's commonly a shell shortcut
    // (e.g. SIGINT-like copy fallback), so let it through untouched.
    if (key === 'c') return !term.hasSelection();
    return !['p', 't', 'w', 'd', 'e', 'b', 'v', 'f', 'arrowup', 'arrowdown', '/', '?'].includes(key);
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
        // Wall-clock start of the command, for the completion notification
        // and the history's duration.
        if (!b.startedAt) b.startedAt = Date.now();
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
          // The thread object is built after the handlers (see the OSC 7
          // note below), so a prompt replayed during setup has none yet —
          // the command is still recorded, just without a host label.
          recordBlockCommand(term, b, threadById(sessionId), info.kind, exit);
          maybeNotifyBlockDone(sessionId, term, b, exit);
        }
      }
    }
    return true;
  });

  // OSC 7 (`\e]7;file://host/path\a`) is how the shell reports its working
  // directory after every prompt. The task panel needs it to know which
  // manifests to scan. Registered here rather than reading the raw PTY chunks
  // because xterm's parser reassembles sequences split across reads.
  //
  // `cwd` is a plain local instead of a `thread.cwd` write because the thread
  // object is built below, *after* drainPreCreate() — a replayed prompt would
  // otherwise hit the temporal dead zone. The final value is folded into the
  // literal, and later reports go through threadById().
  let cwd = '';
  term.parser.registerOscHandler(7, (payload) => {
    const dir = parseOsc7(payload);
    if (dir && dir !== cwd) {
      cwd = dir;
      const t = threadById(sessionId);
      if (t) {
        t.cwd = dir;
        scheduleTaskRefresh();
      }
    }
    return true;
  });

  // Replay whatever the backend emitted while this thread was being built —
  // after the OSC 133 handler above is registered, so an early prompt still
  // opens its command block.
  const { exited } = drainPreCreate(info.id, term);

  // Broadcast fans keystrokes out to every thread currently shown in a pane.
  // It only applies to input typed in a *visible* pane — background threads
  // keep their input private — and panes never show the same thread twice
  // (assignThread swaps), so the Set is just belt-and-braces dedupe.
  // The backend session behind this terminal. A `let`, because an SSH thread
  // that reconnects keeps its terminal and gets a *new* session: everything
  // below that names the session goes through this binding, not info.id, so
  // keystrokes and resizes follow the reconnect instead of going to a session
  // that no longer exists.
  let sessionId = info.id;
  term.onData((data) => {
    // Between a drop and the reconnect there is nothing to type into.
    if (threadById(sessionId)?.reconnect) return;
    if (state.broadcast && state.panes.length >= 2
        && state.panes.some((p) => p.threadId === sessionId)) {
      for (const id of new Set(state.panes.map((p) => p.threadId).filter(Boolean))) {
        invoke('session_write', { id, data }).catch(() => {});
      }
    } else {
      invoke('session_write', { id: sessionId, data }).catch(() => {});
    }
  });
  term.onResize(({ cols, rows }) =>
    invoke('session_resize', { id: sessionId, cols, rows }).catch(() => {}));

  const thread = {
    id: info.id, title: origin.title || info.title, kind: info.kind, pinned: false,
    // A launch title (the profile name) is pinned like a user rename: shells
    // emit OSC 0/2 titles ("user@host: ~", the exe path on cmd.exe) right
    // away, which would otherwise overwrite the profile name.
    term, fit, hostEl, search, customTitle: !!origin.title, activity: false, blocks,
    gl: null, // WebGL addon handle when GPU rendering is active
    // Last directory reported over OSC 7 (local shells with integration on).
    // Empty means "unknown" — the task panel says so rather than guessing.
    cwd,
    // What this thread was launched from, for the session snapshot.
    profileId: origin.profileId || '', sshHostId: origin.sshHostId || '',
    logPath: info.log_path || '',
    // Points this terminal at a new backend session (see sessionId above).
    rebind(newId) { sessionId = newId; this.id = newId; },
    // In-progress automatic reconnect, or null (see scheduleReconnect).
    reconnect: null,
  };
  // OSC-2 title changes (`\e]0;...\a`) rename the thread automatically,
  // unless the user gave it a custom name (task 2).
  term.onTitleChange((title) => {
    if (thread.customTitle || !title) return;
    thread.title = shortenExeTitle(title);
    renderThreads();
  });
  state.threads.push(thread);
  assignThread(paneIdx, info.id);
  // GPU rendering, applied after assignThread() has attached the host to a
  // visible pane (see loadWebgl for why the timing and fallbacks matter).
  applyGpuRendering();
  persistSessionSnapshot();
  // A new thread implies wanting to use it — surface the shell view (a tool
  // panel may be covering the terminal).
  if (currentView !== 'shell') setView('shell');
  renderThreads();
  // The session had already ended before we got here (e.g. the profile's
  // executable failed to start). Its output has just been replayed above, so
  // run the normal exit path now instead of leaving an unclosable row.
  if (exited !== null) handleSessionExit(thread, exited);
}

async function closeThread(id, { kill = true } = {}) {
  const idx = state.threads.findIndex((t) => t.id === id);
  if (idx < 0) return;
  // Guard pinned threads against accidental user-initiated closes. A natural
  // shell exit (kill:false) always removes the thread without prompting.
  if (kill && state.threads[idx].pinned) {
    if (!(await confirmModal(tr('thread.confirmClosePinned', { title: state.threads[idx].title })))) return;
  }
  const [thread] = state.threads.splice(idx, 1);
  if (thread.reconnect) {
    clearTimeout(thread.reconnect.timer);
    thread.reconnect = null;
  }
  if (kill) invoke('session_kill', { id }).catch(() => {});
  thread.term.dispose();
  thread.hostEl.remove();
  persistSessionSnapshot();
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

// Suppresses snapshot writes while boot() is itself recreating threads from
// the snapshot — otherwise a crash mid-restore would truncate the layout.
let restoringSession = false;
let snapshotTimer = null;

/** Persists the current thread layout (debounced) so the next launch can
 * restore it. Stored per thread: the profile it was launched from, or the
 * SSH host it was connected to. */
function persistSessionSnapshot() {
  if (restoringSession) return;
  clearTimeout(snapshotTimer);
  snapshotTimer = setTimeout(() => {
    const threads = state.threads.map((t) => ({
      profile_id: t.kind === 'ssh' ? '' : (t.profileId || ''),
      ssh_host_id: t.kind === 'ssh' ? (t.sshHostId || '') : '',
    }));
    invoke('save_session_snapshot', { snapshot: { threads } }).catch(() => {});
  }, 300);
}

/** Enables GPU-accelerated rendering (xterm WebGL addon) on a thread.
 * Falls back silently to the default renderer when WebGL isn't available
 * (old GPUs, driver blocklists, remote desktops); on a lost context the
 * addon is disposed so xterm falls back instead of leaving a frozen canvas.
 * No-op when already active or when the addon script didn't load. */
function loadWebgl(thread) {
  if (thread.gl || typeof WebglAddon === 'undefined') return;
  try {
    const gl = new WebglAddon.WebglAddon();
    gl.onContextLoss(() => {
      gl.dispose();
      thread.gl = null;
    });
    thread.term.loadAddon(gl);
    thread.gl = gl;
  } catch (_e) { /* keep the default renderer */ }
}

function unloadWebgl(thread) {
  thread.gl?.dispose();
  thread.gl = null;
}

/** Gives the WebGL renderer to the threads a pane is actually showing, and
 * takes it away from every other thread.
 *
 * Browsers cap how many WebGL contexts may be live at once (Chromium: 16,
 * measured by `node scripts/term_bench.mjs --contexts`) and drop the OLDEST
 * one past the cap. Loading the addon per thread therefore meant that opening
 * enough threads silently knocked GPU rendering off the ones opened first —
 * and the thread you were looking at could be one of them. Parked threads
 * gain nothing from a renderer anyway: their host element is detached from
 * the pane, so they paint nothing until they are shown again.
 *
 * Also the entry point for the gpu_rendering setting. */
function applyGpuRendering() {
  const visible = new Set(state.panes.map((p) => p.threadId).filter(Boolean));
  const enabled = state.settings.gpu_rendering !== false;
  for (const t of state.threads) {
    if (enabled && visible.has(t.id)) loadWebgl(t);
    else unloadWebgl(t);
  }
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
        <button type="button" class="pane-thread-btn" title="${tr('pane.threadBtn.title')}">
          <span class="ptd-icon"></span>
          <span class="ptd-label"></span>
          <span class="ptd-caret">${icon('chevronDown')}</span>
        </button>
        <ul class="pane-thread-menu hidden" role="listbox"></ul>
      </div>
      <span class="pane-kind"></span>
      <div class="spacer"></div>
      <button class="pane-close icon-btn" title="${tr('pane.close.title')}">${icon('x')}</button>
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
    li.textContent = tr('thread.none');
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
      <p class="es-title">${tr('pane.emptyTitle')}</p>
      <p class="es-hint">${tr('pane.emptyHint')}</p>`;
    const btn = document.createElement('button');
    btn.className = 'accent-btn';
    btn.innerHTML = `${icon('plus')}<span>${tr('pane.newThread')}</span>`;
    btn.addEventListener('click', () => newLocalThread({ paneIdx }));
    empty.appendChild(btn);
    pane.body.appendChild(empty);
  }
  applyGpuRendering(); // the set of visible threads just changed
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
  scheduleTaskRefresh();
}

function focusPane(idx) {
  if (idx < 0 || idx >= state.panes.length) return;
  state.focusedPane = idx;
  state.panes.forEach((p, i) => p.root.classList.toggle('focused', i === idx));
  threadById(state.panes[idx].threadId)?.term.focus();
  renderThreads();
  scheduleTaskRefresh();
}

function addPane() {
  if (state.panes.length >= 2) return;
  const pane = createPane();
  const splitter = document.createElement('div');
  splitter.className = 'splitter';
  enableSplitterDrag(splitter);
  $('#pane-area').appendChild(splitter);
  $('#pane-area').appendChild(pane.root);
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
  $('#pane-area').querySelector('.splitter')?.remove();
  state.panes[0].root.style.flex = '';
  if (state.panes.length < 2 && state.broadcast) setBroadcast(false, { silent: true });
  focusPane(Math.min(state.focusedPane, state.panes.length - 1));
  applyGpuRendering(); // the closed pane's thread is parked; release its context
  updateSplitUi();
  renderThreads();
}

/** Re-orients an existing split without closing the second pane. The panes'
 * ResizeObservers refit each terminal once the flex direction flips. */
function setSplitDir(dir) {
  state.splitDir = dir;
  if (state.panes[0]) state.panes[0].root.style.flex = ''; // drop the old-axis size
  updateSplitUi();
}

/** Toggle for one direction: not split → split that way; split the same
 * way → unsplit; split the other way → just re-orient. */
function toggleSplitDir(dir) {
  // Reachable from the shortcut and the palette while a tool panel is up,
  // where the panes are hidden — splitting something the user cannot see is
  // not what they asked for. The buttons themselves only exist in the shell
  // view (see #shell-tools), so this is a no-op when one was clicked.
  if (currentView !== 'shell') setView('shell');
  if (state.panes.length < 2) {
    state.splitDir = dir;
    addPane();
  } else if (state.splitDir === dir) {
    removePane(1);
  } else {
    setSplitDir(dir);
  }
}

function setBroadcast(on, opts = {}) {
  state.broadcast = !!on;
  $('#terminals').classList.toggle('broadcast', state.broadcast);
  $('#broadcast-toggle').classList.toggle('active', state.broadcast);
  if (!opts.silent) toast(tr(state.broadcast ? 'toast.broadcastOn' : 'toast.broadcastOff'));
}

function toggleBroadcast() {
  if (currentView !== 'shell') setView('shell'); // see toggleSplitDir
  if (!state.broadcast && state.panes.length < 2) {
    toast(tr('toast.broadcastNeedsSplit'), true);
    return;
  }
  setBroadcast(!state.broadcast);
}

function updateSplitUi() {
  const split = state.panes.length >= 2;
  $('#terminals').classList.toggle('split-h', split && state.splitDir === 'h');
  // Update only the label spans so the leading icons stay put.
  $('#split-toggle .split-label').textContent =
    split && state.splitDir === 'v' ? tr('topbar.splitOff') : tr('topbar.split');
  $('#split-h-toggle .split-label').textContent =
    split && state.splitDir === 'h' ? tr('topbar.splitOff') : tr('topbar.splitH');
  // Broadcast only means something with two panes on screen.
  $('#broadcast-toggle').style.display = split ? '' : 'none';
  document.querySelectorAll('.pane-close').forEach((b) => (b.style.display = split ? '' : 'none'));
}

function enableSplitterDrag(splitter) {
  splitter.addEventListener('mousedown', (ev) => {
    ev.preventDefault();
    const horiz = state.splitDir === 'h';
    document.body.classList.add(horiz ? 'col-resizing' : 'row-resizing');
    const box = $('#pane-area').getBoundingClientRect();
    const move = (e) => {
      const ratio = horiz
        ? Math.min(0.85, Math.max(0.15, (e.clientX - box.left) / box.width))
        : Math.min(0.85, Math.max(0.15, (e.clientY - box.top) / box.height));
      state.panes[0].root.style.flex = `0 0 calc(${(ratio * 100).toFixed(1)}% - 4px)`;
    };
    const up = () => {
      document.body.classList.remove('row-resizing', 'col-resizing');
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
  chevronUp: '<polyline points="18 15 12 9 6 15"/>',
  chevronRight: '<polyline points="9 18 15 12 9 6"/>',
  folder: '<path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/>',
  // Sits next to `folder` in the remote browser, so it is drawn at the same
  // weight — a document sheet with its corner turned.
  file: '<path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><path d="M14 2v6h6"/>',
  grip: '<circle cx="9" cy="6" r="1"/><circle cx="9" cy="12" r="1"/><circle cx="9" cy="18" r="1"/><circle cx="15" cy="6" r="1"/><circle cx="15" cy="12" r="1"/><circle cx="15" cy="18" r="1"/>',
  download: '<path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="7 10 12 15 17 10"/><line x1="12" y1="15" x2="12" y2="3"/>',
  upload: '<path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="17 8 12 3 7 8"/><line x1="12" y1="3" x2="12" y2="15"/>',
  search: '<circle cx="11" cy="11" r="8"/><line x1="21" y1="21" x2="16.65" y2="16.65"/>',
  split: '<rect x="3" y="3" width="18" height="18" rx="2"/><line x1="3" y1="12" x2="21" y2="12"/>',
  splitH: '<rect x="3" y="3" width="18" height="18" rx="2"/><line x1="12" y1="3" x2="12" y2="21"/>',
  broadcast: '<circle cx="12" cy="12" r="2"/><path d="M16.24 7.76a6 6 0 0 1 0 8.49"/><path d="M7.76 16.24a6 6 0 0 1 0-8.49"/><path d="M19.07 4.93a10 10 0 0 1 0 14.14"/><path d="M4.93 19.07a10 10 0 0 1 0-14.14"/>',
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
  layers: '<polygon points="12 2 2 7 12 12 22 7 12 2"/><polyline points="2 17 12 22 22 17"/><polyline points="2 12 12 17 22 12"/>',
  // The icons below are Google Material Icons (24px, filled paths) rather than
  // Lucide strokes — hence their place in FILLED_ICONS.
  moreVert: '<path d="M12 8c1.1 0 2-.9 2-2s-.9-2-2-2-2 .9-2 2 .9 2 2 2zm0 2c-1.1 0-2 .9-2 2s.9 2 2 2 2-.9 2-2-.9-2-2-2zm0 6c-1.1 0-2 .9-2 2s.9 2 2 2 2-.9 2-2-.9-2-2-2z"/>',
  unfoldMore: '<path d="M12 5.83 15.17 9l1.41-1.41L12 3 7.41 7.59 8.83 9 12 5.83zm0 12.34L8.83 15l-1.41 1.41L12 21l4.59-4.59L15.17 15 12 18.17z"/>',
  unfoldLess: '<path d="M7.41 18.59 8.83 20 12 16.83 15.17 20l1.41-1.41L12 14l-4.59 4.59zm9.18-13.18L15.17 4 12 7.17 8.83 4 7.41 5.41 12 10l4.59-4.59z"/>',
  createNewFolder: '<path d="M20 6h-8l-2-2H4c-1.11 0-1.99.89-1.99 2L2 18c0 1.11.89 2 2 2h16c1.11 0 2-.89 2-2V8c0-1.11-.89-2-2-2zm-1 8h-3v3h-2v-3h-3v-2h3V9h2v3h3v2z"/>',
  contentPaste: '<path d="M19 2h-4.18C14.4.84 13.3 0 12 0c-1.3 0-2.4.84-2.82 2H5c-1.1 0-2 .9-2 2v16c0 1.1.9 2 2 2h14c1.1 0 2-.9 2-2V4c0-1.1-.9-2-2-2zm-7 0c.55 0 1 .45 1 1s-.45 1-1 1-1-.45-1-1 .45-1 1-1zm7 18H5V4h2v3h10V4h2v16z"/>',
  list: '<path d="M3 13h2v-2H3v2zm0 4h2v-2H3v2zm0-8h2V7H3v2zm4 4h14v-2H7v2zm0 4h14v-2H7v2zM7 7v2h14V7H7z"/>',
};
const FILLED_ICONS = new Set(['play', 'star', 'grip', 'moreVert', 'unfoldMore', 'unfoldLess', 'createNewFolder', 'list', 'contentPaste']);

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
  if (state.renaming) {
    // A rename is in progress: its inline <input> lives outside the normal
    // render cycle, so skip re-rendering — it would otherwise wipe the input
    // out from under the user on every unrelated state change. `finish()`
    // renders once the rename ends, so nothing stays stale for long.
    //
    // Unless the thread being renamed is itself gone (it exited while the
    // input was open): there is no longer an input worth protecting, and
    // skipping would leave a row for a dead session with a live editor in it.
    if (threadById(state.renaming)) return;
    state.renaming = null;
  }
  const list = $('#thread-list');
  list.innerHTML = '';
  if (!state.threads.length) {
    const li = document.createElement('li');
    li.className = 'thread-empty';
    li.innerHTML = `<div class="es-icon">${icon('terminal')}</div>
      <p class="es-title">${tr('thread.emptyTitle')}</p>
      <p class="es-hint">${tr('thread.emptyHint')}</p>`;
    list.appendChild(li);
  }
  const ordered = orderedThreads();
  // Roving tabindex: exactly one row is in the tab order (the one shown in the
  // focused pane, else the first), and ↑/↓ move focus between rows from there.
  // A flat `tabindex=0` on every row would bury the rest of the chrome behind
  // one Tab stop per thread.
  const rovingIdx = Math.max(0, ordered.findIndex((t) =>
    state.panes[state.focusedPane]?.threadId === t.id));
  ordered.forEach((t, rowIdx) => {
    const li = document.createElement('li');
    const paneIdx = state.panes.findIndex((p) => p.threadId === t.id);
    const isFocused = paneIdx === state.focusedPane && paneIdx >= 0;
    li.className = `thread-item ${t.kind}${paneIdx >= 0 ? ' shown' : ''}${isFocused ? ' focused' : ''}${t.pinned ? ' pinned' : ''}${t.activity ? ' activity' : ''}`;
    li.dataset.id = t.id;
    const color = threadColor(t);
    if (color) li.dataset.color = color;
    li.setAttribute('role', 'option');
    li.setAttribute('aria-selected', String(paneIdx >= 0));
    li.tabIndex = rowIdx === rovingIdx ? 0 : -1;
    li.innerHTML = `
      <span class="kind">${t.kind === 'ssh' ? 'SSH' : '❯'}</span>
      <span class="activity-dot" title="${tr('thread.activity.title')}"></span>
      ${t.logPath ? '<span class="rec">●</span>' : ''}
      ${t.reconnect ? '<span class="reconnecting">⟳</span>' : ''}
      <span class="title"></span>
      <span class="pane-mark">${paneIdx >= 0 && state.panes.length > 1 ? PANE_MARK[paneIdx] : ''}</span>
      <button class="pin" title="${t.pinned ? tr('thread.unpin') : tr('thread.pin')}">${pinIcon(t.pinned)}</button>
      <button class="close" title="${tr('thread.close.title')}">${icon('x')}</button>`;
    li.querySelector('.title').textContent = t.title;
    // Set as a property, not interpolated into the markup: the path is
    // user-controlled text (host names, the configured folder).
    li.querySelector('.rec')?.setAttribute('title', tr('thread.logging.title', { path: t.logPath }));
    if (t.reconnect) {
      li.classList.add('reconnecting');
      li.querySelector('.reconnecting').title = tr('reconnect.badge.title', { n: t.reconnect.attempt + 1, max: RECONNECT_BACKOFF.length });
    }
    li.querySelector('.pin').addEventListener('click', (ev) => {
      ev.stopPropagation();
      togglePin(t.id);
    });
    li.querySelector('.close').addEventListener('click', (ev) => {
      ev.stopPropagation();
      closeThread(t.id);
    });
    li.addEventListener('click', () => assignThread(state.focusedPane, t.id));
    // Keyboard equivalents of the row's mouse affordances.
    li.addEventListener('keydown', (ev) => {
      if (ev.key === 'ArrowDown' || ev.key === 'ArrowUp') {
        ev.preventDefault();
        const sib = ev.key === 'ArrowDown' ? li.nextElementSibling : li.previousElementSibling;
        if (sib?.classList.contains('thread-item')) {
          li.tabIndex = -1;
          sib.tabIndex = 0;
          sib.focus();
        }
      } else if (ev.key === 'Enter' || ev.key === ' ') {
        ev.preventDefault();
        assignThread(state.focusedPane, t.id);
      } else if (ev.key === 'Delete') {
        ev.preventDefault();
        closeThread(t.id);
      }
    });
    list.appendChild(li);
  });
  // Keep every pane's dropdown button in sync with the thread list; rebuild
  // the open menu (if any) so it reflects the latest threads/pins.
  state.panes.forEach((pane) => {
    const t = threadById(pane.threadId);
    pane.ddIcon.innerHTML = t && t.pinned ? pinIcon(true) : '';
    pane.ddLabel.textContent = t ? t.title : (state.threads.length ? '' : tr('thread.none'));
    pane.kindEl.textContent = t ? (t.kind === 'ssh' ? 'SSH' : tr('kind.local')) : '';
    // The host's label color frames the whole pane while its session is on
    // screen (see .pane[data-color]) — the one place the color has to be
    // unmissable, since it is there to be seen before a command is typed.
    const paneColor = threadColor(t);
    if (paneColor) pane.root.dataset.color = paneColor;
    else delete pane.root.dataset.color;
    if (!pane.ddMenu.classList.contains('hidden')) buildPaneMenu(pane);
  });
}

/** Sends text to the focused pane's thread; `run` submits it — each line of
 * a multi-line workflow gets its own Enter (not just the last one), the same
 * as if the user had typed each line and pressed Enter in turn. A bare '\n'
 * left in the middle of the buffer isn't reliably treated as "submit this
 * line" by every shell/REPL, so every line break is normalized to '\r' (and
 * a trailing one added) rather than relying on that. `挿入`(run:false) is
 * left untouched — it's meant to land in the input for the user to review
 * before running anything. */
function sendToActive(text, run) {
  const thread = focusedThread();
  if (!thread) {
    toast(tr('toast.noActiveTerm'), true);
    return;
  }
  sendToThread(thread, text, run);
  thread.term.focus();
}

/** The write itself, for one thread. Split out of sendToActive so a command
 * can go to a thread that is not focused (run-on-all over a host group). */
function sendToThread(thread, text, run) {
  const data = run ? text.replace(/\r\n|\r|\n/g, '\r') + '\r' : text;
  invoke('session_write', { id: thread.id, data }).catch((e) => toast(String(e), true));
}

/* ---------------- workflows ---------------- */

async function refreshWorkflows() {
  try {
    state.workflows = await invoke('list_workflows');
  } catch (e) {
    toast(tr('wf.loadFailed', { e }), true);
  }
  renderWorkflows();
  renderQuickbar();
}

/** Canonical key combo for a keydown event, e.g. "ctrl+shift+g": modifiers in
 * ctrl→alt→shift→meta order, then the lowercased `ev.key` (' ' → "space").
 * Unlike `shortcutFromEvent` it needs no modifier (Enter, Escape and the
 * arrows are bindings too). Returns null for modifier-only keys. */
function keyComboFromEvent(ev) {
  const k = ev.key;
  if (typeof k !== 'string' || ['Control', 'Alt', 'Shift', 'Meta', 'Dead'].includes(k)) return null;
  const mods = [];
  if (ev.ctrlKey) mods.push('ctrl');
  if (ev.altKey) mods.push('alt');
  if (ev.shiftKey) mods.push('shift');
  if (ev.metaKey) mods.push('meta');
  let key = k.toLowerCase();
  if (key === ' ') key = 'space';
  return [...mods, key].join('+');
}

/** Normalized shortcut string from a keydown event, e.g. "ctrl+shift+g".
 * Requires a ctrl/alt/meta modifier so a shortcut never fires while plainly
 * typing. Returns null for modifier-only or unmodified keys. */
function shortcutFromEvent(ev) {
  if (!(ev.ctrlKey || ev.altKey || ev.metaKey)) return null;
  return keyComboFromEvent(ev);
}

const KEY_MODIFIER_ORDER = ['ctrl', 'alt', 'shift', 'meta'];
const KEY_MODIFIER_ALIAS = {
  ctrl: 'ctrl', control: 'ctrl', alt: 'alt', option: 'alt', opt: 'alt', shift: 'shift',
  meta: 'meta', cmd: 'meta', command: 'meta', super: 'meta', win: 'meta',
};
const KEY_NAME_ALIAS = {
  esc: 'escape', return: 'enter', spacebar: 'space',
  up: 'arrowup', down: 'arrowdown', left: 'arrowleft', right: 'arrowright',
};
// What a key name may be: one character, or a `KeyboardEvent.key` name that
// exists (lowercased). Strict on purpose so a typo is reported, not silently
// bound to a key that can never be pressed.
const KEY_NAME_RE = /^(f([1-9]|1\d|2[0-4])|arrow(up|down|left|right)|enter|escape|tab|space|backspace|delete|insert|home|end|pageup|pagedown)$/;

/** Parses a user-written combo ("Ctrl+Shift+P", "cmd+k", "mod+enter") into the
 * canonical form `keyComboFromEvent` emits, or null if it does not parse.
 * Case-insensitive; parts are trimmed. `mod` is ⌘ on macOS and Ctrl elsewhere.
 * Exactly one non-modifier key is required. A literal "+" key cannot be
 * written (it is the separator, so "ctrl++" is rejected). */
function normalizeKeyCombo(str) {
  if (typeof str !== 'string') return null;
  const mods = new Set();
  let key = null;
  for (const raw of str.toLowerCase().split('+')) {
    const part = raw.trim();
    if (!part) return null;
    const mod = part === 'mod' ? (IS_MAC ? 'meta' : 'ctrl') : KEY_MODIFIER_ALIAS[part];
    if (mod) { mods.add(mod); continue; }
    if (key !== null) return null;
    key = KEY_NAME_ALIAS[part] || part;
    if ([...key].length !== 1 && !KEY_NAME_RE.test(key)) return null;
  }
  if (key === null) return null;
  return [...KEY_MODIFIER_ORDER.filter((m) => mods.has(m)), key].join('+');
}

const KEY_LABEL = {
  arrowup: '↑', arrowdown: '↓', arrowleft: '←', arrowright: '→',
  enter: 'Enter', escape: 'Esc', space: 'Space',
};
const SC_LABEL = { ctrl: 'Ctrl', alt: 'Alt', shift: 'Shift', meta: 'Meta' };
function prettyShortcut(s) {
  if (!s) return '';
  return s.split('+')
    .map((p) => (p === 'meta' && IS_MAC ? '⌘' : SC_LABEL[p] || KEY_LABEL[p]
      || (p.length === 1 ? p.toUpperCase() : p[0].toUpperCase() + p.slice(1))))
    .join('+');
}

/** The workflow whose registered shortcut matches this event, if any. */
function workflowForShortcut(ev) {
  const sc = shortcutFromEvent(ev);
  if (!sc) return null;
  return state.workflows.find((w) => w.shortcut && w.shortcut === sc) || null;
}

// Combos the app itself owns, which a workflow shortcut could never win, so
// binding them is pointless/confusing. The window handler treats Ctrl and Meta
// interchangeably (`ev.ctrlKey || ev.metaKey`), so the Meta form is reserved
// too — otherwise Cmd+Shift+P would be accepted by the editor on macOS and then
// silently never fire. Note the Meta spelling: `shortcutFromEvent` emits
// modifiers in ctrl→alt→shift→meta order, so Cmd+Shift+P normalizes to
// "shift+meta+p", not "meta+shift+p".
const RESERVED_SHORTCUTS = new Set([
  ...['p', 't', 'w', 'd', 'e', 'b', 'c', 'v', 'f', 'arrowup', 'arrowdown', '/', '?']
    .flatMap((k) => [`ctrl+shift+${k}`, `shift+meta+${k}`]),
  // Command-block navigation (OSC 133).
  'ctrl+alt+arrowup', 'ctrl+alt+arrowdown',
]);

/** True when the event targets one of the app's own text controls (a search
 * box, a modal field, the inline rename input) — where a keystroke is the user
 * typing, not invoking a workflow shortcut. The terminal is deliberately
 * excluded: xterm.js routes keys through a hidden `<textarea>` inside `.xterm`,
 * and running a workflow from the terminal is the whole point of the feature.
 * Without this, a workflow bound to e.g. Alt+G fires while typing in the
 * workflow search field (and on macOS, Option+key is ordinary text entry). */
function isAppTextEntry(el) {
  if (!el || typeof el.closest !== 'function') return false;
  if (el.closest('.xterm')) return false;
  return !!el.closest('input, textarea, select, [contenteditable="true"]');
}

/* ---------------- command palette keys ----------------
 * Built-in keys, overridable per action from `palette_keys` in settings.json.
 * `open` uses `mod` so it stays ⌘⇧P on macOS and Ctrl+Shift+P elsewhere, the
 * same split as `appMod`. */
const PALETTE_KEY_DEFAULTS = {
  open: ['mod+shift+p'],
  next: ['arrowdown', 'ctrl+n'],
  prev: ['arrowup', 'ctrl+p'],
  run: ['enter'],
  insert: ['shift+enter'],
  save: ['ctrl+enter', 'meta+enter'],
  close: ['escape'],
};

/** The key table in effect: `keys` action → canonical combos, `custom` action →
 * whether settings.json supplied it, `invalid` the entries that were ignored.
 * Recomputed by `applyPaletteKeys`; empty until the first settings load. */
let paletteKeys = { keys: {}, custom: {}, invalid: [] };

/** True when `combo` can open the palette without getting in the way of
 * typing: needs a ctrl/alt/meta modifier, or is a function key. */
function isSafeOpenCombo(combo) {
  const parts = combo.split('+');
  return /^f\d+$/.test(parts[parts.length - 1]) || parts.some((p) => ['ctrl', 'alt', 'meta'].includes(p));
}

function resolvePaletteKeys(settings) {
  const given = settings && settings.palette_keys;
  const src = given && typeof given === 'object' && !Array.isArray(given) ? given : {};
  const keys = {};
  const custom = {};
  const invalid = Object.keys(src).filter((a) => !(a in PALETTE_KEY_DEFAULTS));
  for (const action in PALETTE_KEY_DEFAULTS) {
    const list = src[action];
    const good = [];
    if (Array.isArray(list)) {
      for (const entry of list) {
        const combo = normalizeKeyCombo(entry);
        if (combo && (action !== 'open' || isSafeOpenCombo(combo))) good.push(combo);
        else invalid.push(String(entry));
      }
    } else if (list !== undefined) {
      invalid.push(action);
    }
    custom[action] = good.length > 0;
    keys[action] = good.length ? [...new Set(good)] : PALETTE_KEY_DEFAULTS[action].map(normalizeKeyCombo);
  }
  return { keys, custom, invalid };
}

/** Does this keydown trigger palette action `action` ("open", "next", …)? */
function paletteKeyIs(action, ev) {
  const combo = keyComboFromEvent(ev);
  return !!combo && (paletteKeys.keys[action] || []).includes(combo);
}

/** The combo to show for an action: on macOS the ⌘ form if there is one, else
 * the first listed. */
function paletteKeyFirst(action) {
  const list = paletteKeys.keys[action] || [];
  return (IS_MAC && list.find((c) => c.split('+').includes('meta'))) || list[0] || '';
}

// Display order puts meta before shift (⌘⇧P), unlike the canonical one.
function displayCombo(combo) {
  const parts = combo.split('+');
  const key = parts.pop();
  const rank = (m) => ['ctrl', 'alt', 'meta', 'shift'].indexOf(m);
  return [...parts.sort((a, b) => rank(a) - rank(b)), key].join('+');
}

function paletteKeyLabel(action) {
  return prettyShortcut(displayCombo(paletteKeyFirst(action)));
}

/** The pieces of a combo for separate <kbd> chips (⌘ ⇧ P / Ctrl Enter / ↓). */
function kbdParts(combo) {
  const glyph = { ctrl: IS_MAC ? '⌃' : 'Ctrl', alt: IS_MAC ? '⌥' : 'Alt', shift: '⇧', meta: IS_MAC ? '⌘' : 'Meta' };
  return displayCombo(combo).split('+').map((p) => glyph[p] || prettyShortcut(p));
}

/** Compact one-chip form for the top bar: "Ctrl⇧P", "⌘⇧P", "Ctrl+K", "F1". */
function compactCombo(combo) {
  const isGlyph = (c) => /[⌃⌥⇧⌘↑↓←→]/.test(c);
  return kbdParts(combo).reduce((out, p, i) => (i === 0 ? p
    : out + (isGlyph(out.slice(-1)) || isGlyph(p[0]) ? '' : '+') + p), '');
}

/** Repaints everything that shows palette keys: top-bar chip + tooltip, the
 * palette footer. (The shortcut overlay re-renders itself when opened.) */
function renderPaletteKeyUi() {
  const open = paletteKeyFirst('open');
  const chip = document.querySelector('#palette-btn kbd');
  if (chip && open) chip.textContent = compactCombo(open);
  const btn = $('#palette-btn');
  if (btn) btn.title = tr('topbar.palette.title', { key: paletteKeyLabel('open') });
  document.querySelectorAll('[data-palette-keys]').forEach((el) => {
    el.replaceChildren();
    for (const action of el.dataset.paletteKeys.split(' ')) {
      const combo = paletteKeyFirst(action);
      if (!combo) continue;
      for (const part of kbdParts(combo)) {
        const k = document.createElement('kbd');
        k.textContent = part;
        el.appendChild(k);
      }
    }
  });
}

/** Re-reads `palette_keys` from `state.settings`, repaints, and warns once
 * about entries it had to ignore. */
function applyPaletteKeys() {
  paletteKeys = resolvePaletteKeys(state.settings);
  renderPaletteKeyUi();
  if (paletteKeys.invalid.length) toast(tr('paletteKeys.invalid', { keys: paletteKeys.invalid.join(', ') }), true);
}

/** Returns a reason string if `sc` is a bad workflow shortcut, else null. */
function shortcutConflict(sc, excludeId) {
  if (RESERVED_SHORTCUTS.has(sc)) return tr('sc.conflict.reserved');
  if ((paletteKeys.keys.open || []).includes(sc)) return tr('sc.conflict.paletteOpen');
  // A plain Ctrl+<letter> (no shift/alt/meta) is a core shell key (Ctrl+C
  // interrupt, Ctrl+D EOF, Ctrl+Z suspend, …); stealing it would break the
  // terminal.
  if (/^ctrl\+[a-z0-9]$/.test(sc)) return tr('sc.conflict.shell');
  const dup = state.workflows.find((w) => w.id !== excludeId && w.shortcut === sc);
  if (dup) return tr('sc.conflict.dup', { name: dup.name });
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

/** Path segments of a workflow's group, trimmed and empties dropped.
 * `"Deploy / Staging"` → `["Deploy", "Staging"]`; ungrouped → `[]`. */
function groupParts(w) {
  return (w.group || '').split('/').map((x) => x.trim()).filter(Boolean);
}

/** Canonical group path — the same normalization the backend stores. */
function normalizeGroup(s) {
  return (s || '').split('/').map((x) => x.trim()).filter(Boolean).join('/');
}

/** Distinct existing group paths (plus every ancestor prefix) for the editor's
 * autocomplete, so users can nest under an existing group without retyping. */
function existingGroups(items = state.workflows) {
  const set = new Set();
  for (const w of items) {
    const parts = groupParts(w);
    for (let i = 1; i <= parts.length; i++) set.add(parts.slice(0, i).join('/'));
  }
  return [...set].sort();
}

/** Builds a tree of workflows keyed by their `/`-separated group path. Each
 * node keeps its direct `workflows` (in stored order) and child `groups` (in
 * first-appearance order) — used by both the grouped panel and the right-click
 * flyout menu. */
function buildWorkflowTree(workflows) {
  const root = { name: '', path: '', groups: new Map(), workflows: [] };
  for (const w of workflows) {
    let node = root;
    for (const part of groupParts(w)) {
      if (!node.groups.has(part)) {
        node.groups.set(part, {
          name: part,
          path: node.path ? `${node.path}/${part}` : part,
          groups: new Map(),
          workflows: [],
        });
      }
      node = node.groups.get(part);
    }
    node.workflows.push(w);
  }
  return root;
}

/** Total workflows in a node, including all descendants. */
function countWorkflows(node) {
  let n = node.workflows.length;
  for (const [, c] of node.groups) n += countWorkflows(c);
  return n;
}

/** Moves a workflow one slot up/down among its same-group siblings by swapping
 * their positions in the stored order, then persisting via `reorder_workflows`.
 * Swapping two same-group items never disturbs any other group's order. */
async function moveWorkflow(w, dir) {
  const groupOf = (x) => groupParts(x).join('/');
  const g = groupOf(w);
  const sibs = state.workflows.filter((x) => groupOf(x) === g);
  const si = sibs.findIndex((x) => x.id === w.id);
  const target = sibs[si + dir];
  if (!target) return; // already at the group's edge
  const ids = state.workflows.map((x) => x.id);
  const ai = ids.indexOf(w.id);
  const bi = ids.indexOf(target.id);
  [ids[ai], ids[bi]] = [ids[bi], ids[ai]];
  try {
    await invoke('reorder_workflows', { ids });
    await refreshWorkflows();
  } catch (e) {
    toast(tr('toast.reorderFailed', { e }), true);
  }
}

/** Saves a copy of `w` as a new workflow placed right after the original.
 *
 * The name gets a "コピー" suffix and the shortcut is dropped — a key combo may
 * only be bound to one workflow (`shortcutConflict`), so carrying it over would
 * create a duplicate binding that the editor itself would reject. Everything
 * else (command, description, group, tags, quick button) is copied verbatim,
 * which is the point: duplicating is how you fork a long template before
 * tweaking it. */
async function duplicateWorkflow(w) {
  try {
    const copy = await invoke('save_workflow', {
      workflow: {
        id: '',
        name: tr('wf.copyName', { name: w.name }),
        description: w.description || '',
        command: w.command,
        tags: [...(w.tags || [])],
        group: w.group || '',
        shortcut: '',
        show_button: !!w.show_button,
      },
    });
    // save_workflow appends, which in a grouped panel drops the copy under some
    // unrelated group far from what the user just clicked. Move it next to its
    // original so it lands in view, in the same group. The copy is filtered out
    // first (as in `reorderWorkflowTo`) so it is listed exactly once even if
    // `state.workflows` already picked it up — a doubled id is not a no-op for
    // every reorder implementation.
    const ids = state.workflows.map((x) => x.id).filter((id) => id !== copy.id);
    const at = ids.indexOf(w.id);
    if (at >= 0) {
      ids.splice(at + 1, 0, copy.id);
      await invoke('reorder_workflows', { ids });
    }
    await refreshWorkflows();
    toast(tr('wf.duplicated', { name: copy.name }));
  } catch (e) {
    toast(tr('wf.duplicateFailed', { e }), true);
  }
}

// The workflow currently being dragged (drag-and-drop reorder), or null.
// `group` gates drops onto other *cards* so a card can only be dropped among
// its own group's siblings — the same constraint as the ▲▼ buttons. Rows of
// the group tree ignore that gate: dropping on one moves the workflow there.
let wfDrag = null;

/** Drops the dragged workflow immediately before/after `targetId` in the stored
 * order and persists it. Both are in the same group (enforced by the drag
 * handlers), so this only reorders within that group. */
async function reorderWorkflowTo(draggedId, targetId, placeAfter) {
  if (draggedId === targetId) return;
  const ids = state.workflows.map((x) => x.id).filter((id) => id !== draggedId);
  const ti = ids.indexOf(targetId);
  if (ti < 0) return;
  ids.splice(placeAfter ? ti + 1 : ti, 0, draggedId);
  try {
    await invoke('reorder_workflows', { ids });
    await refreshWorkflows();
  } catch (e) {
    toast(tr('toast.reorderFailed', { e }), true);
  }
}

/** One workflow card. `opts.showGroup` adds a group badge (used in the flat
 * search view); `opts.moveUp`/`opts.moveDown` add ▲▼ reorder buttons, each
 * disabled at the group's edge; `opts.draggable` makes the card reorderable by
 * dragging its grip handle onto another card in the same group.
 *
 * The card deliberately offers no "run" or "insert" button: both go through
 * `sendToActive`, and this panel is a full-window tool view with the terminal
 * workspace hidden (see `#body.tool-view`) — so they either fire at a thread
 * the user cannot see or, with no thread open, only toast "no active
 * terminal". Both are offered where a terminal *is* visible instead: the
 * quick-launch bar, the terminal right-click menu (Shift+click inserts), the
 * command palette (Shift+Enter inserts), and per-workflow shortcuts. The card
 * is left to manage the workflow itself — edit, duplicate, delete, reorder. */
function workflowCard(w, opts = {}) {
  const li = document.createElement('li');
  li.className = 'card';
  li.innerHTML = `
    <h4></h4><div class="desc"></div><code></code>
    <div class="wf-badges"></div>
    <div class="row">
      <button class="accent-btn dup">${icon('copy')}${tr('common.duplicate')}</button>
      <button class="ghost-btn edit">${icon('edit')}${tr('common.edit')}</button>
      <button class="danger-btn del">${icon('trash')}${tr('common.delete')}</button>
    </div>`;
  li.querySelector('h4').textContent = w.name;
  li.querySelector('.desc').textContent = w.description || '';
  li.querySelector('code').textContent = w.command;
  const badges = li.querySelector('.wf-badges');
  if (opts.showGroup && groupParts(w).length) {
    const g = document.createElement('span');
    g.className = 'wf-badge wf-group-badge';
    g.innerHTML = `${icon('folder')}<span></span>`;
    g.querySelector('span').textContent = groupParts(w).join(' / ');
    badges.appendChild(g);
  }
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
    b.innerHTML = `${icon('star')}${tr('wf.quickButton')}`;
    badges.appendChild(b);
  }
  if (opts.moveUp !== undefined || opts.moveDown !== undefined) {
    const reorder = document.createElement('div');
    reorder.className = 'wf-reorder';
    if (opts.draggable) {
      // Drag handle. The card is only made draggable while the grip is held, so
      // dragging never fires from a button click or from selecting the command
      // text (which stays selectable).
      const grip = document.createElement('button');
      grip.type = 'button';
      grip.className = 'icon-btn wf-drag-handle';
      grip.title = tr('wf.drag');
      grip.innerHTML = icon('grip');
      grip.addEventListener('mousedown', () => { li.draggable = true; });
      grip.addEventListener('mouseup', () => { li.draggable = false; });
      reorder.appendChild(grip);
    }
    const up = document.createElement('button');
    up.className = 'icon-btn';
    up.title = tr('common.moveUp');
    up.innerHTML = icon('chevronUp');
    up.disabled = !opts.moveUp;
    up.addEventListener('click', () => moveWorkflow(w, -1));
    const down = document.createElement('button');
    down.className = 'icon-btn';
    down.title = tr('common.moveDown');
    down.innerHTML = icon('chevronDown');
    down.disabled = !opts.moveDown;
    down.addEventListener('click', () => moveWorkflow(w, 1));
    reorder.appendChild(up);
    reorder.appendChild(down);
    li.querySelector('.row').appendChild(reorder);
  }
  if (opts.draggable) wireWorkflowDrag(li, w);
  li.querySelector('.edit').addEventListener('click', () => editWorkflow(w));
  const dup = li.querySelector('.dup');
  dup.title = tr('wf.duplicate.title');
  dup.addEventListener('click', () => duplicateWorkflow(w));
  li.querySelector('.del').addEventListener('click', async () => {
    if (!(await confirmModal(tr('confirm.delete', { name: w.name })))) return;
    await invoke('delete_workflow', { id: w.id });
    refreshWorkflows();
  });
  return li;
}

/** Wires HTML5 drag-and-drop reordering onto a workflow card. A card accepts a
 * drop only from a card in the same group; the drop lands the dragged workflow
 * before or after this card depending on which half the cursor is over, shown
 * live with a `drop-before`/`drop-after` insertion line. */
function wireWorkflowDrag(li, w) {
  const group = groupParts(w).join('/');
  li.dataset.wfId = w.id;
  li.dataset.wfGroup = group;
  const clearMarks = () => li.classList.remove('drop-before', 'drop-after');
  const isAfter = (ev) => {
    const r = li.getBoundingClientRect();
    return ev.clientY > r.top + r.height / 2;
  };
  const accepts = () => wfDrag && wfDrag.group === group && wfDrag.id !== w.id;

  li.addEventListener('dragstart', (ev) => {
    wfDrag = { id: w.id, group };
    li.classList.add('dragging');
    ev.dataTransfer.effectAllowed = 'move';
    // Some browsers require data to be set for the drag to actually start.
    ev.dataTransfer.setData('text/plain', w.id);
  });
  li.addEventListener('dragend', () => {
    li.classList.remove('dragging');
    li.draggable = false;
    wfDrag = null;
    document.querySelectorAll('#wf-list .drop-before, #wf-list .drop-after, #wf-tree-body .drop-into')
      .forEach((el) => el.classList.remove('drop-before', 'drop-after', 'drop-into'));
  });
  li.addEventListener('dragover', (ev) => {
    if (!accepts()) return;
    ev.preventDefault(); // allow the drop
    ev.dataTransfer.dropEffect = 'move';
    const after = isAfter(ev);
    li.classList.toggle('drop-after', after);
    li.classList.toggle('drop-before', !after);
  });
  li.addEventListener('dragleave', clearMarks);
  li.addEventListener('drop', (ev) => {
    if (!accepts()) return;
    ev.preventDefault();
    const after = isAfter(ev);
    const draggedId = wfDrag.id;
    clearMarks();
    reorderWorkflowTo(draggedId, w.id, after);
  });
}

/* ---- group tree (left pane of the Workflows panel) ----
 *
 * Groups are not stored entities: a workflow just carries a `group` path like
 * "Deploy/Staging", and `buildWorkflowTree` derives the hierarchy from it. So
 * every group operation below is really an operation on the workflows
 * underneath — reordering groups rewrites workflow order, renaming rewrites the
 * `group` prefix, and an empty group cannot exist (a new one only appears once
 * its first workflow is saved). */

// Selection keys. A group is `g:<path>`; the two fixed rows get their own keys
// so no group name, whatever it is, can collide with them.
const WF_ALL = 'all';
const WF_NONE = 'none';
const wfGroupKey = (path) => `g:${path}`;

// UI-only state for the tree: which node the card list is scoped to and which
// groups are folded. Shared by the tree and the list's group headings, and kept
// in localStorage as a per-viewer convenience — every access is guarded because
// storage can be unavailable or hold junk, and the panel must work without it.
const wfUi = { selected: WF_ALL, collapsed: new Set() };

function loadWfUi() {
  try {
    const c = JSON.parse(localStorage.getItem('senju.wf.collapsed') || '[]');
    if (Array.isArray(c)) wfUi.collapsed = new Set(c.filter((x) => typeof x === 'string'));
  } catch { /* unavailable or corrupt: start expanded */ }
  try {
    const s = localStorage.getItem('senju.wf.selected');
    if (typeof s === 'string' && s) wfUi.selected = s;
  } catch { /* start on "All" */ }
}

function saveWfUi() {
  try {
    localStorage.setItem('senju.wf.collapsed', JSON.stringify([...wfUi.collapsed]));
    localStorage.setItem('senju.wf.selected', wfUi.selected);
  } catch { /* storage unavailable: the choice just won't survive a restart */ }
}
loadWfUi();

/** Workflow ids in the order the panel shows them: a node's own workflows, then
 * each child group (in Map order) recursively. Feeding this to
 * `reorder_workflows` after editing a node's `groups` Map is how a group move
 * is persisted. */
function flattenWorkflowTree(node) {
  const ids = node.workflows.map((w) => w.id);
  for (const [, c] of node.groups) ids.push(...flattenWorkflowTree(c));
  return ids;
}

/** The node for a `/`-separated group path ('' = the root), or null. */
function findGroupNode(root, path) {
  let node = root;
  for (const part of path.split('/').filter(Boolean)) {
    node = node.groups.get(part);
    if (!node) return null;
  }
  return node;
}

const groupParentPath = (path) => path.slice(0, Math.max(0, path.lastIndexOf('/')));
const groupLeafName = (path) => path.slice(path.lastIndexOf('/') + 1);
/** True when workflow `w` is in group `path` or any group below it. */
const inGroupSubtree = (w, path) => {
  const g = groupParts(w).join('/');
  return g === path || g.startsWith(`${path}/`);
};

/** Writes the tree's current group order to the backend and reloads. */
async function persistGroupOrder(root) {
  try {
    await invoke('reorder_workflows', { ids: flattenWorkflowTree(root) });
    await refreshWorkflows();
  } catch (e) {
    toast(tr('toast.reorderFailed', { e }), true);
  }
}

/** Moves a group one slot up/down among its siblings (same parent only — the
 * same rule as workflows within a group). */
async function moveGroup(path, dir) {
  const root = buildWorkflowTree(state.workflows);
  const parent = findGroupNode(root, groupParentPath(path));
  if (!parent) return;
  const entries = [...parent.groups];
  const i = entries.findIndex(([name]) => name === groupLeafName(path));
  const j = i + dir;
  if (i < 0 || j < 0 || j >= entries.length) return; // already at the edge
  [entries[i], entries[j]] = [entries[j], entries[i]];
  parent.groups = new Map(entries);
  await persistGroupOrder(root);
}

/** Drops group `draggedPath` immediately before/after sibling `targetPath`. */
async function reorderGroupTo(draggedPath, targetPath, placeAfter) {
  if (draggedPath === targetPath) return;
  if (groupParentPath(draggedPath) !== groupParentPath(targetPath)) return;
  const root = buildWorkflowTree(state.workflows);
  const parent = findGroupNode(root, groupParentPath(targetPath));
  if (!parent) return;
  const dragged = groupLeafName(draggedPath);
  const target = groupLeafName(targetPath);
  const entries = [...parent.groups].filter(([name]) => name !== dragged);
  const ti = entries.findIndex(([name]) => name === target);
  const moved = parent.groups.get(dragged);
  if (ti < 0 || !moved) return;
  entries.splice(placeAfter ? ti + 1 : ti, 0, [dragged, moved]);
  parent.groups = new Map(entries);
  await persistGroupOrder(root);
}

/** Moves one workflow into group `path` ('' = ungrouped) and files it at the
 * end of that group's own workflows. `save_workflow` keeps a known id's
 * position, so without the follow-up reorder the card would stay wherever it
 * sat in the old group's stretch of the stored order. */
async function moveWorkflowToGroup(id, path) {
  const w = state.workflows.find((x) => x.id === id);
  if (!w || groupParts(w).join('/') === path) return;
  try {
    await invoke('save_workflow', { workflow: { ...w, group: path } });
    // Re-place the card in the tree and flatten it, rather than splicing the
    // flat list: every group keeps its slot among its siblings — the source
    // group too, which would otherwise shift when the card was its first.
    const root = buildWorkflowTree(state.workflows);
    const from = findGroupNode(root, groupParts(w).join('/'));
    const to = findGroupNode(root, path);
    if (from && to) {
      from.workflows = from.workflows.filter((x) => x.id !== id);
      to.workflows.push(w);
    }
    const ids = flattenWorkflowTree(root);
    await invoke('reorder_workflows', { ids });
    await refreshWorkflows();
    toast(tr('wf.moved', { name: w.name, group: path ? path.split('/').join(' / ') : tr('wf.tree.ungrouped') }));
  } catch (e) {
    toast(tr('wf.moveFailed', { e }), true);
    refreshWorkflows();
  }
}

/** Why `name` cannot be a single group segment, or null when it can. */
function groupNameProblem(name) {
  if (!name) return tr('wf.group.name.empty');
  if (name.includes('/')) return tr('wf.group.name.slash');
  return null;
}

/** Asks for a new group's name, then opens the workflow editor with the group
 * prefilled. The group does not exist yet — it is created by saving that first
 * workflow — which is why the dialog says so instead of just "Create". */
function promptNewGroup(parentPath) {
  openModal({
    title: parentPath ? tr('wf.group.newSub.title') : tr('wf.group.new.title'),
    okLabel: tr('wf.group.next'),
    body: [
      field(tr('wf.group.name'), 'name', '', { required: true }),
      fieldNote(tr('wf.group.new.hint')),
    ],
    onOk: async (values) => {
      const name = values.name.trim();
      const problem = groupNameProblem(name);
      if (problem) throw problem; // shown inline; the dialog stays open
      const group = parentPath ? `${parentPath}/${name}` : name;
      // Deferred: submitModal hides the modal right after onOk resolves, which
      // would immediately close an editor opened from in here.
      setTimeout(() => editWorkflow(newWorkflowIn(group)), 0);
    },
  });
}

/** A blank, unsaved workflow draft in `group` (editWorkflow treats `id: ''` as new). */
function newWorkflowIn(group) {
  return { id: '', name: '', description: '', command: '', tags: [], group, shortcut: '', show_button: false };
}

/** Renames the last segment of group `path`. Every workflow in the subtree
 * gets its `group` prefix rewritten through `save_workflow`, which keeps each
 * one's position, so the group stays where it was. Renaming onto an existing
 * sibling's name simply merges the two. */
function promptRenameGroup(path) {
  const oldName = groupLeafName(path);
  openModal({
    title: tr('wf.group.rename.title'),
    okLabel: tr('common.save'),
    body: [field(tr('wf.group.name'), 'name', oldName, { required: true })],
    onOk: async (values) => {
      const name = values.name.trim();
      const problem = groupNameProblem(name);
      if (problem) throw problem;
      if (name === oldName) return;
      const parent = groupParentPath(path);
      const newPath = parent ? `${parent}/${name}` : name;
      const affected = state.workflows.filter((w) => inGroupSubtree(w, path));
      try {
        for (const w of affected) {
          const g = groupParts(w).join('/');
          await invoke('save_workflow', { workflow: { ...w, group: newPath + g.slice(path.length) } });
        }
      } catch (e) {
        await refreshWorkflows(); // some may have been renamed already
        throw tr('wf.group.renameFailed', { e });
      }
      // Keep the fold state and the selection pointing at the renamed group.
      const remap = (p) => (p === path || p.startsWith(`${path}/`) ? newPath + p.slice(path.length) : p);
      wfUi.collapsed = new Set([...wfUi.collapsed].map(remap));
      if (wfUi.selected.startsWith('g:')) wfUi.selected = wfGroupKey(remap(wfUi.selected.slice(2)));
      saveWfUi();
      await refreshWorkflows();
    },
  });
}

/** Whether `w` belongs to the tree node the list is currently scoped to. */
function inWfSelection(w) {
  const sel = wfUi.selected;
  if (sel === WF_ALL) return true;
  if (sel === WF_NONE) return groupParts(w).length === 0;
  return inGroupSubtree(w, sel.slice(2));
}

/** Falls back to "All" when the saved selection no longer exists (its last
 * workflow was deleted or moved, or the stored value is stale). */
function resolveWfSelection(root) {
  const sel = wfUi.selected;
  const ok = sel === WF_ALL
    || (sel === WF_NONE && root.workflows.length > 0)
    || (sel.startsWith('g:') && !!findGroupNode(root, sel.slice(2)));
  if (!ok) {
    wfUi.selected = WF_ALL;
    saveWfUi();
  }
}

function selectWfNode(key) {
  wfUi.selected = key;
  saveWfUi();
  renderWorkflows();
}

function toggleWfCollapsed(path, collapse) {
  const folded = wfUi.collapsed.has(path);
  if (collapse === undefined) collapse = !folded;
  if (collapse === folded) return;
  if (collapse) wfUi.collapsed.add(path); else wfUi.collapsed.delete(path);
  saveWfUi();
  renderWorkflows();
}

/** Every group path that has subgroups — the only ones that can be folded. */
function foldableGroupPaths(node, out = []) {
  for (const [, c] of node.groups) {
    if (c.groups.size) out.push(c.path);
    foldableGroupPaths(c, out);
  }
  return out;
}

// The group row being dragged in the tree (sibling reorder), or null. Card
// drags use `wfDrag`; the two never overlap.
let wfGroupDrag = null;

/** One row of the tree. `o`: key, depth, iconName, name, count, and for real
 * groups also `path`, `node`, `index`, `siblings`. */
function wfTreeRow(o) {
  const isGroup = o.path !== undefined;
  const row = document.createElement('div');
  row.className = 'wf-tree-row';
  row.setAttribute('role', 'treeitem');
  row.dataset.key = o.key;
  if (isGroup) row.dataset.path = o.path;
  row.style.setProperty('--depth', o.depth);
  row.setAttribute('aria-level', o.depth + 1);
  row.setAttribute('aria-selected', String(wfUi.selected === o.key));
  row.classList.toggle('selected', wfUi.selected === o.key);
  const foldable = isGroup && o.node.groups.size > 0;
  const folded = foldable && wfUi.collapsed.has(o.path);
  if (foldable) row.setAttribute('aria-expanded', String(!folded));
  row.innerHTML = `
    <span class="wf-tree-guides"></span>
    <span class="wf-tree-twist">${foldable ? icon(folded ? 'chevronRight' : 'chevronDown') : ''}</span>
    ${icon(o.iconName)}
    <span class="wf-tree-name"></span>
    <span class="wf-tree-count"></span>`;
  row.querySelector('.wf-tree-name').textContent = o.name;
  row.querySelector('.wf-tree-count').textContent = o.count;
  row.title = o.name;
  row.addEventListener('click', () => selectWfNode(o.key));
  if (foldable) {
    row.querySelector('.wf-tree-twist').addEventListener('click', (ev) => {
      ev.stopPropagation(); // fold without also selecting the row
      toggleWfCollapsed(o.path);
    });
  }

  if (isGroup) {
    const actions = document.createElement('span');
    actions.className = 'wf-tree-actions';
    const mk = (iconName, title, onClick, disabled) => {
      const b = document.createElement('button');
      b.type = 'button';
      b.className = 'icon-btn wf-tree-act';
      b.tabIndex = -1; // reached with the keyboard through the row's context menu
      b.title = title;
      b.innerHTML = icon(iconName);
      b.disabled = !!disabled;
      b.addEventListener('click', (ev) => { ev.stopPropagation(); onClick(b); });
      actions.appendChild(b);
    };
    mk('chevronUp', tr('wf.group.moveUp'), () => moveGroup(o.path, -1), o.index === 0);
    mk('chevronDown', tr('wf.group.moveDown'), () => moveGroup(o.path, 1), o.index === o.siblings - 1);
    mk('moreVert', tr('wf.group.more'), (b) => {
      const r = b.getBoundingClientRect();
      openWfTreeMenu(r.left, r.bottom + 2, o.path);
    });
    row.appendChild(actions);
    row.addEventListener('contextmenu', (ev) => {
      ev.preventDefault();
      openWfTreeMenu(ev.clientX, ev.clientY, o.path);
    });
  }
  wireWfTreeDrag(row, o, isGroup);
  row.addEventListener('keydown', (ev) => wfTreeKey(ev, row, o, foldable, folded));
  return row;
}

/** Group rows drag among their siblings; any row but "All" also takes a
 * dropped workflow card. Reordering shows the same before/after insertion line
 * as cards; a card drop highlights the whole row instead. */
function wireWfTreeDrag(row, o, isGroup) {
  const marks = ['drop-before', 'drop-after', 'drop-into'];
  const clear = () => row.classList.remove(...marks);
  const isAfter = (ev) => {
    const r = row.getBoundingClientRect();
    return ev.clientY > r.top + r.height / 2;
  };
  // What a drag over this row would do, or null when it can't land here.
  const intent = () => {
    if (isGroup && wfGroupDrag && wfGroupDrag.path !== o.path
        && groupParentPath(wfGroupDrag.path) === groupParentPath(o.path)) return 'reorder';
    if (wfDrag && o.key !== WF_ALL && wfDrag.group !== (isGroup ? o.path : '')) return 'move';
    return null;
  };
  if (isGroup && o.siblings > 1) {
    row.draggable = true;
    row.addEventListener('dragstart', (ev) => {
      wfGroupDrag = { path: o.path };
      row.classList.add('dragging');
      ev.dataTransfer.effectAllowed = 'move';
      ev.dataTransfer.setData('text/plain', o.path);
    });
    row.addEventListener('dragend', () => {
      wfGroupDrag = null;
      row.classList.remove('dragging');
      document.querySelectorAll('#wf-tree-body .wf-tree-row')
        .forEach((el) => el.classList.remove(...marks));
    });
  }
  row.addEventListener('dragover', (ev) => {
    const what = intent();
    if (!what) return;
    ev.preventDefault();
    ev.dataTransfer.dropEffect = 'move';
    if (what === 'reorder') {
      const after = isAfter(ev);
      row.classList.toggle('drop-after', after);
      row.classList.toggle('drop-before', !after);
    } else {
      row.classList.add('drop-into');
    }
  });
  row.addEventListener('dragleave', (ev) => {
    if (!row.contains(ev.relatedTarget)) clear();
  });
  row.addEventListener('drop', (ev) => {
    const what = intent();
    if (!what) return;
    ev.preventDefault();
    const after = isAfter(ev);
    clear();
    if (what === 'reorder') reorderGroupTo(wfGroupDrag.path, o.path, after);
    else moveWorkflowToGroup(wfDrag.id, isGroup ? o.path : '');
  });
}

/** Tree keyboard model: ↑/↓ move focus, ← folds (or jumps to the parent),
 * → unfolds (or steps into the first child), Enter/Space selects. */
function wfTreeKey(ev, row, o, foldable, folded) {
  const rows = [...$('#wf-tree-body').querySelectorAll('.wf-tree-row')];
  const i = rows.indexOf(row);
  const focusRow = (el) => { if (el) { ev.preventDefault(); el.focus(); } };
  switch (ev.key) {
    case 'ArrowDown': focusRow(rows[i + 1]); break;
    case 'ArrowUp': focusRow(rows[i - 1]); break;
    case 'Home': focusRow(rows[0]); break;
    case 'End': focusRow(rows[rows.length - 1]); break;
    case 'ArrowRight':
      ev.preventDefault();
      if (foldable && folded) toggleWfCollapsed(o.path, false);
      else if (foldable) rows[i + 1]?.focus();
      break;
    case 'ArrowLeft':
      ev.preventDefault();
      if (foldable && !folded) toggleWfCollapsed(o.path, true);
      else if (o.path !== undefined && o.path.includes('/')) {
        focusRow(rows.find((r) => r.dataset.path === groupParentPath(o.path)));
      }
      break;
    case 'Enter':
    case ' ':
      ev.preventDefault();
      selectWfNode(o.key);
      break;
    case 'ContextMenu':
    case 'F10':
      if (o.path !== undefined && (ev.key === 'ContextMenu' || ev.shiftKey)) {
        ev.preventDefault();
        const r = row.getBoundingClientRect();
        openWfTreeMenu(r.left + 24, r.bottom, o.path);
      }
      break;
    default:
  }
}

/** Appends the rows for a node's child groups, recursing into the unfolded ones. */
function renderWfTreeNode(body, node, depth) {
  const siblings = node.groups.size;
  let index = 0;
  for (const [, child] of node.groups) {
    body.appendChild(wfTreeRow({
      key: wfGroupKey(child.path), path: child.path, node: child, depth,
      iconName: 'folder', name: child.name, count: countWorkflows(child), index, siblings,
    }));
    index++;
    if (child.groups.size && !wfUi.collapsed.has(child.path)) renderWfTreeNode(body, child, depth + 1);
  }
}

function renderWorkflowTree(root) {
  const body = $('#wf-tree-body');
  // Re-rendering replaces every row; hand focus back to the same one so
  // keyboard navigation survives folding and selecting.
  const focusedKey = body.contains(document.activeElement) ? document.activeElement.dataset.key : null;
  body.innerHTML = '';
  body.appendChild(wfTreeRow({
    key: WF_ALL, depth: 0, iconName: 'list', name: tr('wf.tree.all'), count: state.workflows.length,
  }));
  if (root.workflows.length) {
    body.appendChild(wfTreeRow({
      key: WF_NONE, depth: 0, iconName: 'folder', name: tr('wf.tree.ungrouped'), count: root.workflows.length,
    }));
  }
  renderWfTreeNode(body, root, 0);
  // Roving tabindex: one stop for Tab (the selected row, else the first), the
  // rest reached with the arrow keys.
  const rows = [...body.children];
  const stop = rows.find((r) => r.dataset.key === wfUi.selected) || rows[0];
  for (const r of rows) r.tabIndex = r === stop ? 0 : -1;
  if (focusedKey) rows.find((r) => r.dataset.key === focusedKey)?.focus();
}

/** The "more" menu of a group row: a small popup next to the pointer, closed by
 * any outside click, Escape, or choosing an item. */
function openWfTreeMenu(x, y, path) {
  closeWfTreeMenu();
  const menu = document.createElement('div');
  menu.id = 'wf-tree-menu';
  menu.className = 'popup-menu';
  menu.setAttribute('role', 'menu');
  const items = [
    ['plus', tr('wf.group.addWorkflow'), () => editWorkflow(newWorkflowIn(path))],
    ['createNewFolder', tr('wf.group.newSub'), () => promptNewGroup(path)],
    ['edit', tr('wf.group.rename'), () => promptRenameGroup(path)],
  ];
  for (const [iconName, label, run] of items) {
    const b = document.createElement('button');
    b.type = 'button';
    b.className = 'popup-item';
    b.setAttribute('role', 'menuitem');
    b.innerHTML = `<span class="pi-line">${icon(iconName)}<span class="pi-name"></span></span>`;
    b.querySelector('.pi-name').textContent = label;
    b.addEventListener('click', () => { closeWfTreeMenu(); run(); });
    menu.appendChild(b);
  }
  document.body.appendChild(menu);
  menu.style.left = `${Math.max(8, Math.min(x, window.innerWidth - menu.offsetWidth - 8))}px`;
  menu.style.top = `${Math.max(8, Math.min(y, window.innerHeight - menu.offsetHeight - 8))}px`;
  menu.addEventListener('keydown', (ev) => {
    const btns = [...menu.querySelectorAll('button')];
    const i = btns.indexOf(document.activeElement);
    if (ev.key === 'ArrowDown') { ev.preventDefault(); btns[(i + 1) % btns.length].focus(); }
    else if (ev.key === 'ArrowUp') { ev.preventDefault(); btns[(i - 1 + btns.length) % btns.length].focus(); }
    else if (ev.key === 'Escape') { ev.preventDefault(); closeWfTreeMenu(); }
  });
  menu.querySelector('button').focus();
  setTimeout(() => window.addEventListener('mousedown', wfTreeMenuOutside), 0);
}

function closeWfTreeMenu() {
  $('#wf-tree-menu')?.remove();
  window.removeEventListener('mousedown', wfTreeMenuOutside);
}

function wfTreeMenuOutside(ev) {
  if (!ev.target.closest('#wf-tree-menu')) closeWfTreeMenu();
}

/** Renders a group node into the panel: its direct workflows (reorderable
 * among themselves), then a header + recursion for each subgroup. A heading
 * folds its subgroup — the same state the tree uses, so both stay in step. */
function renderWorkflowNode(list, node, depth) {
  node.workflows.forEach((w, i) => {
    list.appendChild(workflowCard(w, {
      moveUp: i > 0,
      moveDown: i < node.workflows.length - 1,
      // Always draggable, even alone in its group: the tree takes a dropped
      // card into any other group, so a lone card still has somewhere to go.
      draggable: true,
    }));
  });
  for (const [, child] of node.groups) {
    const folded = wfUi.collapsed.has(child.path);
    const foldable = child.groups.size > 0 || child.workflows.length > 0;
    const head = document.createElement('li');
    head.className = 'wf-group-head wf-collapsible';
    head.style.setProperty('--depth', depth);
    head.innerHTML = `<button type="button" class="wf-twist" aria-expanded="${!folded}">${icon(folded ? 'chevronRight' : 'chevronDown')}</button>${icon('folder')}<span class="wf-group-name"></span><span class="wf-group-count"></span>`;
    head.querySelector('.wf-twist').title = tr('wf.group.toggle');
    head.querySelector('.wf-group-name').textContent = child.name;
    head.querySelector('.wf-group-count').textContent = countWorkflows(child);
    if (foldable) head.addEventListener('click', () => toggleWfCollapsed(child.path));
    list.appendChild(head);
    if (!folded) renderWorkflowNode(list, child, depth + 1);
  }
}

function renderWorkflows() {
  const q = $('#wf-search').value.trim().toLowerCase();
  const list = $('#wf-list');
  list.innerHTML = '';
  closeWfTreeMenu();
  const root = buildWorkflowTree(state.workflows);
  resolveWfSelection(root);
  // With nothing registered there are no groups to browse: drop the tree and
  // let the empty state take the whole panel.
  const any = state.workflows.length > 0;
  $('#wf-tree').hidden = !any;
  $('#wf-layout').classList.toggle('no-tree', !any);
  if (!any) {
    list.appendChild(emptyState('zap', tr('wf.emptyTitle'),
      tr('wf.emptyHint'),
      { label: tr('wf.emptyCta'), onClick: () => editWorkflow(null) }));
    return;
  }
  renderWorkflowTree(root);
  // Searching flattens across groups (reordering makes no sense in a filtered
  // view) and shows each match's group as a badge. It stays inside the node
  // picked in the tree; "All" searches everything.
  if (q) {
    const items = state.workflows.filter((w) => inWfSelection(w) &&
      [w.name, w.description, w.command, w.group, (w.tags || []).join(' ')]
        .join(' ').toLowerCase().includes(q));
    if (!items.length) {
      list.appendChild(emptyState('search', tr('wf.searchNoMatchTitle'),
        tr('search.noMatch', { q })));
      return;
    }
    for (const w of items) list.appendChild(workflowCard(w, { showGroup: true }));
    return;
  }
  // Unfiltered: the selected subtree, with per-group reordering.
  const sel = wfUi.selected;
  if (sel === WF_ALL) {
    renderWorkflowNode(list, root, 0);
    return;
  }
  const scope = document.createElement('li');
  scope.className = 'wf-scope-head';
  const node = sel === WF_NONE ? null : findGroupNode(root, sel.slice(2));
  scope.innerHTML = `${icon('folder')}<span class="wf-scope-name"></span><span class="wf-group-count"></span>`;
  scope.querySelector('.wf-scope-name').textContent =
    node ? node.path.split('/').join(' / ') : tr('wf.tree.ungrouped');
  scope.querySelector('.wf-group-count').textContent = node ? countWorkflows(node) : root.workflows.length;
  list.appendChild(scope);
  // "Ungrouped" is the root's own workflows without its subgroups.
  renderWorkflowNode(list, node || { workflows: root.workflows, groups: new Map() }, 0);
}

/** Opens the workflow editor prefilled with a command that was run — from the
 * history, a finished block, or the right-click menu — so turning "that thing
 * I typed" into a reusable template is one step instead of a retype. The
 * name defaults to the command's first words; the editor is where it gets a
 * real one. */
function saveAsWorkflow(command) {
  const cmd = (command || '').trim();
  if (!cmd) return;
  if (currentView !== 'workflows') setView('workflows');
  const name = cmd.split(/\s+/).slice(0, 3).join(' ').slice(0, 40);
  editWorkflow({ id: '', name, description: '', command: cmd, tags: [], group: '', shortcut: '', show_button: false });
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
  // Built-ins are resolved here, not in the backend: the answers live in the
  // UI (the focused thread, its host, the clock, the clipboard). Only the
  // ones the template names are computed, so a template that never mentions
  // {{$clipboard}} never reads the clipboard.
  const builtins = await invoke('workflow_builtins', { command: w.command });
  if (builtins.length) values = { ...(await builtinValues(builtins)), ...values };
  const filled = await invoke('fill_workflow', { command: w.command, values });
  sendToActive(filled, run);
}

/** Values for the `$`-variables in `names`, from the focused thread and the
 * clock. Anything unknown (no thread, a local thread asked for {{$host}}, a
 * clipboard that refuses) is '' — the template's own default then applies. */
async function builtinValues(names) {
  const thread = focusedThread();
  const host = thread && thread.kind === 'ssh' ? hostById(thread.sshHostId) : null;
  const now = new Date();
  const pad = (n) => String(n).padStart(2, '0');
  const date = `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}`;
  const time = `${pad(now.getHours())}:${pad(now.getMinutes())}:${pad(now.getSeconds())}`;
  const out = {};
  for (const name of names) {
    let v = '';
    switch (name) {
      case '$cwd': v = thread?.cwd || ''; break;
      case '$host': v = host?.host || ''; break;
      case '$user': v = host?.username || ''; break;
      case '$port': v = host ? String(host.port) : ''; break;
      case '$thread': v = thread?.title || ''; break;
      case '$date': v = date; break;
      case '$time': v = time; break;
      // Filesystem-safe: for log and backup names.
      case '$datetime': v = `${date}_${time.replace(/:/g, '-')}`; break;
      case '$timestamp': v = String(Math.floor(now.getTime() / 1000)); break;
      case '$selection': v = thread?.term?.getSelection?.() || ''; break;
      case '$clipboard':
        try { v = await navigator.clipboard.readText(); } catch { v = ''; }
        break;
      default: v = '';
    }
    // Nothing known: leave the key out rather than send '', so the template's
    // own default ({{$cwd:~}}) still applies — a present empty value would
    // win over it.
    if (v !== '') out[name] = v;
  }
  return out;
}

function editWorkflow(w) {
  // A draft from "save as workflow" has fields but no id: still a new one.
  const isNew = !w || !w.id;
  openModal({
    title: isNew ? tr('wf.editTitle.new') : tr('wf.editTitle.edit'),
    okLabel: tr('common.save'),
    body: [
      fieldSection(tr('section.basic')),
      field(tr('wf.field.name'), 'name', w?.name || '', { required: true }),
      field(tr('wf.field.desc'), 'description', w?.description || ''),
      field(tr('wf.field.group'), 'group', w?.group || '',
        { list: existingGroups(), placeholder: tr('wf.group.ph') }),
      field(tr('wf.field.tags'), 'tags', (w?.tags || []).join(', ')),
      fieldSection(tr('wf.section.command')),
      fieldTextarea(tr('wf.field.template'), 'command', w?.command || '', { required: true }),
      fieldNote(tr('wf.builtins.hint')),
      fieldSection(tr('wf.section.launch')),
      fieldShortcut(tr('wf.field.shortcut'), 'shortcut', w?.shortcut || '', { excludeId: w?.id || '' }),
      fieldCheckbox(tr('wf.field.showButton'), 'show_button', !!w?.show_button),
    ],
    onOk: async (values) => {
      await invoke('save_workflow', {
        workflow: {
          id: w?.id || '',
          name: values.name.trim(),
          description: values.description.trim(),
          command: values.command,
          tags: values.tags.split(',').map((t) => t.trim()).filter(Boolean),
          group: normalizeGroup(values.group),
          shortcut: values.shortcut || '',
          show_button: !!values.show_button,
        },
      });
      refreshWorkflows();
    },
  });
}

/** Exports every workflow to a user-chosen JSON file (shareable envelope,
 * ids stripped — see senju-core::share). */
async function exportWorkflows() {
  try {
    const path = await window.__TAURI__.dialog.save({
      defaultPath: 'senju-workflows.json',
      filters: [{ name: 'JSON', extensions: ['json'] }],
    });
    if (!path) return; // cancelled
    const count = await invoke('export_workflows', { path });
    toast(tr('wf.export.done', { count }));
  } catch (e) {
    toast(tr('wf.export.failed', { e }), true);
  }
}

/** Imports workflows from an export file: the backend parses candidates, the
 * user picks them in a checklist (existing name+command pairs are labeled as
 * already added and start unchecked), and each picked one is saved through
 * the normal save_workflow path with a fresh id. Conflicting shortcuts are
 * dropped so an import can never steal an existing binding. */
/** Writes the whole configuration to a file the user picks. Credentials are
 * stripped by the backend, which is what makes this file safe to share. */
async function exportSettings() {
  try {
    const path = await window.__TAURI__.dialog.save({
      defaultPath: 'senju-settings.json',
      filters: [{ name: 'JSON', extensions: ['json'] }],
    });
    if (!path) return; // cancelled
    await invoke('export_settings', { path });
    toast(tr('settings.export.done'));
  } catch (e) {
    toast(tr('settings.export.failed', { e }), true);
  }
}

/** Imports a settings file, showing what it holds before applying any of it.
 * Reading and applying are separate calls so the confirmation describes the
 * real file rather than what the user hoped was in it. */
async function importSettings() {
  let path;
  let preview;
  try {
    path = await window.__TAURI__.dialog.open({
      multiple: false,
      directory: false,
      filters: [{ name: 'JSON', extensions: ['json'] }],
    });
    if (!path) return; // cancelled
    preview = await invoke('read_settings_file', { path: String(path) });
  } catch (e) {
    toast(tr('settings.import.failed', { e }), true);
    return;
  }
  const body = [{ tag: 'note', label: tr('settings.import.summary', preview) }];
  if (preview.settings) body.push({ tag: 'note', label: tr('settings.import.settingsToo') });
  body.push({ tag: 'note', label: tr('settings.backup.secrets') });
  openModal({
    title: tr('settings.import.confirm'),
    okLabel: tr('settings.import'),
    body,
    onOk: async () => {
      try {
        await invoke('import_settings', { path: String(path) });
      } catch (e) {
        toast(tr('settings.import.failed', { e }), true);
        return;
      }
      // Everything the import touched is cached in `state`, so re-read rather
      // than patching each list by hand.
      await Promise.all([
        loadSettings(), refreshWorkflows(), refreshHosts(),
        refreshProfiles(), refreshLaunchSets(), refreshServers(),
      ]);
      applyTheme();
      applyLanguage();
      toast(tr('settings.import.done'));
    },
  });
}

async function importWorkflows() {
  let candidates;
  try {
    const path = await window.__TAURI__.dialog.open({
      multiple: false,
      filters: [{ name: 'JSON', extensions: ['json'] }],
    });
    if (!path) return; // cancelled
    candidates = (await invoke('read_workflows_file', { path: Array.isArray(path) ? path[0] : path })) || [];
  } catch (e) {
    toast(tr('wf.import.failed', { e }), true);
    return;
  }
  if (!candidates.length) {
    toast(tr('wf.import.none'), true);
    return;
  }
  const isDup = (c) => state.workflows.some((w) => w.name === c.name && w.command === c.command);
  const body = [fieldNote(tr('wf.import.hint'))];
  candidates.forEach((c, i) => {
    const dup = isDup(c);
    const label = `${c.name}${groupParts(c).length ? ` (${groupParts(c).join('/')})` : ''} — ${c.command}`
      + (dup ? ` ${tr('wf.import.dup')}` : '');
    body.push(fieldCheckbox(label, `imp_${i}`, !dup));
  });
  openModal({
    title: tr('wf.import.title'),
    okLabel: tr('wf.import.ok'),
    body,
    onOk: async (values) => {
      let count = 0;
      for (const [i, c] of candidates.entries()) {
        if (!values[`imp_${i}`]) continue;
        const workflow = { ...c, id: '' };
        if (workflow.shortcut && shortcutConflict(workflow.shortcut, '')) workflow.shortcut = '';
        await invoke('save_workflow', { workflow });
        count++;
      }
      await refreshWorkflows();
      toast(tr('wf.import.done', { count }));
    },
  });
}

async function promptPlaceholders(w, placeholders) {
  // What was typed last time, as this time's defaults. A template default
  // still applies to anything not remembered. Only saved workflows have an
  // id to remember under; an ad-hoc run just gets the template's defaults.
  let remembered = {};
  if (w.id) {
    try {
      remembered = (await invoke('get_workflow_inputs', { id: w.id })) || {};
    } catch {
      remembered = {};
    }
  }
  return new Promise((resolve) => {
    // A `{{name|a,b,c}}` placeholder renders as a dropdown limited to its
    // options. Preselected: what was remembered, then the explicit
    // `:default`, then the first option — each only if it is one of them.
    const body = placeholders.map((p) => {
      const opts = Array.isArray(p.options) ? p.options : [];
      if (opts.length) {
        const pick = [remembered[p.name], p.default].find((v) => opts.includes(v)) ?? opts[0];
        return fieldSelect(p.name, `ph_${p.name}`, pick, opts.map((o) => [o, o]));
      }
      return field(p.name, `ph_${p.name}`, remembered[p.name] ?? p.default ?? '');
    });
    if (w.id) {
      body.push(fieldCheckbox(tr('wf.param.remember'), 'remember', true));
      body.push(fieldNote(tr('wf.param.remember.hint')));
    }
    openModal({
      title: tr('wf.paramTitle', { name: w.name }),
      okLabel: tr('common.run'),
      body,
      onOk: (values) => {
        const out = {};
        for (const p of placeholders) out[p.name] = values[`ph_${p.name}`];
        if (w.id) {
          // Unticked = forget (an empty map clears the record).
          invoke('save_workflow_inputs', { id: w.id, values: values.remember ? out : {} }).catch(() => {});
        }
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
    toast(tr('host.loadFailed', { e }), true);
  }
  renderHosts();
  // A host's color is painted on its open threads and panes, so an edit here
  // has to reach the thread list as well.
  renderThreads();
}

const HOST_COLORS = ['red', 'orange', 'yellow', 'green', 'blue', 'purple'];

function hostById(id) {
  return state.hosts.find((h) => h.id === id);
}

/** Label color of the host a thread is connected to; '' for local threads
 * and for hosts without one. */
function threadColor(t) {
  return (t && t.kind === 'ssh' && hostById(t.sshHostId)?.color) || '';
}

/** The open SSH thread for a host, if any (the first, when there are several). */
function openThreadFor(hostId) {
  return state.threads.find((t) => t.kind === 'ssh' && t.sshHostId === hostId);
}

const AUTH_LABEL_KEY = { password: 'auth.password', key: 'auth.key', agent: 'auth.agent', keypassword: 'auth.keypassword' };
const authLabel = (m) => (AUTH_LABEL_KEY[m] ? tr(AUTH_LABEL_KEY[m]) : m);

function renderHosts() {
  const q = $('#host-search').value.trim().toLowerCase();
  const list = $('#host-list');
  list.innerHTML = '';
  if (!state.hosts.length) {
    list.appendChild(emptyState('server', tr('host.emptyTitle'),
      tr('host.emptyHint'),
      { label: tr('host.emptyCta'), onClick: () => editHost(null) }));
    return;
  }
  // Searching flattens across groups and shows each match's group as a badge,
  // the same as the workflows panel.
  if (q) {
    const items = state.hosts.filter((h) =>
      [h.name, h.host, h.username, h.group, (h.tags || []).join(' ')]
        .join(' ').toLowerCase().includes(q));
    if (!items.length) {
      list.appendChild(emptyState('search', tr('host.searchNoMatchTitle'),
        tr('host.searchNoMatch', { q })));
      return;
    }
    for (const h of items) list.appendChild(hostCard(h, { showGroup: true }));
    return;
  }
  // buildWorkflowTree only reads `.group`, so hosts fit it as they are.
  renderHostNode(list, buildWorkflowTree(state.hosts), 0);
}

/** Hosts directly under a node, then each child group under its own heading.
 * A heading carries the two things a group is for — opening every host in
 * it, and sending one command to all of them — because those act on the
 * group as a unit, and the group is the only place that unit is visible. */
function renderHostNode(list, node, depth) {
  for (const h of node.workflows) list.appendChild(hostCard(h));
  for (const [, child] of node.groups) {
    const head = document.createElement('li');
    head.className = 'wf-group-head host-group-head';
    head.style.setProperty('--depth', depth);
    head.innerHTML = `${icon('folder')}<span class="wf-group-name"></span><span class="wf-group-count"></span>
      <span class="hg-actions">
        <button class="ghost-btn hg-connect">${icon('login')}<span></span></button>
        <button class="ghost-btn hg-run">${icon('play')}<span></span></button>
      </span>`;
    head.querySelector('.wf-group-name').textContent = child.name;
    head.querySelector('.wf-group-count').textContent = countWorkflows(child);
    const hosts = hostsUnder(child);
    const connect = head.querySelector('.hg-connect');
    connect.querySelector('span').textContent = tr('host.group.connectAll');
    connect.title = tr('host.group.connectAll.title');
    connect.addEventListener('click', () => connectHosts(child.path, hosts));
    const run = head.querySelector('.hg-run');
    run.querySelector('span').textContent = tr('host.group.runAll');
    run.title = tr('host.group.runAll.title');
    run.addEventListener('click', () => runOnHosts(child.path, hosts));
    list.appendChild(head);
    renderHostNode(list, child, depth + 1);
  }
}

/** Every host in a group node and its subgroups — what "all" means for the
 * group's connect-all and run-on-all. */
function hostsUnder(node) {
  const out = [...node.workflows];
  for (const [, c] of node.groups) out.push(...hostsUnder(c));
  return out;
}

function hostCard(h, opts = {}) {
  const li = document.createElement('li');
  li.className = 'card';
  if (h.color) li.dataset.color = h.color;
  li.innerHTML = `
      <h4><span class="host-dot"></span><span class="host-name"></span></h4>
      <div class="host-conn"></div>
      <div class="host-badges"><span class="auth-badge">${icon('lock')}<span class="auth-label"></span></span></div>
      <div class="row">
        <button class="accent-btn connect">${icon('login')}${tr('common.connect')}</button>
        <button class="ghost-btn install">${icon('lock')}${tr('host.install')}</button>
        <button class="ghost-btn edit">${icon('edit')}${tr('common.edit')}</button>
        <button class="danger-btn del">${icon('trash')}${tr('common.delete')}</button>
      </div>`;
  li.querySelector('.host-name').textContent = h.name || `${h.username}@${h.host}`;
  li.querySelector('.install').addEventListener('click', () => installPublicKey(h));
  if (!h.color) li.querySelector('.host-dot').remove();
  li.querySelector('.host-conn').textContent = `${h.username}@${h.host}:${h.port}`;
  li.querySelector('.auth-label').textContent = authLabel(h.auth_method);
  const badges = li.querySelector('.host-badges');
  if (opts.showGroup && groupParts(h).length) {
    const g = document.createElement('span');
    g.className = 'wf-badge wf-group-badge';
    g.innerHTML = `${icon('folder')}<span></span>`;
    g.querySelector('span').textContent = groupParts(h).join(' / ');
    badges.appendChild(g);
  }
  for (const tag of h.tags || []) {
    const t = document.createElement('span');
    t.className = 'tag';
    t.textContent = tag;
    badges.appendChild(t);
  }
  li.querySelector('.connect').addEventListener('click', () => newSshThread(h));
  li.querySelector('.edit').addEventListener('click', () => editHost(h));
  li.querySelector('.del').addEventListener('click', async () => {
    if (!(await confirmModal(tr('confirm.delete', { name: h.name || h.host })))) return;
    await invoke('delete_ssh_host', { id: h.id });
    refreshHosts();
  });
  return li;
}

/** The known_hosts file as a checklist, for pruning stale entries. The only
 * action is removal: adding is what connecting does, and editing a line by
 * hand is how people end up trusting the wrong key. */
async function openKnownHosts() {
  let entries;
  try {
    entries = await invoke('list_known_hosts');
  } catch (e) {
    toast(String(e), true);
    return;
  }
  const body = [fieldNote(tr('host.knownHosts.hint'))];
  if (!entries.length) body.push(fieldNote(tr('host.knownHosts.empty')));
  for (const e of entries) {
    const host = e.hashed ? tr('host.knownHosts.hashed') : e.hosts;
    const key = e.fingerprint || tr('host.knownHosts.unparsed');
    const label = `${tr('host.knownHosts.line', { line: e.line })} · ${e.marker ? `${e.marker} ` : ''}${host} · ${e.key_type} ${key}`;
    body.push(fieldCheckbox(label, `kh_${e.line}`, false));
  }
  openModal({
    title: tr('host.knownHosts.title'),
    okLabel: tr('host.knownHosts.ok'),
    body,
    onOk: async (values) => {
      const lines = entries.filter((e) => values[`kh_${e.line}`]).map((e) => e.line);
      if (!lines.length) throw tr('host.knownHosts.none');
      const n = await invoke('remove_known_hosts', { lines });
      toast(tr('host.knownHosts.removed', { n }));
    },
  });
}

function openKeygen() {
  openModal({
    title: tr('host.keygen.title'),
    okLabel: tr('host.keygen.ok'),
    body: [
      field(tr('host.keygen.name'), 'name', 'id_ed25519', { required: true }),
      field(tr('host.keygen.comment'), 'comment', 'senju-term'),
      field(tr('host.keygen.passphrase'), 'passphrase', '', { type: 'password' }),
      field(tr('host.keygen.passphrase2'), 'passphrase2', '', { type: 'password' }),
      fieldNote(tr('host.keygen.hint')),
    ],
    onOk: async (v) => {
      if ((v.passphrase || '') !== (v.passphrase2 || '')) throw tr('host.keygen.mismatch');
      const g = await invoke('generate_ssh_key', {
        name: v.name.trim(), comment: v.comment.trim(), passphrase: v.passphrase || null,
      });
      // Shown after this modal closes (submitModal clears the body).
      setTimeout(() => showGeneratedKey(g), 0);
    },
  });
}

function showGeneratedKey(g) {
  openModal({
    title: tr('host.keygen.done.title'),
    okLabel: tr('common.close'),
    hideCancel: true,
    body: [
      fieldNote(tr('host.keygen.done.paths', { priv: g.private_path, pub: g.public_path })),
      fieldNote(tr('host.keygen.done.fp', { fp: g.fingerprint })),
      fieldTextarea(tr('host.keygen.done.pub'), 'pub', g.public_key),
      fieldNote(tr('host.keygen.done.next')),
    ],
    extraActions: [{
      label: tr('host.keygen.done.copy'),
      className: 'ghost-btn',
      onClick: async ({ setStatus }) => {
        try {
          await navigator.clipboard.writeText(g.public_key);
          setStatus(tr('host.keygen.copied'), 'ok');
        } catch (e) {
          setStatus(String(e), 'err');
        }
      },
    }],
    onOk: () => {},
  });
}

/** ssh-copy-id over the open session's SFTP: pick a local .pub, append it to
 * the remote authorized_keys, and optionally switch the host to key auth so
 * the password that got us in is the last one typed. */
async function installPublicKey(h) {
  const thread = openThreadFor(h.id);
  if (!thread) {
    toast(tr('host.install.needSession'), true);
    return;
  }
  let keys;
  try {
    keys = await invoke('list_public_keys');
  } catch (e) {
    toast(String(e), true);
    return;
  }
  if (!keys.length) {
    toast(tr('host.install.noKeys'), true);
    return;
  }
  const base = (p) => p.split(/[\\/]/).pop();
  const options = keys.map((k) => [k.path, `${base(k.path)} · ${k.key_type} ${k.fingerprint}${k.comment ? ` · ${k.comment}` : ''}`]);
  // Default to the key the host is already configured with, if it has one.
  const preferred = keys.find((k) => h.key_path && k.path === `${h.key_path}.pub`)?.path || keys[0].path;
  const host = h.name || h.host;
  openModal({
    title: tr('host.install.title', { host }),
    okLabel: tr('host.install.ok'),
    body: [
      fieldNote(tr('host.install.hint')),
      fieldSelect(tr('host.install.key'), 'pub', preferred, options),
      fieldCheckbox(tr('host.install.switch'), 'switch', h.auth_method !== 'key' && h.auth_method !== 'keypassword'),
    ],
    onOk: async (v) => {
      const added = await invoke('install_public_key', { sessionId: thread.id, pubPath: v.pub });
      toast(tr(added ? 'host.install.added' : 'host.install.present', { host }));
      if (v.switch) {
        const keyPath = v.pub.replace(/\.pub$/, '');
        // Blank password/passphrase mean "keep what is saved" (see
        // save_ssh_host), so spreading the redacted host is safe.
        await invoke('save_ssh_host', { host: { ...h, auth_method: 'key', key_path: keyPath } });
        await refreshHosts();
        toast(tr('host.install.switched', { path: keyPath }));
      }
    },
  });
}

/** Opens a session to every host in `hosts` that has none yet, one after
 * another: a connect may prompt (secrets, an unknown host key), and starting
 * them all at once would stack those prompts on top of each other. Hosts that
 * are already open are skipped — "connect all" means "make sure they are all
 * up", not "open a second session to each". */
async function connectHosts(group, hosts) {
  const pending = hosts.filter((h) => !openThreadFor(h.id));
  const skipped = hosts.length - pending.length;
  if (!pending.length) {
    toast(tr('host.connectAll.allOpen', { group }));
    return;
  }
  if (skipped) toast(tr('host.connectAll.skipped', { n: skipped }));
  if (currentView !== 'shell') setView('shell');
  for (const h of pending) await newSshThread(h);
}

/** One command to every connected host of a group. Hosts without an open
 * session are shown but cannot be targets: connecting for them here would
 * mean a prompt per host in the middle of what was meant as one action, and
 * a command typed into a shell that has not finished logging in goes nowhere
 * useful. The target list is a checklist so one box can be left out of a
 * group-wide command without leaving the group. */
function runOnHosts(group, hosts) {
  const targets = hosts.map((h) => ({ h, thread: openThreadFor(h.id) }));
  if (!targets.some((x) => x.thread)) {
    toast(tr('host.run.none', { group }), true);
    return;
  }
  const body = [
    fieldTextarea(tr('host.run.command'), 'command', '', { required: true }),
    fieldSection(tr('host.run.targets')),
  ];
  for (const { h, thread } of targets) {
    const label = h.name || `${h.username}@${h.host}`;
    if (thread) body.push(fieldCheckbox(label, `t_${h.id}`, true));
    else body.push(fieldNote(`${label} — ${tr('host.run.notConnected')}`));
  }
  openModal({
    title: tr('host.run.title', { group }),
    okLabel: tr('host.run.ok'),
    body,
    onOk: (values) => {
      const chosen = targets.filter((x) => x.thread && values[`t_${x.h.id}`]);
      // A string, not an Error: the modal shows String(e) inline.
      if (!chosen.length) throw tr('host.run.noneChecked');
      for (const { thread } of chosen) sendToThread(thread, values.command, true);
      toast(tr('host.run.sent', { n: chosen.length }));
      if (currentView !== 'shell') setView('shell');
    },
  });
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
    save_credentials: !!values.save_credentials,
    forwards: (values.forwards || '')
      .split('\n')
      .map((l) => l.trim())
      .filter(Boolean),
    jump_hosts: values.jump_hosts || [],
    group: normalizeGroup(values.group),
    tags: (values.tags || '').split(',').map((t) => t.trim()).filter(Boolean),
    color: values.color || '',
    log_sessions: !!values.log_sessions,
    auto_reconnect: !!values.auto_reconnect,
  });
  openModal({
    title: isNew ? tr('host.editTitle.new') : tr('host.editTitle.edit'),
    okLabel: tr('common.save'),
    body: [
      fieldSection(tr('host.section.dest')),
      field(tr('host.field.name'), 'name', h?.name || ''),
      field(tr('host.field.host'), 'host', h?.host || '', { required: true, placeholder: 'example.com' }),
      field(tr('host.field.port'), 'port', String(h?.port ?? 22), { type: 'number' }),
      field(tr('host.field.username'), 'username', h?.username || '', { required: true }),
      fieldSection(tr('host.section.auth')),
      fieldSelect(tr('host.field.authMethod'), 'auth_method', h?.auth_method || 'password', [
        ['password', tr('auth.password')], ['key', tr('auth.keyfile')], ['agent', tr('auth.agent')],
        ['keypassword', tr('auth.keypassword2')],
      ]),
      field(tr('host.field.keyPath'), 'key_path', h?.key_path || '', { placeholder: '~/.ssh/id_ed25519' }),
      fieldCheckbox(tr('host.field.saveCreds'), 'save_credentials', !!h?.save_credentials),
      fieldNote(tr('host.field.saveCreds.hint')),
      fieldSection(tr('host.section.forwards')),
      fieldTextarea(tr('host.field.forwards'), 'forwards', (h?.forwards || []).join('\n'), {
        placeholder: '8080:localhost:80\n-R 9000:localhost:3000\n-D 1080',
      }),
      fieldNote(tr('host.field.forwards.hint')),
      fieldSection(tr('host.section.jump')),
      { tag: 'jumplist', name: 'jump_hosts', label: tr('host.field.jump'), items: h?.jump_hosts || [], selfId: h?.id || '' },
      fieldNote(tr('host.field.jump.hint')),
      fieldSection(tr('host.section.label')),
      field(tr('host.field.group'), 'group', h?.group || '',
        { list: existingGroups(state.hosts), placeholder: tr('host.group.ph') }),
      field(tr('host.field.tags'), 'tags', (h?.tags || []).join(', ')),
      fieldSelect(tr('host.field.color'), 'color', h?.color || '',
        [['', tr('host.color.none')], ...HOST_COLORS.map((c) => [c, tr(`host.color.${c}`)])]),
      fieldNote(tr('host.field.color.hint')),
      fieldSection(tr('host.section.log')),
      fieldCheckbox(tr('host.field.logSessions'), 'log_sessions', !!h?.log_sessions),
      fieldNote(tr('host.field.logSessions.hint')),
      fieldSection(tr('host.section.reconnect')),
      fieldCheckbox(tr('host.field.autoReconnect'), 'auto_reconnect', !!h?.auto_reconnect),
      fieldNote(tr('host.field.autoReconnect.hint')),
      fieldSection(tr('host.section.test')),
      field(tr('host.field.testPassword'), 'test_password', '', { type: 'password' }),
      field(tr('host.field.testPassphrase'), 'test_passphrase', '', { type: 'password' }),
    ],
    // The connection test uses the SAME fail-closed TOFU handshake as a real
    // connect: on first contact with an unknown host it does NOT send the
    // password — it reports the fingerprint and, on a second press, re-tests
    // trusting exactly that fingerprint (which is required to exercise auth).
    extraActions: [{
      label: tr('host.test'),
      className: 'ghost-btn',
      onClick: async ({ values, setStatus, btn }) => {
        if (!values.host.trim() || !values.username.trim()) {
          setStatus(tr('host.test.needHostUser'), 'err');
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
        setStatus(tr('host.test.running'), '');
        try {
          const rep = await invoke('test_ssh_connection', args);
          const key = rep.host_key_known
            ? tr('host.test.keyMatch')
            : tr('host.test.keyApproved', { type: rep.key_type, fp: rep.fingerprint });
          setStatus(tr('host.test.ok', { key }), 'ok');
          testTrustFingerprint = null;
          btn.textContent = tr('host.test');
        } catch (e) {
          const msg = String(e);
          const err = parseHostKeyError(msg);
          if (err && err.tag === 'UNKNOWN_HOST_KEY' && !testTrustFingerprint) {
            // First contact — no credential was sent. Show the fingerprint;
            // pressing the button again trusts it for the auth test only.
            testTrustFingerprint = err.fingerprint;
            btn.textContent = tr('host.test.trustBtn');
            setStatus(tr('host.test.newKey', { type: err.keyType, fp: err.fingerprint }), 'ok');
          } else if (err && (err.tag === 'FINGERPRINT_MISMATCH' || err.tag === 'HOST_KEY_MISMATCH')) {
            testTrustFingerprint = null;
            btn.textContent = tr('host.test');
            setStatus(tr('host.test.mitm'), 'err');
          } else {
            testTrustFingerprint = null;
            btn.textContent = tr('host.test');
            setStatus(tr('host.test.failed', { msg }), 'err');
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

/** Imports hosts from ~/.ssh/config: the backend parses the file into
 * candidates, the user checks the ones to add, and each is saved through the
 * normal save_ssh_host path. Candidates matching an existing entry
 * (host+port+username) start unchecked and are labeled as already added. */
async function importSshConfig() {
  let candidates;
  try {
    candidates = (await invoke('read_ssh_config')) || [];
  } catch (e) {
    toast(tr('host.import.failed', { e }), true);
    return;
  }
  if (!candidates.length) {
    toast(tr('host.import.none'), true);
    return;
  }
  const isDup = (c) => state.hosts.some(
    (h) => h.host === c.host && h.port === c.port && h.username === c.username);
  const body = [fieldNote(tr('host.import.hint'))];
  candidates.forEach((c, i) => {
    const dup = isDup(c);
    const label = `${c.name} — ${c.username}@${c.host}:${c.port} · ${authLabel(c.auth_method)}`
      + (dup ? ` ${tr('host.import.dup')}` : '');
    body.push(fieldCheckbox(label, `imp_${i}`, !dup));
  });
  openModal({
    title: tr('host.import.title'),
    okLabel: tr('host.import.ok'),
    body,
    onOk: async (values) => {
      let count = 0;
      for (const [i, c] of candidates.entries()) {
        if (!values[`imp_${i}`]) continue;
        await invoke('save_ssh_host', { host: c });
        count++;
      }
      await refreshHosts();
      toast(tr('host.import.done', { count }));
    },
  });
}

function sshKindLabel(host) {
  if (host.auth_method === 'keypassword') return tr('secret.kind.combined');
  if (host.auth_method === 'key') return tr('secret.kind.key');
  return tr('secret.kind.password');
}

/** Collects per-hop secrets for a connection chain `[jump1, .., target]` in
 * one modal, keyed by host id: `{ [hostId]: {password?, passphrase?} }`, or
 * null when cancelled. Hosts using ssh-agent need no secret and are skipped;
 * if every hop uses the agent the modal is skipped entirely. Secrets are used
 * for the one connection only and never persisted. Field names are namespaced
 * by hop index so multiple hops' fields don't collide. */
function promptChainSecrets(chainHosts, saved = {}) {
  // A hop is not asked when it authenticates by agent, or when its
  // credentials are already saved — in the second case the backend supplies
  // them, so this side sends nothing for that hop at all.
  const asks = (h) => h.auth_method !== 'agent' && !saved[h.id];
  const needing = chainHosts.filter(asks);
  if (needing.length === 0) return Promise.resolve({});
  const multi = chainHosts.length > 1;
  const lastIdx = chainHosts.length - 1;
  return new Promise((resolve) => {
    const body = [];
    const meta = [];
    chainHosts.forEach((host, idx) => {
      if (!asks(host)) return;
      const isKey = host.auth_method === 'key';
      const isCombined = host.auth_method === 'keypassword';
      if (multi) {
        body.push(fieldSection(tr('secret.hop', {
          role: idx === lastIdx ? tr('secret.role.target') : tr('secret.role.jump'),
          name: host.name || host.host,
        })));
      }
      if (isCombined) {
        body.push(field(tr('secret.passphraseOpt'), `h${idx}_passphrase`, '', { type: 'password' }));
        body.push(field(tr('secret.userPassword', { user: host.username }), `h${idx}_password`, '', { type: 'password' }));
      } else {
        body.push(field(isKey ? tr('secret.passphraseOnly') : tr('secret.userPassword', { user: host.username }),
          `h${idx}_secret`, '', { type: 'password' }));
      }
      meta.push({ id: host.id, idx, isKey, isCombined });
    });
    openModal({
      title: multi
        ? tr('secret.chainTitle', { name: chainHosts[lastIdx].name || chainHosts[lastIdx].host })
        : tr('secret.title', { name: needing[0].name || needing[0].host, kind: sshKindLabel(needing[0]) }),
      okLabel: tr('common.connect'),
      body,
      onOk: (values) => {
        const out = {};
        for (const m of meta) {
          if (m.isCombined) {
            out[m.id] = { passphrase: values[`h${m.idx}_passphrase`] || null, password: values[`h${m.idx}_password`] || null };
          } else if (m.isKey) {
            out[m.id] = { passphrase: values[`h${m.idx}_secret`] || null };
          } else {
            out[m.id] = { password: values[`h${m.idx}_secret`] || null };
          }
        }
        resolve(out);
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
    toast(tr('profile.loadFailed', { e }), true);
  }
  renderProfiles();
  renderProfileSettingOptions();
}

/** Mirrors the backend's `Store::resolve_profile`: the requested id if it
 * exists, else the default profile, else the first one, else undefined. */
function resolveProfile(id) {
  const find = (x) => x && state.profiles.find((p) => p.id === x);
  return find(id) || find(state.settings.default_profile_id) || state.profiles[0];
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
    list.appendChild(emptyState('terminal', tr('profile.emptyTitle'),
      tr('profile.emptyHint'),
      { label: tr('profile.emptyCta'), onClick: () => editProfile(null) }));
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
        <button class="accent-btn launch">${icon('play')}${tr('common.launch')}</button>
        <button class="ghost-btn setdefault">${icon('star')}${tr('profile.setDefault')}</button>
        <button class="ghost-btn edit">${icon('edit')}${tr('common.edit')}</button>
        <button class="danger-btn del">${icon('trash')}${tr('common.delete')}</button>
      </div>`;
    li.querySelector('.star').innerHTML = def ? icon('star') : '';
    li.querySelector('.pname').textContent = p.name;
    const cmd = p.command || tr('profile.systemDefault');
    const argsText = (p.args || []).length ? ' ' + p.args.join(' ') : '';
    li.querySelector('.meta').textContent = cmd + argsText + (p.cwd ? `  ·  ${p.cwd}` : '');
    li.querySelector('.launch').addEventListener('click', () => newLocalThread({ profileId: p.id }));
    const setDefaultBtn = li.querySelector('.setdefault');
    setDefaultBtn.disabled = def;
    setDefaultBtn.addEventListener('click', () => setDefaultProfile(p.id));
    li.querySelector('.edit').addEventListener('click', () => editProfile(p));
    li.querySelector('.del').addEventListener('click', async () => {
      if (state.profiles.length <= 1) return toast(tr('profile.cantDeleteLast'), true);
      if (!(await confirmModal(tr('profile.confirmDelete', { name: p.name })))) return;
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
        toast(tr('toast.deleteFailed', { e }), true);
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
  toast(tr('profile.defaultChanged'));
}

function editProfile(p) {
  const isNew = !p;
  openModal({
    title: isNew ? tr('profile.editTitle.new') : tr('profile.editTitle.edit'),
    okLabel: tr('common.save'),
    body: [
      field(tr('host.field.name'), 'name', p?.name || '', { required: true }),
      field(tr('profile.field.command'), 'command', p?.command || '', { placeholder: '/bin/zsh, powershell.exe …' }),
      field(tr('profile.field.args'), 'args', (p?.args || []).join(' ')),
      field(tr('profile.field.cwd'), 'cwd', p?.cwd || '', { placeholder: '~/work' }),
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

/* ---------------- launch sets (batch-open shells + workflows) ---------------- */

async function refreshLaunchSets() {
  try {
    state.launchSets = await invoke('list_launch_sets');
  } catch (e) {
    toast(tr('set.loadFailed', { e }), true);
  }
  renderLaunchSets();
}

/** Human-readable "a → b → c" summary of a set's items, by profile/host name
 * (falling back to a "(削除済み)" label if the reference no longer exists). */
function launchSetSummary(set) {
  return set.items
    .map((it) => {
      if (it.ssh_host_id) {
        const h = state.hosts.find((x) => x.id === it.ssh_host_id);
        return h ? `⇄ ${h.name || h.host}` : tr('set.deletedRef.ssh');
      }
      const p = state.profiles.find((x) => x.id === it.profile_id);
      return p ? p.name : tr('set.deletedRef.profile');
    })
    .join(' → ');
}

function renderLaunchSets() {
  const list = $('#launchset-list');
  list.innerHTML = '';
  if (!state.launchSets.length) {
    list.appendChild(emptyState('layers', tr('set.emptyTitle'),
      tr('set.emptyHint'),
      { label: tr('set.emptyCta'), onClick: () => editLaunchSet(null) }));
    return;
  }
  for (const set of state.launchSets) {
    const li = document.createElement('li');
    li.className = 'card';
    li.innerHTML = `
      <h4></h4>
      <div class="desc"></div>
      <div class="row">
        <button class="accent-btn launch">${icon('play')}${tr('common.launch')}</button>
        <button class="ghost-btn edit">${icon('edit')}${tr('common.edit')}</button>
        <button class="danger-btn del">${icon('trash')}${tr('common.delete')}</button>
      </div>`;
    li.querySelector('h4').textContent = set.name;
    li.querySelector('.desc').textContent =
      tr('set.itemsSummary', { count: set.items.length, summary: launchSetSummary(set) });
    li.querySelector('.launch').addEventListener('click', () => runLaunchSet(set));
    li.querySelector('.edit').addEventListener('click', () => editLaunchSet(set));
    li.querySelector('.del').addEventListener('click', async () => {
      if (!(await confirmModal(tr('confirm.delete', { name: set.name })))) return;
      await invoke('delete_launch_set', { id: set.id });
      refreshLaunchSets();
    });
    list.appendChild(li);
  }
}

/** Opens every item in order (local shell or SSH connection), running that
 * item's workflow and/or direct command once its shell is actually ready. An
 * SSH item that needs a password still prompts (secrets are never persisted,
 * so this can't be skipped) — that pauses the batch on its modal like any
 * manual connect. If an item is cancelled or its profile/host was since
 * deleted, its whole step is skipped rather than firing its command at
 * whatever thread happens to be focused. */
async function runLaunchSet(set) {
  if (!set.items.length) return;
  setView('shell');
  for (const item of set.items) {
    const before = state.threads.length;
    if (item.ssh_host_id) {
      const host = state.hosts.find((h) => h.id === item.ssh_host_id);
      if (!host) {
        toast(tr('set.hostMissing'), true);
        continue;
      }
      await newSshThread(host);
    } else {
      if (item.profile_id && !state.profiles.some((p) => p.id === item.profile_id)) {
        toast(tr('set.profileMissing'), true);
        continue;
      }
      await newLocalThread({ profileId: item.profile_id || null });
    }
    if (state.threads.length === before) continue; // cancelled/failed — nothing auto-runs
    if (item.workflow_id) {
      const wf = state.workflows.find((w) => w.id === item.workflow_id);
      if (wf) await runWorkflow(wf, true);
    }
    // The item's own command runs after its workflow, so a set can pair a
    // saved workflow with a one-off follow-up. Routed through runWorkflow so
    // it gets the same {{placeholder}} prompting as a real workflow.
    if (item.command) {
      await runWorkflow({ name: set.name, command: item.command }, true);
    }
  }
  toast(tr('set.launched', { name: set.name }));
}

/** One repeatable row in the launch-set editor: which shell/host to open, and
 * which workflow and/or ad-hoc command (either, both, or neither) to auto-run
 * once it's ready. */
/** Encodes / decodes a target-<select> option value ("<kind>:<id>").
 *
 * The id can itself contain a colon — seeded profiles use stable ids like
 * `builtin:default` — so decoding splits on the FIRST colon only. A plain
 * `split(':')` truncated `profile:builtin:default` to the id `builtin`, which
 * matched no profile: the saved item pointed at nothing, so reopening the
 * editor showed an unselected shell and launching skipped the item. */
const targetValue = (kind, id) => `${kind}:${id}`;
function parseTargetValue(value) {
  const sep = value.indexOf(':');
  if (sep < 0) return { kind: '', id: '' };
  return { kind: value.slice(0, sep), id: value.slice(sep + 1) };
}

function buildItemRow(item) {
  const li = document.createElement('li');
  li.className = 'li-row';

  const targetSel = document.createElement('select');
  targetSel.className = 'li-target';
  const profileGroup = document.createElement('optgroup');
  profileGroup.label = tr('menu.localProfiles');
  for (const p of state.profiles) {
    const o = document.createElement('option');
    o.value = targetValue('profile', p.id);
    o.textContent = p.name;
    profileGroup.appendChild(o);
  }
  targetSel.appendChild(profileGroup);
  if (state.hosts.length) {
    const hostGroup = document.createElement('optgroup');
    hostGroup.label = tr('menu.sshHosts');
    for (const h of state.hosts) {
      const o = document.createElement('option');
      o.value = targetValue('ssh', h.id);
      o.textContent = h.name || h.host;
      hostGroup.appendChild(o);
    }
    targetSel.appendChild(hostGroup);
  }
  const current = item?.ssh_host_id ? targetValue('ssh', item.ssh_host_id)
    : item?.profile_id ? targetValue('profile', item.profile_id) : '';
  // An id that no longer resolves (deleted profile/host, or one saved by the
  // truncating parse this comment's helper fixes) leaves the select blank
  // rather than silently pointing the item at some other shell.
  if (current) targetSel.value = current;

  const wfSel = document.createElement('select');
  wfSel.className = 'li-workflow';
  const none = document.createElement('option');
  none.value = '';
  none.textContent = tr('set.noAutorun');
  wfSel.appendChild(none);
  for (const w of state.workflows) {
    const o = document.createElement('option');
    o.value = w.id;
    o.textContent = w.name;
    wfSel.appendChild(o);
  }
  wfSel.value = item?.workflow_id || '';

  const removeBtn = document.createElement('button');
  removeBtn.type = 'button';
  removeBtn.className = 'icon-btn li-remove';
  removeBtn.title = tr('set.removeItem.title');
  removeBtn.innerHTML = icon('x');
  removeBtn.addEventListener('click', () => li.remove());

  // Ad-hoc command, on its own full-width line below the two selects: it is
  // free text (often long) and pairs with — rather than replaces — the
  // workflow, so it gets the room a third select-sized column wouldn't give it.
  const cmdInput = document.createElement('input');
  cmdInput.type = 'text';
  cmdInput.className = 'li-command';
  cmdInput.placeholder = tr('set.command.ph');
  cmdInput.title = tr('set.command.title');
  cmdInput.value = item?.command || '';

  const head = document.createElement('div');
  head.className = 'li-head';
  head.appendChild(targetSel);
  head.appendChild(wfSel);
  head.appendChild(removeBtn);

  li.appendChild(head);
  li.appendChild(cmdInput);
  return li;
}

/** Builds the modal's repeatable item-list editor. Read back at submit time
 * via `readItemListRows()` — see `collectModalValues()`. Rows are unlimited;
 * an empty set starts with one blank row so the editor is never a bare
 * "add" button. */
function buildItemListEditor(initialItems) {
  const wrap = document.createElement('div');
  wrap.className = 'li-editor';
  wrap.dataset.itemlist = '1';
  wrap.dataset.name = 'items';

  const rows = document.createElement('ul');
  rows.className = 'li-rows';
  for (const it of initialItems.length ? initialItems : [null]) {
    rows.appendChild(buildItemRow(it));
  }
  wrap.appendChild(rows);

  const addBtn = document.createElement('button');
  addBtn.type = 'button';
  addBtn.className = 'ghost-btn li-add';
  addBtn.innerHTML = `${icon('plus')}<span>${tr('set.addItem')}</span>`;
  addBtn.addEventListener('click', () => rows.appendChild(buildItemRow(null)));
  wrap.appendChild(addBtn);

  return wrap;
}

/** Ordered picker for a host's ProxyJump chain: each row is a saved host
 * (excluding this host), and the row order is the hop order (first row = first
 * hop from the client). Reuses the item-list editor's styling. */
function buildJumpListEditor(selectedIds, selfId) {
  const wrap = document.createElement('div');
  wrap.className = 'li-editor';
  wrap.dataset.jumplist = '1';
  wrap.dataset.name = 'jump_hosts';
  const options = state.hosts.filter((h) => h.id && h.id !== selfId);
  const rows = document.createElement('ul');
  rows.className = 'li-rows';
  const addRow = (id) => rows.appendChild(buildJumpRow(options, id));
  (selectedIds || []).forEach(addRow);
  wrap.appendChild(rows);
  const addBtn = document.createElement('button');
  addBtn.type = 'button';
  addBtn.className = 'ghost-btn li-add';
  addBtn.innerHTML = `${icon('plus')}<span>${tr('host.jump.add')}</span>`;
  addBtn.disabled = options.length === 0;
  addBtn.addEventListener('click', () => addRow(''));
  wrap.appendChild(addBtn);
  return wrap;
}

function buildJumpRow(options, selectedId) {
  const row = document.createElement('li');
  row.className = 'li-row';
  const sel = document.createElement('select');
  sel.className = 'jl-target';
  const blank = document.createElement('option');
  blank.value = '';
  blank.textContent = tr('host.jump.pick');
  sel.appendChild(blank);
  for (const h of options) {
    const o = document.createElement('option');
    o.value = h.id;
    o.textContent = `${h.name || h.host} (${h.username}@${h.host}:${h.port})`;
    sel.appendChild(o);
  }
  sel.value = selectedId || '';
  const del = document.createElement('button');
  del.type = 'button';
  del.className = 'icon-btn li-del';
  del.innerHTML = icon('trash');
  del.title = tr('common.delete');
  del.addEventListener('click', () => row.remove());
  row.appendChild(sel);
  row.appendChild(del);
  return row;
}

/** Reads the jump-list editor back into an ordered array of host ids, in DOM
 * (hop) order, dropping empty rows. */
function readJumpListRows(container) {
  const out = [];
  for (const sel of container.querySelectorAll('.jl-target')) {
    if (sel.value) out.push(sel.value);
  }
  return out;
}

/** Reads the item-list editor's live DOM back into `LaunchSetItem[]` at
 * submit time — simpler and less error-prone than keeping a JSON blob in
 * sync with every add/remove. Rows with no selectable target (no profiles or
 * hosts exist at all) are skipped. */
function readItemListRows(container) {
  const out = [];
  for (const row of container.querySelectorAll('.li-row')) {
    const { kind, id } = parseTargetValue(row.querySelector('.li-target').value);
    if (!id) continue;
    out.push({
      profile_id: kind === 'profile' ? id : '',
      ssh_host_id: kind === 'ssh' ? id : '',
      workflow_id: row.querySelector('.li-workflow').value,
      command: row.querySelector('.li-command').value.trim(),
    });
  }
  return out;
}

function editLaunchSet(set) {
  const isNew = !set;
  openModal({
    title: isNew ? tr('set.editTitle.new') : tr('set.editTitle.edit'),
    okLabel: tr('common.save'),
    body: [
      fieldSection(tr('section.basic')),
      field(tr('wf.field.name'), 'name', set?.name || '', { required: true }),
      fieldSection(tr('set.section.itemsOrder')),
      fieldItemList(tr('set.field.items'), 'items', set?.items || []),
    ],
    onOk: async (values) => {
      if (!values.items.length) throw new Error(tr('set.needOneItem'));
      await invoke('save_launch_set', {
        set: { id: set?.id || '', name: values.name.trim(), items: values.items },
      });
      refreshLaunchSets();
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
    toast(tr('settings.loadFailed', { e }), true);
  }
  const f = $('#settings-form').elements;
  f.font_size.value = state.settings.font_size;
  f.font_family.value = state.settings.font_family;
  f.scrollback.value = state.settings.scrollback;
  f.theme.value = state.settings.theme === 'light' ? 'light' : 'dark';
  f.language.value = state.settings.language === 'en' ? 'en' : 'ja';
  renderTermThemeOptions();
  f.term_theme.value = termThemeSelectValue(state.settings.term_theme);
  f.term_theme.dataset.prev = f.term_theme.value;
  renderTermColorEditor(state.settings.term_colors || {});
  updateTermColorEditorVisibility();
  f.restore_session.checked = state.settings.restore_session !== false;
  f.gpu_rendering.checked = state.settings.gpu_rendering !== false;
  f.shell_integration.checked = state.settings.shell_integration !== false;
  f.right_click.value = ['copy_paste', 'menu'].includes(state.settings.right_click)
    ? state.settings.right_click : '';
  f.external_terminal.value = state.settings.external_terminal || '';
  f.notify_long_commands.checked = state.settings.notify_long_commands !== false;
  f.notify_threshold_secs.value = Number.isFinite(Number(state.settings.notify_threshold_secs))
    ? state.settings.notify_threshold_secs : 10;
  // The backend blanks the key before it crosses into the page, so there is
  // nothing to fill in — only whether one is stored, which the placeholder
  // and the delete row report.
  f.ai_api_key.value = '';
  const keySaved = !!state.settings.aiApiKeySaved;
  f.ai_api_key.placeholder = keySaved ? tr('settings.aiApiKey.savedPlaceholder') : 'sk-ant-...';
  $('#ai-key-clear-row').hidden = !keySaved;
  if (f.clear_ai_api_key) f.clear_ai_api_key.checked = false;
  f.ai_model.value = state.settings.ai_model || '';
  f.auto_update_check.checked = state.settings.auto_update_check !== false;
  f.session_log_dir.value = state.settings.session_log_dir || '';
  f.session_log_plain.checked = state.settings.session_log_plain !== false;
  f.session_log_local.checked = !!state.settings.session_log_local;
  // The resolved default as the placeholder, so a blank field says where
  // logs actually go rather than nothing.
  invoke('session_log_dir').then((d) => { f.session_log_dir.placeholder = d; }).catch(() => {});
  applyTheme();
  // Covers boot, settings import and "reload settings file" in one place.
  applyPaletteKeys();
  applyLanguage();
}

/* ---- terminal color scheme (settings) ---- */

/** The select's value for a stored id: unknown presets show as "follow". */
function termThemeSelectValue(id) {
  return id === 'custom' || TERM_PRESETS[id] ? id : '';
}

function renderTermThemeOptions() {
  const sel = $('#settings-form').elements.term_theme;
  const prev = sel.value;
  sel.innerHTML = '';
  const add = (value, text) => {
    const o = document.createElement('option');
    o.value = value;
    o.textContent = text;
    sel.appendChild(o);
  };
  add('', tr('settings.termTheme.follow'));
  for (const [id, p] of Object.entries(TERM_PRESETS)) add(id, p.name);
  add('custom', tr('settings.termTheme.custom'));
  sel.value = prev;
}

/** Builds the 19-swatch editor from `colors` (missing slots show the UI
 * theme's palette so every input has a meaningful starting value). */
function renderTermColorEditor(colors) {
  const box = $('#term-colors-editor');
  box.innerHTML = '';
  const effective = buildTermTheme(colors);
  const groups = [
    ['tc.group.base', ['background', 'foreground', 'cursor']],
    ['tc.group.normal', ['black', 'red', 'green', 'yellow', 'blue', 'magenta', 'cyan', 'white']],
    ['tc.group.bright', ['brightBlack', 'brightRed', 'brightGreen', 'brightYellow', 'brightBlue', 'brightMagenta', 'brightCyan', 'brightWhite']],
  ];
  for (const [groupKey, keys] of groups) {
    const g = document.createElement('div');
    g.className = 'tc-group';
    g.textContent = tr(groupKey);
    box.appendChild(g);
    for (const key of keys) {
      const label = document.createElement('label');
      label.className = 'tc-swatch';
      const input = document.createElement('input');
      input.type = 'color';
      input.dataset.tcKey = key;
      input.value = effective[key];
      input.addEventListener('input', previewTermTheme);
      label.appendChild(input);
      const span = document.createElement('span');
      span.textContent = tr(TERM_COLOR_KEYS.find(([k]) => k === key)[1]);
      label.appendChild(span);
      box.appendChild(label);
    }
  }
  // Seed the editor from a preset / reset to the UI theme palette.
  const actions = document.createElement('div');
  actions.className = 'tc-actions';
  const copyLabel = document.createElement('span');
  copyLabel.className = 'field-hint';
  copyLabel.textContent = tr('tc.copyFrom');
  const copySel = document.createElement('select');
  for (const [id, p] of Object.entries(TERM_PRESETS)) {
    const o = document.createElement('option');
    o.value = id;
    o.textContent = p.name;
    copySel.appendChild(o);
  }
  copySel.addEventListener('change', () => {
    renderTermColorEditor(TERM_PRESETS[copySel.value].colors);
    previewTermTheme();
  });
  const reset = document.createElement('button');
  reset.type = 'button';
  reset.className = 'ghost-btn';
  reset.textContent = tr('tc.reset');
  reset.addEventListener('click', () => {
    renderTermColorEditor({});
    previewTermTheme();
  });
  actions.append(copyLabel, copySel, reset);
  box.appendChild(actions);
}

function readTermColorEditor() {
  const out = {};
  for (const input of $('#term-colors-editor').querySelectorAll('input[data-tc-key]')) {
    out[input.dataset.tcKey] = input.value.toLowerCase();
  }
  return out;
}

function updateTermColorEditorVisibility() {
  const custom = $('#settings-form').elements.term_theme.value === 'custom';
  $('#term-colors-editor').classList.toggle('hidden', !custom);
}

/** Live preview: pushes the form's current selection to every open
 * terminal without touching saved settings (Save persists; navigating away
 * and re-applying the theme restores the saved palette). */
function previewTermTheme() {
  const f = $('#settings-form').elements;
  const id = f.term_theme.value;
  const theme = resolveTermTheme(id, id === 'custom' ? readTermColorEditor() : null);
  for (const t of state.threads) t.term.options.theme = theme;
}

$('#settings-form').elements.term_theme.addEventListener('change', () => {
  const f = $('#settings-form').elements;
  if (f.term_theme.value === 'custom') {
    // First switch to custom with nothing saved: start from whatever the
    // user was just looking at (the preset selected a moment ago, saved or
    // not), rather than a blank UI-theme palette.
    const saved = state.settings.term_colors || {};
    if (!Object.keys(saved).length) {
      const prevId = f.term_theme.dataset.prev ?? state.settings.term_theme;
      renderTermColorEditor(TERM_PRESETS[prevId] ? TERM_PRESETS[prevId].colors : {});
    }
  }
  f.term_theme.dataset.prev = f.term_theme.value;
  updateTermColorEditorVisibility();
  previewTermTheme();
});

/* A Store or package-manager build never self-updates, so the toggle would be
 * a lie. Disable it and swap the hint for who does own updates — the setting's
 * stored value is left untouched, so it comes back if the same profile is used
 * with a downloaded build. */
function applyAutoUpdateAvailability() {
  const box = $('#settings-form').elements.auto_update_check;
  const hint = $('#auto-update-hint');
  if (!box || !hint) return;
  const managed = state.dist && state.dist.inAppUpdates === false;
  box.disabled = !!managed;
  box.closest('.checkbox-row')?.classList.toggle('disabled', !!managed);
  if (managed) {
    hint.textContent = tr('settings.autoUpdate.managed', { owner: updateOwnerLabel() });
    hint.removeAttribute('data-i18n');
  } else {
    hint.setAttribute('data-i18n', 'settings.autoUpdate.hint');
    hint.textContent = tr('settings.autoUpdate.hint');
  }
}

$('#session-log-open').addEventListener('click', async () => {
  try {
    await invoke('reveal_path', { path: await invoke('session_log_dir') });
  } catch (e) {
    toast(tr('settings.sessionLog.openFailed', { e }), true);
  }
});

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
    language: f.elements.language.value === 'en' ? 'en' : 'ja',
    term_theme: f.elements.term_theme.value,
    term_colors: f.elements.term_theme.value === 'custom'
      ? readTermColorEditor() : (state.settings.term_colors || {}),
    restore_session: f.elements.restore_session.checked,
    gpu_rendering: f.elements.gpu_rendering.checked,
    shell_integration: f.elements.shell_integration.checked,
    right_click: f.elements.right_click.value,
    external_terminal: f.elements.external_terminal.value.trim(),
    notify_long_commands: f.elements.notify_long_commands.checked,
    notify_threshold_secs: Math.max(0, parseInt(f.elements.notify_threshold_secs.value, 10) || 0),
    // Blank = leave the stored key alone. The field never held it, so
    // treating empty as "delete" would drop the key on every unrelated save.
    ai_api_key: f.elements.ai_api_key.value.trim(),
    ai_model: f.elements.ai_model.value.trim(),
    auto_update_check: f.elements.auto_update_check.checked,
    session_log_dir: f.elements.session_log_dir.value.trim(),
    session_log_plain: f.elements.session_log_plain.checked,
    session_log_local: f.elements.session_log_local.checked,
  };
  const clearKey = !!(f.elements.clear_ai_api_key && f.elements.clear_ai_api_key.checked);
  await invoke('save_settings', {
    settings: state.settings,
    clearAiApiKey: clearKey,
  });
  state.settings.aiApiKeySaved =
    !clearKey && (state.settings.aiApiKeySaved || !!state.settings.ai_api_key);
  state.settings.ai_api_key = '';
  applyTheme();
  applyLanguage();
  applyGpuRendering();
  for (const t of state.threads) {
    t.term.options.fontSize = state.settings.font_size;
    t.term.options.fontFamily = fontFamilyStack();
    t.term.options.scrollback = state.settings.scrollback;
    t.fit.fit();
  }
  toast(tr('settings.saved'));
});

/* ---------------- auto update ---------------- */

/* Who updates this copy depends on where it came from — a downloaded installer
 * updates itself, a Microsoft Store or package-manager install must not (see
 * senju-core's `distribution` module). `dist_info` is read once at boot; the
 * backend also refuses to register the updater plugin at all for the managed
 * channels, so this is the friendly half of a two-layer rule, not the only one.
 *
 * Externally managed builds still get a working "check for updates": it hands
 * off to the Store's own updates pane instead of doing nothing. */
async function loadDistInfo() {
  try {
    state.dist = await invoke('dist_info');
  } catch (_e) {
    // Older backend or a failed call — assume nothing is managed for us and
    // let the updater path report its own unavailability.
    state.dist = { channel: 'unknown', installer: null, inAppUpdates: true, hasExternalManager: false };
  }
}

/** Human label for who applies updates, used in the settings hint and toasts. */
function updateOwnerLabel() {
  const key = {
    msstore: 'update.owner.msstore',
    macappstore: 'update.owner.macappstore',
    'system-package': 'update.owner.systemPackage',
    development: 'update.owner.development',
  }[state.dist?.channel];
  return key ? tr(key) : '';
}

/* Signed updates from GitHub Releases via tauri-plugin-updater. The check is
 * a no-op wherever the updater isn't usable (dev builds, builds made before
 * the owner configured the signing key, offline) — `manual` only controls
 * whether those cases surface as toasts. Installing always asks first, and
 * signature verification is enforced by the plugin. */
async function checkForUpdates(manual = false) {
  // Managed elsewhere: never self-update, but do offer the handoff.
  if (state.dist && state.dist.inAppUpdates === false) {
    if (!manual) return;
    const owner = updateOwnerLabel();
    if (!state.dist.hasExternalManager) {
      toast(tr('update.managedElsewhere', { owner }));
      return;
    }
    if (await confirmModal(tr('update.openManager', { owner }), tr('update.openManager.ok'))) {
      try {
        await invoke('open_update_manager');
      } catch (e) {
        toast(String(e), true);
      }
    }
    return;
  }
  const updater = window.__TAURI__?.updater;
  if (!updater) {
    if (manual) toast(tr('update.unavailable'), true);
    return;
  }
  let update;
  try {
    update = await updater.check();
  } catch (e) {
    if (manual) toast(tr('update.checkFailed', { e }), true);
    return;
  }
  if (!update) {
    if (manual) toast(tr('update.none'));
    return;
  }
  const install = await confirmModal(
    tr('update.available', { version: update.version }),
    tr('update.install'),
  );
  if (!install) return;
  toast(tr('update.downloading'));
  try {
    await update.downloadAndInstall();
  } catch (e) {
    toast(tr('update.failed', { e }), true);
    return;
  }
  if (await confirmModal(tr('update.installed'), tr('update.relaunch'))) {
    try {
      await window.__TAURI__.process.relaunch();
    } catch (e) {
      toast(String(e), true);
    }
  }
}

/* ---------------- AI command assist ---------------- */

/* Privacy-first by design: the backend sends ONLY the typed question plus
 * OS/shell names to the Anthropic API using the user's own key — never
 * terminal contents, history, or files (see senju-core's ai module). The
 * suggested command is never executed automatically: "insert" types it into
 * the terminal for review, and Enter is up to the user. */

let aiBusy = false;

function openAiAssist() {
  if (!state.settings.aiApiKeySaved) {
    toast(tr('ai.noKey'), true);
    setView('settings');
    return;
  }
  openModal({
    title: tr('ai.modal.title'),
    okLabel: tr('ai.ask'),
    body: [
      { tag: 'note', label: tr('ai.privacyNote') },
      { tag: 'textarea', name: 'query', label: tr('ai.query.label'), value: '', required: true },
    ],
    onOk: async (values) => {
      if (aiBusy) throw tr('ai.asking');
      aiBusy = true;
      toast(tr('ai.asking'));
      try {
        const s = await invoke('ai_suggest_command', { query: values.query.trim() });
        // submitModal hides the modal and clears its body after onOk resolves;
        // open the result modal on the next tick so it isn't wiped with it.
        setTimeout(() => openAiResult(s), 0);
      } catch (e) {
        const msg = String(e);
        if (msg === 'no-api-key') throw tr('ai.noKey');
        if (msg === 'refusal') throw tr('ai.refused');
        throw tr('ai.failed', { e: msg });
      } finally {
        aiBusy = false;
      }
    },
  });
}

function openAiResult(s) {
  const body = [
    { tag: 'textarea', name: 'command', label: tr('ai.result.command'), value: s.command, required: true },
    { tag: 'note', label: s.explanation },
  ];
  if ((s.caution || '').trim()) body.push({ tag: 'note', label: `⚠ ${s.caution}` });
  body.push({ tag: 'note', label: tr('ai.result.insertNote') });
  openModal({
    title: tr('ai.result.title'),
    okLabel: tr('ai.result.insert'),
    body,
    onOk: (values) => {
      const cmd = values.command.trim();
      if (cmd) sendToActive(cmd, false); // insert only — never auto-run
    },
    extraActions: [{
      label: tr('ai.result.copy'),
      onClick: async ({ values }) => {
        try {
          await navigator.clipboard.writeText(values.command.trim());
          toast(tr('ai.result.copied'));
        } catch (e) {
          toast(String(e), true);
        }
      },
    }],
  });
}

/* ---------------- built-in dev server ---------------- */

/* The backend owns the sockets; this mirrors what it reports. Nothing here
 * decides whether a server is running — `list_dev_servers` does, and every
 * mutation is followed by a re-read, so a start that failed can never leave a
 * card claiming to serve something. */

/** Requests kept for the log. Past this the oldest are dropped: the panel is a
 * live view, not a record, and an unbounded array behind a page load loop grows
 * without end. */
const SERVER_LOG_MAX = 300;

async function refreshServers() {
  try {
    const [running, presets] = await Promise.all([
      invoke('list_dev_servers'),
      invoke('list_dev_server_presets'),
    ]);
    state.servers.running = running;
    state.servers.presets = presets;
  } catch (e) {
    toast(String(e), true);
  }
  renderServers();
}

/** Starts a server and, if asked, opens it. `opts` comes either from the start
 * form or from a saved preset; both produce the same call. */
async function startServer(opts) {
  const root = (opts.root || '').trim();
  if (!root) {
    toast(tr('srv.noRoot'), true);
    return null;
  }
  let info;
  try {
    info = await invoke('start_dev_server', {
      root,
      port: Number(opts.port) || 0,
      spa: !!opts.spa,
      hotReload: opts.hotReload !== false,
    });
  } catch (e) {
    toast(String(e), true);
    return null;
  }
  toast(tr('srv.started', { url: info.url }));
  if (opts.openBrowser) openServer(info);
  await refreshServers();
  return info;
}

async function stopServer(id) {
  try {
    await invoke('stop_dev_server', { id });
    toast(tr('srv.stopped'));
  } catch (e) {
    toast(String(e), true);
  }
  await refreshServers();
}

function openServer(info) {
  invoke('open_external', { url: info.url }).catch((e) => toast(String(e), true));
}

/** Seconds since `startedAt` as a compact h/m/s string. Not localized beyond
 * the unit letters: it is read at a glance next to a URL, and "2h 05m" says the
 * same thing in both languages the app ships. */
function uptime(startedAt) {
  const secs = Math.max(0, Math.floor(Date.now() / 1000) - startedAt);
  if (secs < 60) return `${secs}s`;
  const m = Math.floor(secs / 60);
  if (m < 60) return `${m}m`;
  return `${Math.floor(m / 60)}h ${String(m % 60).padStart(2, '0')}m`;
}

/** Keeps the uptime badges honest while the panel is open. Only the running
 * list is redrawn, and only while it is on screen — a timer that re-rendered
 * the whole panel (or ran with the panel closed) would be pure churn. */
let serverUptimeTimer = 0;

function syncUptimeTicker() {
  const wanted = currentView === 'servers' && state.servers.running.length > 0;
  if (wanted && !serverUptimeTimer) {
    serverUptimeTimer = setInterval(() => {
      if (currentView === 'servers') renderRunningServers();
      else syncUptimeTicker();
    }, 30000);
  } else if (!wanted && serverUptimeTimer) {
    clearInterval(serverUptimeTimer);
    serverUptimeTimer = 0;
  }
}

function renderServers() {
  renderRunningServers();
  renderServerPresets();
  renderServerLog();
  syncUptimeTicker();
}

function renderRunningServers() {
  const list = $('#srv-running');
  list.innerHTML = '';
  if (!state.servers.running.length) {
    list.appendChild(emptyState('server', tr('srv.empty.running'), tr('srv.empty.running.hint')));
    return;
  }
  for (const info of state.servers.running) list.appendChild(runningServerCard(info));
}

function runningServerCard(info) {
  const li = document.createElement('li');
  li.className = 'card srv-card';
  li.innerHTML = `
    <h4><button type="button" class="srv-url"></button></h4>
    <div class="srv-root"></div>
    <div class="wf-badges"></div>
    <div class="row">
      <button class="accent-btn srv-open">${icon('login')}${tr('srv.open')}</button>
      <button class="ghost-btn srv-copy">${icon('copy')}${tr('srv.copy')}</button>
      <button class="danger-btn srv-stop">${icon('x')}${tr('srv.stop')}</button>
    </div>`;
  // Deliberately a button, not an <a>: a real link in the webview can navigate
  // the app itself to the served site (a ctrl-click, a middle-click) and
  // replace the terminal with the page being previewed. This can only ever
  // hand the URL to the OS browser.
  const link = li.querySelector('.srv-url');
  link.textContent = info.url;
  link.addEventListener('click', () => openServer(info));
  li.querySelector('.srv-root').textContent = info.root;

  const badges = li.querySelector('.wf-badges');
  const badge = (text, cls) => {
    const el = document.createElement('span');
    el.className = cls || 'wf-badge';
    el.textContent = text;
    badges.appendChild(el);
  };
  if (info.hotReload) badge(tr('srv.badge.hot'));
  if (info.spa) badge(tr('srv.badge.spa'), 'tag');
  badge(tr('srv.uptime', { t: uptime(info.startedAt) }), 'wf-badge wf-group-badge');

  li.querySelector('.srv-open').addEventListener('click', () => openServer(info));
  li.querySelector('.srv-copy').addEventListener('click', async () => {
    try {
      await navigator.clipboard.writeText(info.url);
      toast(tr('srv.copied'));
    } catch (e) {
      toast(String(e), true);
    }
  });
  li.querySelector('.srv-stop').addEventListener('click', () => stopServer(info.id));
  return li;
}

function renderServerPresets() {
  const list = $('#srv-presets');
  list.innerHTML = '';
  if (!state.servers.presets.length) {
    list.appendChild(emptyState('star', tr('srv.empty.presets'), tr('srv.empty.presets.hint')));
    return;
  }
  for (const preset of state.servers.presets) list.appendChild(presetCard(preset));
}

function presetCard(preset) {
  const li = document.createElement('li');
  li.className = 'card srv-card';
  li.innerHTML = `
    <h4></h4>
    <div class="srv-root"></div>
    <div class="wf-badges"></div>
    <div class="row">
      <button class="accent-btn run">${icon('play')}${tr('srv.start')}</button>
      <button class="ghost-btn edit">${icon('edit')}${tr('common.edit')}</button>
      <button class="danger-btn del">${icon('trash')}${tr('common.delete')}</button>
    </div>`;
  li.querySelector('h4').textContent = preset.name;
  li.querySelector('.srv-root').textContent = preset.root;
  const badges = li.querySelector('.wf-badges');
  const badge = (text, cls) => {
    const el = document.createElement('span');
    el.className = cls || 'wf-badge';
    el.textContent = text;
    badges.appendChild(el);
  };
  badge(preset.port ? `:${preset.port}` : tr('srv.autoPort'), 'wf-badge wf-group-badge');
  if (preset.hot_reload !== false) badge(tr('srv.badge.hot'));
  if (preset.spa) badge(tr('srv.badge.spa'), 'tag');

  li.querySelector('.run').addEventListener('click', () => startServer({
    root: preset.root,
    port: preset.port,
    spa: preset.spa,
    hotReload: preset.hot_reload !== false,
    openBrowser: preset.open_browser,
  }));
  li.querySelector('.edit').addEventListener('click', () => editServerPreset(preset));
  li.querySelector('.del').addEventListener('click', () => {
    openModal({
      title: tr('srv.preset.deleteConfirm', { name: preset.name }),
      okLabel: tr('common.delete'),
      body: [],
      onOk: async () => {
        await invoke('delete_dev_server_preset', { id: preset.id });
        await refreshServers();
      },
    });
  });
  return li;
}

/** Editor for a saved setup. `seed` may be an existing preset (edit) or the
 * current form contents (the "save this setup" button), which is why it takes
 * a plain object rather than an id. */
function editServerPreset(seed) {
  const isNew = !seed || !seed.id;
  openModal({
    title: isNew ? tr('srv.preset.editTitle.new') : tr('srv.preset.editTitle.edit'),
    okLabel: tr('common.save'),
    body: [
      field(tr('srv.preset.name'), 'name', seed?.name || '', { required: true }),
      field(tr('srv.field.root'), 'root', seed?.root || '', { required: true }),
      field(tr('srv.field.port'), 'port', String(seed?.port ?? 0)),
      fieldNote(tr('srv.portHint')),
      fieldCheckbox(tr('srv.field.hotReload'), 'hot_reload', seed?.hot_reload !== false),
      fieldCheckbox(tr('srv.field.spa'), 'spa', !!seed?.spa),
      fieldCheckbox(tr('srv.field.openBrowser'), 'open_browser', !!seed?.open_browser),
    ],
    onOk: async (values) => {
      await invoke('save_dev_server_preset', {
        preset: {
          id: seed?.id || '',
          name: values.name.trim(),
          root: values.root.trim(),
          // A blank or non-numeric port means "any free port", the same as 0 —
          // the alternative is NaN reaching the backend as a u16.
          port: Math.max(0, Math.min(65535, parseInt(values.port, 10) || 0)),
          spa: values.spa,
          hot_reload: values.hot_reload,
          open_browser: values.open_browser,
        },
      });
      await refreshServers();
    },
  });
}

function renderServerLog() {
  const list = $('#srv-log');
  list.innerHTML = '';
  if (!state.servers.log.length) {
    const li = document.createElement('li');
    li.className = 'srv-log-empty';
    li.textContent = tr('srv.empty.log');
    list.appendChild(li);
    return;
  }
  // Newest first: the request you just made is the one you are looking for.
  for (const entry of state.servers.log.slice().reverse()) {
    const li = document.createElement('li');
    li.className = 'srv-log-row';
    // 2xx is unremarkable; 4xx/5xx is the reason anyone opens this list.
    li.classList.add(entry.status >= 400 ? 'bad' : 'ok');
    li.innerHTML = '<span class="lg-status"></span><span class="lg-method"></span><span class="lg-path"></span><span class="lg-port"></span>';
    li.querySelector('.lg-status').textContent = entry.status;
    li.querySelector('.lg-method').textContent = entry.method;
    li.querySelector('.lg-path').textContent = entry.path;
    const port = state.servers.running.find((s) => s.id === entry.serverId)?.port;
    li.querySelector('.lg-port').textContent = port ? `:${port}` : '';
    list.appendChild(li);
  }
}

listen('devserver:access', (ev) => {
  state.servers.log.push(ev.payload);
  if (state.servers.log.length > SERVER_LOG_MAX) {
    state.servers.log.splice(0, state.servers.log.length - SERVER_LOG_MAX);
  }
  // Requests arrive in bursts (one page = many assets); only redraw the list
  // that is actually on screen.
  if (currentView === 'servers') renderServerLog();
});

listen('devserver:reloaded', (ev) => {
  const [, path] = ev.payload;
  if (currentView === 'servers') toast(tr('srv.reloaded', { path }));
});

listen('devserver:watch-failed', (ev) => {
  const [, message] = ev.payload;
  toast(tr('srv.watchFailed', { e: message }), true);
});

// Not an error: most of the tree still reloads. Said plainly so the few files
// that quietly will not do not look like the server being broken.
listen('devserver:watch-limited', (ev) => {
  const [, watched] = ev.payload;
  toast(tr('srv.watchLimited', { n: watched }));
});

/* ---------------- task runners (package.json / Cargo.toml / ...) ---------------- */

/** Turns an OSC 7 payload into a plain filesystem path.
 *
 * The payload is normally `file://<host>/<path>`, percent-encoded. The host is
 * whatever the shell thinks its hostname is; it is dropped rather than checked,
 * because OSC 7 only reaches us from local shells today (SSH sessions get no
 * shell integration) and a mismatched hostname is not evidence of anything.
 * Returns '' for anything that doesn't look like an absolute path — a garbled
 * report should leave the panel saying "unknown", not scan the wrong tree. */
function parseOsc7(payload) {
  let raw = payload || '';
  if (raw.startsWith('file://')) {
    const slash = raw.indexOf('/', 'file://'.length);
    raw = slash >= 0 ? raw.slice(slash) : '';
  }
  if (!raw.startsWith('/')) return '';
  let path;
  try {
    path = decodeURIComponent(raw);
  } catch {
    // A stray '%' the shell failed to escape. The undecoded form is still
    // closer to the truth than nothing, and the backend simply won't find it.
    path = raw;
  }
  // `file:///C:/Users/x`: the leading slash belongs to the URL, not the path.
  if (/^\/[A-Za-z]:[\\/]/.test(path)) path = path.slice(1);
  return path;
}

// OSC 7 arrives at every prompt, so a `cd` in a loop would otherwise fire a
// scan per iteration. Coalesce into one scan shortly after things settle.
let taskRefreshTimer = 0;
// Scans are async and the directory can change while one is in flight; only
// the newest scan is allowed to write its result.
let taskScanToken = 0;

function scheduleTaskRefresh() {
  clearTimeout(taskRefreshTimer);
  taskRefreshTimer = setTimeout(() => refreshTasks(), 150);
}

/** Shows or hides the task list beside the terminal.
 *
 * It is a pane rather than a tool view because pressing a task means "run this
 * and watch it": the list and the shell it types into have to be visible at
 * the same time, or every run costs a round trip through the tabs. Opening it
 * narrows the panes, so the terminals are refitted afterwards. */
function setTaskPane(open) {
  // Reachable from the command palette while a tool panel is up, where the
  // panes — and so the pane this sits beside — are hidden.
  if (open && currentView !== 'shell') setView('shell');
  state.taskPane = !!open;
  $('#taskpane').classList.toggle('hidden', !state.taskPane);
  $('#taskpane-grip').classList.toggle('hidden', !state.taskPane);
  $('#task-toggle').classList.toggle('active', state.taskPane);
  applyTaskPaneWidth();
  // The directory is live, so the list is re-scanned every time the pane
  // opens rather than trusting whatever the last OSC 7 report left behind.
  if (state.taskPane) refreshTasks(true);
  requestAnimationFrame(() => {
    for (const pane of state.panes) threadById(pane.threadId)?.fit.fit();
    if (!state.taskPane) focusedThread()?.term.focus();
  });
}

// Below this the cards clip too badly to read; above it the pane is just
// taking room the terminal could use. TERM_MIN is what the terminal keeps
// whatever the pane asks for — a squeezed terminal is the failure mode that
// matters, since the whole point of the pane is to watch a command run.
const TASKPANE_MIN = 200;
const TASKPANE_MAX = 640;
const TERM_MIN = 280;

/** What the layout can actually give the pane right now. Measured against the
 * space beside the terminal rather than the window, because the thread bar
 * takes its own column and may or may not be showing. */
function maxTaskPaneWidth() {
  const width = (sel) => $(sel).getBoundingClientRect().width;
  const threadbar = $('#threadbar').classList.contains('hidden') ? 0 : width('#threadbar');
  const avail = width('#body') - threadbar - width('#taskpane-grip');
  return Math.max(TASKPANE_MIN, Math.min(TASKPANE_MAX, avail - TERM_MIN));
}

/** Records the width the user dragged to. Kept separate from what is actually
 * applied: a narrow window clamps the pane, but forgetting the asked-for width
 * would mean a single resize silently throws the user's sizing away, never to
 * return when the room does. */
function setTaskPaneWidth(px) {
  state.taskPaneWidth = Math.min(TASKPANE_MAX, Math.max(TASKPANE_MIN, px));
  applyTaskPaneWidth();
}

/** Puts the wanted width on the pane, clamped to what the layout allows now.
 * Re-run whenever that changes (window resize, thread bar toggle), so the pane
 * gives way when room is short and grows back when it returns. */
function applyTaskPaneWidth() {
  if (!state.taskPane) return;
  $('#taskpane').style.width = `${Math.min(maxTaskPaneWidth(), state.taskPaneWidth)}px`;
}

/** Re-scans the focused thread's working directory. Skips the backend call
 * when neither the thread nor its directory has changed, unless `force` (the
 * refresh button — the files themselves may have been edited). */
async function refreshTasks(force = false) {
  const thread = focusedThread();
  const threadId = thread ? thread.id : '';
  // SSH threads never report a directory (no remote shell integration), so
  // they are treated as "no cwd" and get their own explanatory empty state.
  const cwd = thread && thread.kind !== 'ssh' ? (thread.cwd || '') : '';
  if (!force && threadId === state.tasks.thread && cwd === state.tasks.cwd) {
    renderTasks();
    return;
  }
  const token = ++taskScanToken;
  state.tasks.thread = threadId;
  state.tasks.cwd = cwd;
  state.tasks.groups = [];
  renderTasks();
  if (!cwd) return;
  let groups = [];
  try {
    groups = await invoke('scan_tasks', { dir: cwd });
  } catch {
    // A directory that was removed between the report and the scan is ordinary
    // (the shell may already have moved on). An empty list says the same thing
    // the user needs to know, without a toast for something they didn't do.
    groups = [];
  }
  if (token !== taskScanToken) return; // a newer scan won
  state.tasks.groups = groups;
  renderTasks();
}

function renderTasks() {
  const body = $('#task-body');
  body.innerHTML = '';
  $('#task-cwd').textContent =
    state.tasks.cwd ? tr('tasks.cwd', { dir: state.tasks.cwd }) : '';
  const list = document.createElement('ul');
  list.className = 'card-list';
  body.appendChild(list);
  const thread = focusedThread();
  if (!thread) {
    list.appendChild(emptyState('terminal', tr('tasks.empty.noThread.title'),
      tr('tasks.empty.noThread')));
  } else if (thread.kind === 'ssh') {
    list.appendChild(emptyState('server', tr('tasks.empty.ssh.title'),
      tr('tasks.empty.ssh')));
  } else if (!state.tasks.cwd) {
    list.appendChild(emptyState('info', tr('tasks.empty.noCwd.title'),
      tr('tasks.empty.noCwd')));
  } else if (!state.tasks.groups.length) {
    list.appendChild(emptyState('file', tr('tasks.empty.none.title'),
      tr('tasks.empty.none')));
  } else {
    for (const g of state.tasks.groups) list.appendChild(taskGroupCard(g));
  }
}

/** One card per manifest, so it stays obvious which file a command came from:
 * a repo with both a package.json and a Makefile can declare the same name
 * twice, meaning two different things. */
function taskGroupCard(g) {
  const li = document.createElement('li');
  li.className = 'card task-card';
  li.innerHTML = `<h4><span class="task-file"></span><span class="task-runner"></span></h4>
    <ul class="task-list"></ul>`;
  li.querySelector('.task-file').textContent = g.file;
  li.querySelector('.task-runner').textContent = g.runner;
  const ul = li.querySelector('.task-list');
  for (const t of g.tasks) ul.appendChild(taskRow(t));
  return li;
}

function taskRow(t) {
  const row = document.createElement('li');
  row.className = 'task-row';
  row.innerHTML = `
    <div class="task-text"><span class="task-name"></span><span class="task-detail"></span></div>
    <code class="task-cmd"></code>
    <button class="accent-btn task-run">${icon('play')}<span></span></button>`;
  row.querySelector('.task-name').textContent = t.name;
  const detail = row.querySelector('.task-detail');
  if (t.detail) detail.textContent = t.detail; else detail.remove();
  row.querySelector('.task-cmd').textContent = t.command;
  const btn = row.querySelector('.task-run');
  btn.title = tr('tasks.hint');
  btn.querySelector('span').textContent = tr('tasks.run');
  // Shift-click inserts without submitting, the same as the command palette
  // and the workflow buttons — a task you want a flag added to shouldn't have
  // to be retyped.
  btn.addEventListener('click', (ev) => runTask(t, !ev.shiftKey));
  return row;
}

/** Sends a task to the focused pane. Watching it run is the reason the button
 * was pressed, so a tool panel is dismissed first — that only happens when the
 * task came from the command palette, since the pane's own buttons are next to
 * the terminal and only exist in the shell view. Nothing is executed here or
 * in the backend: the command is typed into the shell exactly as if the user
 * had, so their aliases, environment and confirmation prompts all still
 * apply. */
function runTask(t, run) {
  sendToActive(t.command, run);
  if (currentView !== 'shell') setView('shell');
}

/* ---------------- command palette ---------------- */

const palette = {
  open: false,
  items: [],
  selected: 0,
};

/** Re-reads settings.json (hand edits, e.g. `palette_keys`) into the running
 * app; `loadSettings` refills the form and re-applies theme, language and keys. */
async function reloadSettingsFile() {
  await loadSettings();
  toast(tr('palette.settingsReloaded'));
}

/** Reveals settings.json in the file manager. The file only exists after the
 * first save, so write the current settings out first when it is missing. */
async function showSettingsFile() {
  try {
    const path = await invoke('settings_file_path');
    try {
      await invoke('reveal_path', { path });
    } catch {
      await invoke('save_settings', { settings: state.settings });
      await invoke('reveal_path', { path });
    }
  } catch (e) {
    toast(tr('palette.settingsFileFailed', { e }), true);
  }
}

function paletteEntries() {
  const entries = [
    { kind: 'action', label: tr('palette.newLocalThread'), detail: 'Ctrl+Shift+T', run: () => newLocalThread() },
    {
      kind: 'action',
      label: state.panes.length >= 2 && state.splitDir === 'v'
        ? tr('palette.unsplitPane') : tr('palette.splitPane'),
      detail: 'Ctrl+Shift+D',
      run: () => toggleSplitDir('v'),
    },
    {
      kind: 'action',
      label: state.panes.length >= 2 && state.splitDir === 'h'
        ? tr('palette.unsplitPane') : tr('palette.splitPaneH'),
      detail: 'Ctrl+Shift+E',
      run: () => toggleSplitDir('h'),
    },
    {
      kind: 'action',
      label: tr(state.taskPane ? 'palette.tasksClose' : 'palette.tasksOpen'),
      detail: '',
      run: () => setTaskPane(!state.taskPane),
    },
    { kind: 'action', label: tr('palette.aiAssist'), detail: 'AI', run: openAiAssist },
    { kind: 'action', label: tr('palette.checkUpdate'), detail: '', run: () => checkForUpdates(true) },
    { kind: 'action', label: tr('palette.registerWorkflow'), detail: '', run: () => editWorkflow(null) },
    { kind: 'action', label: tr('palette.registerHost'), detail: '', run: () => editHost(null) },
    { kind: 'action', label: tr('palette.registerSet'), detail: '', run: () => editLaunchSet(null) },
    { kind: 'action', label: tr('palette.shortcutsList'), detail: 'Ctrl+Shift+/', run: openShortcuts },
    { kind: 'action', label: tr('palette.reloadSettings'), detail: 'settings.json', run: reloadSettingsFile },
    { kind: 'action', label: tr('palette.showSettingsFile'), detail: 'settings.json', run: showSettingsFile },
  ];
  // Transfers ride the focused thread's SSH connection, so they are only
  // offered when there is one to ride.
  if (focusedThread()?.kind === 'ssh') {
    entries.push(
      { kind: 'action', label: tr('sftp.browser.title'), detail: 'SFTP', run: () => openRemoteBrowser() },
      { kind: 'action', label: tr('sftp.upload.title'), detail: 'SFTP', run: sftpUpload },
      { kind: 'action', label: tr('sftp.download.title'), detail: 'SFTP', run: sftpDownload },
    );
  }
  if (state.panes.length >= 2) {
    entries.push({
      kind: 'action',
      label: tr(state.broadcast ? 'palette.broadcastOff' : 'palette.broadcastOn'),
      detail: 'Ctrl+Shift+B',
      run: toggleBroadcast,
    });
  }
  for (const p of state.profiles) {
    entries.push({
      kind: 'action',
      label: tr('palette.newThreadOf', { name: p.name }),
      detail: p.command || tr('profile.systemDefault'),
      run: () => newLocalThread({ profileId: p.id }),
    });
  }
  for (const t of state.threads) {
    entries.push({
      kind: 'thread',
      label: tr('palette.showThread', { title: t.title }),
      detail: t.kind === 'ssh' ? tr('thread.kind.ssh') : tr('thread.kind.local'),
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
      label: groupParts(w).length ? `${groupParts(w).join('/')} / ${w.name}` : w.name,
      detail: w.command,
      run: () => runWorkflow(w, true),
      insert: () => runWorkflow(w, false),
    });
  }
  for (const g of state.tasks.groups) {
    for (const t of g.tasks) {
      entries.push({
        kind: 'task',
        label: tr('palette.task', { name: t.name }),
        detail: `${g.file} · ${t.command}`,
        run: () => runTask(t, true),
        insert: () => runTask(t, false),
      });
    }
  }
  for (const preset of state.servers.presets) {
    entries.push({
      kind: 'server',
      label: tr('palette.startServer', { name: preset.name }),
      detail: `${preset.root}${preset.port ? ` :${preset.port}` : ''}`,
      run: () => startServer({
        root: preset.root,
        port: preset.port,
        spa: preset.spa,
        hotReload: preset.hot_reload !== false,
        openBrowser: preset.open_browser,
      }),
    });
  }
  for (const info of state.servers.running) {
    entries.push({
      kind: 'server',
      label: tr('palette.stopServer', { url: info.url }),
      detail: info.root,
      run: () => stopServer(info.id),
    });
  }
  for (const set of state.launchSets) {
    entries.push({
      kind: 'set',
      label: tr('palette.launchSet', { name: set.name }),
      detail: tr('set.itemsSummary', { count: set.items.length, summary: launchSetSummary(set) }),
      run: () => runLaunchSet(set),
    });
  }
  const lang = state.settings.language === 'en' ? 'en' : 'ja';
  for (const h of state.history) {
    const date = h.at
      ? new Date(h.at * 1000).toLocaleString(lang, { month: 'numeric', day: 'numeric', hour: '2-digit', minute: '2-digit' })
      : '';
    const where = `${h.kind === 'ssh' ? 'SSH' : tr('kind.local')}${h.host ? ` ${h.host}` : ''}`;
    const parts = [where];
    if (h.exit !== null && h.exit !== undefined) parts.push(h.exit === 0 ? '✓' : `✗ ${h.exit}`);
    if (h.duration_ms >= 1000) parts.push(formatElapsed(h.duration_ms));
    if (h.runs > 1) parts.push(`×${h.runs}`);
    if (date) parts.push(date);
    entries.push({
      kind: 'hist',
      label: h.command,
      detail: parts.join(' · '),
      hist: h,
      run: () => sendToActive(h.command, true),
      insert: () => sendToActive(h.command, false),
      save: () => saveAsWorkflow(h.command),
    });
  }
  return entries;
}

/** Splits a palette query into free text plus history filters:
 *   `@host`    only history entries whose host contains `host`
 *   `!failed`  only entries that exited non-zero (`!ok` = zero)
 *   `#ssh` / `#local`   only that kind
 * Filters apply to history rows; other rows are hidden while any filter is
 * active (the user is clearly looking for a past command). */
function parsePaletteQuery(raw) {
  const f = { text: [], host: null, exit: null, kind: null };
  for (const tok of raw.split(/\s+/).filter(Boolean)) {
    if (tok.startsWith('@') && tok.length > 1) f.host = tok.slice(1).toLowerCase();
    else if (/^!(failed|fail|err|error)$/i.test(tok)) f.exit = 'fail';
    else if (/^!(ok|success)$/i.test(tok)) f.exit = 'ok';
    else if (/^#(ssh|local)$/i.test(tok)) f.kind = tok.slice(1).toLowerCase();
    else f.text.push(tok);
  }
  f.active = f.host !== null || f.exit !== null || f.kind !== null;
  f.query = f.text.join(' ');
  return f;
}

function histMatchesFilters(h, f) {
  if (!f.active) return true;
  if (f.kind && h.kind !== f.kind) return false;
  if (f.host && !(h.host || '').toLowerCase().includes(f.host)) return false;
  if (f.exit === 'fail' && !(typeof h.exit === 'number' && h.exit !== 0)) return false;
  if (f.exit === 'ok' && h.exit !== 0) return false;
  return true;
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

async function refreshHistory() {
  try {
    state.history = (await invoke('list_history')) || [];
  } catch (_e) {
    state.history = [];
  }
}

function openPalette() {
  palette.open = true;
  // Refresh history in the background; re-rank once it lands.
  refreshHistory().then(() => { if (palette.open) updatePalette(); });
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
  const f = parsePaletteQuery($('#palette-input').value.trim());
  const q = f.query;
  palette.items = paletteEntries()
    .filter((e) => (e.kind === 'hist' ? histMatchesFilters(e.hist, f) : !f.active))
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
      { wf: 'CMD', ssh: 'SSH', action: 'ACT', thread: 'THR', set: 'SET', hist: 'HIST' }[e.kind]
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
  if (paletteKeyIs('close', ev)) return closePalette();
  if (paletteKeyIs('next', ev)) {
    palette.selected = Math.min(palette.selected + 1, palette.items.length - 1);
    renderPalette();
    ev.preventDefault();
  } else if (paletteKeyIs('prev', ev)) {
    palette.selected = Math.max(palette.selected - 1, 0);
    renderPalette();
    ev.preventDefault();
  } else {
    const save = paletteKeyIs('save', ev);
    const insert = paletteKeyIs('insert', ev);
    if (!save && !insert && !paletteKeyIs('run', ev)) return;
    const item = palette.items[palette.selected];
    if (item) {
      closePalette();
      // A save/insert key on an entry without that action just runs it.
      if (save && item.save) item.save();
      else if (insert && item.insert) item.insert();
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
/** A repeatable list editor (see buildItemListEditor) — used by the launch-set
 * editor to pick which shells/hosts + workflows it opens. */
function fieldItemList(label, name, items) {
  return { label, name, items: items || [], tag: 'itemlist' };
}
/** A non-input explanatory paragraph (dimmed, like .field-hint). */
function fieldNote(label) {
  return { tag: 'note', label };
}

/** Reads the current value of every field in the modal body. Checkboxes read
 * as booleans; shortcut inputs read their normalized combo (dataset), not the
 * pretty display value; item-list editors are read from their live DOM (see
 * readItemListRows) rather than a single input's value. */
function collectModalValues() {
  const values = {};
  for (const el of $('#modal-body').querySelectorAll('input, textarea, select')) {
    if (el.closest('[data-itemlist]') || el.closest('[data-jumplist]')) continue;
    if (el.type === 'checkbox') values[el.name] = el.checked;
    else if ('shortcut' in el.dataset) values[el.name] = el.dataset.shortcut;
    else values[el.name] = el.value;
  }
  for (const list of $('#modal-body').querySelectorAll('[data-itemlist]')) {
    values[list.dataset.name] = readItemListRows(list);
  }
  for (const list of $('#modal-body').querySelectorAll('[data-jumplist]')) {
    values[list.dataset.name] = readJumpListRows(list);
  }
  return values;
}

/* Let a textarea grow with its content instead of holding a long command in a
 * fixed slot. Growth only — never shrinking — so dragging the resize handle
 * larger sticks, and the CSS `max-height` caps it, after which the box scrolls
 * with a reserved gutter so the bar never covers text.
 *
 * Measured on the next frame because fields are built before they are in the
 * document, and `scrollHeight` reads 0 until the element has been laid out. */
function autoGrowTextarea(el) {
  const grow = () => {
    // scrollHeight excludes the border, which box-sizing:border-box counts.
    const border = el.offsetHeight - el.clientHeight;
    if (el.scrollHeight + border > el.clientHeight + border) {
      el.style.height = `${el.scrollHeight + border}px`;
    }
  };
  el.addEventListener('input', grow);
  requestAnimationFrame(grow);
}

/** `hideCancel` is for modals where the two footer buttons would mean the
 * same thing — a running transfer has nothing to confirm, so "Cancel" next to
 * "Abort" is two ways to say stop. */
function openModal({ title, okLabel, body, onOk, onCancel, extraActions, hideCancel }) {
  $('#modal-title').textContent = title;
  $('#modal-ok').textContent = okLabel || 'OK';
  $('#modal-cancel').hidden = !!hideCancel;
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
    if (f.tag === 'browser') {
      const wrap = document.createElement('div');
      wrap.className = 'rb';
      wrap.innerHTML = `
        <div class="rb-head">
          <button type="button" class="ghost-btn rb-up"></button>
          <code class="rb-path"></code>
          <button type="button" class="accent-btn rb-upload"></button>
        </div>
        <ul class="rb-list"></ul>
        <p class="rb-status"></p>`;
      wrap.querySelector('.rb-up').textContent = tr('sftp.browser.up');
      wrap.querySelector('.rb-upload').textContent = tr('sftp.browser.upload');
      wrap.querySelector('.rb-up').addEventListener('click', () => {
        if (browserState && browserState.parent != null) browserGo(browserState.parent);
      });
      wrap.querySelector('.rb-upload').addEventListener('click', browserUploadHere);
      box.appendChild(wrap);
      continue;
    }
    if (f.tag === 'progress') {
      const wrap = document.createElement('div');
      wrap.className = 'xfer';
      wrap.innerHTML = '<progress class="xfer-bar" max="100"></progress><span class="xfer-text"></span>';
      box.appendChild(wrap);
      continue;
    }
    if (f.tag === 'note') {
      const p = document.createElement('p');
      p.className = 'field-hint';
      p.textContent = f.label;
      box.appendChild(p);
      continue;
    }
    const label = document.createElement('label');
    label.textContent = f.label;
    let el;
    if (f.tag === 'textarea') {
      el = document.createElement('textarea');
      el.value = f.value;
      autoGrowTextarea(el);
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
      el.placeholder = tr('shortcut.ph');
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
    } else if (f.tag === 'itemlist') {
      el = buildItemListEditor(f.items);
    } else if (f.tag === 'jumplist') {
      el = buildJumpListEditor(f.items, f.selfId);
    } else {
      el = document.createElement('input');
      el.type = f.type || 'text';
      el.value = f.value;
      if (f.placeholder) el.placeholder = f.placeholder;
      // Keep passwords/passphrases out of the browser's autofill store.
      if (el.type === 'password') el.autocomplete = 'new-password';
      // Optional autocomplete suggestions (e.g. existing workflow groups).
      if (Array.isArray(f.list) && f.list.length) {
        const dl = document.createElement('datalist');
        dl.id = `dl-${f.name}`;
        for (const opt of f.list) {
          const o = document.createElement('option');
          o.value = opt;
          dl.appendChild(o);
        }
        el.setAttribute('list', dl.id);
        label.appendChild(dl);
      }
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
    modalCtx.err.textContent = tr('modal.requiredFields');
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

function confirmModal(message, okLabel) {
  return new Promise((resolve) => {
    openModal({
      title: message,
      okLabel: okLabel || tr('common.delete'),
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
 * each other tab (Workflows / SSH / プロファイル / 設定) takes over the whole main
 * content area with its panel, instead of a narrow right sidebar. Exactly one
 * view is shown at a time. */
let currentView = 'shell';

/** In a narrow window the header tabs scroll sideways rather than squeeze the
 * titlebar (see #header-tabs CSS). Marks the edges that have tabs hidden past
 * them so CSS can fade them, and, with `reveal`, brings the active tab into
 * view. */
function updateHeaderTabsOverflow(reveal = false) {
  const nav = $('#header-tabs');
  if (reveal) {
    const active = nav.querySelector('.hdr-tab.active');
    if (active) active.scrollIntoView({ block: 'nearest', inline: 'nearest' });
  }
  const max = nav.scrollWidth - nav.clientWidth;
  nav.classList.toggle('more-start', nav.scrollLeft > 1);
  nav.classList.toggle('more-end', nav.scrollLeft < max - 1);
}

function setView(name) {
  currentView = name;
  const isShell = name === 'shell';
  // In a tool view the terminal workspace is hidden (see #body.tool-view CSS)
  // and the panel container fills the window.
  $('#body').classList.toggle('tool-view', !isShell);
  $('#sidebar').classList.toggle('hidden', isShell);
  // The shell's own controls go with it: in a tool panel there are no panes to
  // split and no thread list to toggle, so leaving them up would offer buttons
  // that quietly do nothing.
  $('#shell-tools').classList.toggle('hidden', !isShell);
  if (!isShell) {
    document.querySelectorAll('.panel').forEach((p) =>
      p.classList.toggle('active', p.id === `panel-${name}`));
  }
  document.querySelectorAll('.hdr-tab').forEach((b) =>
    b.classList.toggle('active', b.dataset.panel === name));
  updateHeaderTabsOverflow(true);
  // Uptime and the running list are only correct as of the last read, and the
  // backend can outlive a panel that was never opened.
  if (name === 'servers') refreshServers();
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
  applyTaskPaneWidth();
});

$('#new-thread').addEventListener('click', () => newLocalThread());
$('#new-thread-menu').addEventListener('click', (ev) => {
  ev.stopPropagation();
  toggleProfileMenu(ev.currentTarget);
});
$('#split-toggle').addEventListener('click', () => toggleSplitDir('v'));
$('#split-h-toggle').addEventListener('click', () => toggleSplitDir('h'));
$('#broadcast-toggle').addEventListener('click', toggleBroadcast);
$('#palette-btn').addEventListener('click', openPalette);
$('#host-known').addEventListener('click', openKnownHosts);
$('#host-keygen').addEventListener('click', openKeygen);
$('#task-toggle').addEventListener('click', () => setTaskPane(!state.taskPane));
$('#task-close').addEventListener('click', () => setTaskPane(false));
$('#task-refresh').addEventListener('click', () => refreshTasks(true));
// Dragging the grip resizes the pane, the same as the splitter between panes.
// The terminals need no refit here: each pane's ResizeObserver already refits
// its terminal as the pane's box changes.
$('#taskpane-grip').addEventListener('mousedown', (ev) => {
  ev.preventDefault();
  // Reuses the splitter's guard, which also stops the terminals under the
  // pointer from swallowing the drag (see body.col-resizing).
  document.body.classList.add('col-resizing');
  // The pane is flush with the window's right edge, so the width the pointer
  // asks for is simply what is left of the window from it.
  const move = (e) => setTaskPaneWidth(window.innerWidth - e.clientX);
  const up = () => {
    document.body.classList.remove('col-resizing');
    window.removeEventListener('mousemove', move);
    window.removeEventListener('mouseup', up);
  };
  window.addEventListener('mousemove', move);
  window.addEventListener('mouseup', up);
});

$('#srv-start').addEventListener('submit', (ev) => {
  ev.preventDefault();
  startServer(serverFormValues());
});
$('#srv-browse').addEventListener('click', async () => {
  const dir = await window.__TAURI__.dialog.open({ directory: true, multiple: false });
  if (dir) $('#srv-root').value = dir;
});
// The shell already knows where it is (OSC 7), so the common case — "serve the
// directory I am standing in" — needs no typing and no file dialog.
$('#srv-use-cwd').addEventListener('click', () => {
  const cwd = focusedThread()?.cwd;
  if (!cwd) {
    toast(tr('srv.useCwd.none'), true);
    return;
  }
  $('#srv-root').value = cwd;
});
$('#srv-save-preset').addEventListener('click', () => {
  const values = serverFormValues();
  editServerPreset({
    name: values.root.split(/[\\/]/).filter(Boolean).pop() || '',
    root: values.root,
    port: values.port,
    spa: values.spa,
    hot_reload: values.hotReload,
    open_browser: values.openBrowser,
  });
});
$('#srv-log-clear').addEventListener('click', () => {
  state.servers.log = [];
  renderServerLog();
});

function serverFormValues() {
  return {
    root: $('#srv-root').value.trim(),
    port: Math.max(0, Math.min(65535, parseInt($('#srv-port').value, 10) || 0)),
    spa: $('#srv-spa').checked,
    hotReload: $('#srv-hot').checked,
    openBrowser: $('#srv-open').checked,
  };
}
// "Register" while a group is selected starts the new workflow in that group.
$('#wf-add').addEventListener('click', () => {
  const sel = wfUi.selected;
  editWorkflow(sel.startsWith('g:') ? newWorkflowIn(sel.slice(2)) : null);
});
$('#wf-tree-new').addEventListener('click', () => promptNewGroup(''));
$('#wf-tree-expand').addEventListener('click', () => {
  wfUi.collapsed.clear();
  saveWfUi();
  renderWorkflows();
});
$('#wf-tree-collapse').addEventListener('click', () => {
  wfUi.collapsed = new Set(foldableGroupPaths(buildWorkflowTree(state.workflows)));
  saveWfUi();
  renderWorkflows();
});
// Narrow windows stack the tree above the list; its title then folds it away.
$('#wf-tree-toggle').addEventListener('click', () => {
  const closed = $('#wf-tree').classList.toggle('closed');
  $('#wf-tree-toggle').setAttribute('aria-expanded', String(!closed));
});
$('#wf-export').addEventListener('click', exportWorkflows);
$('#wf-import').addEventListener('click', importWorkflows);
$('#settings-export').addEventListener('click', exportSettings);
$('#settings-import').addEventListener('click', importSettings);
$('#host-add').addEventListener('click', () => editHost(null));
$('#host-import').addEventListener('click', importSshConfig);
$('#profile-add').addEventListener('click', () => editProfile(null));
$('#launchset-add').addEventListener('click', () => editLaunchSet(null));
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
    item.querySelector('.pi-cmd').textContent = p.command || tr('profile.systemDefault');
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
    sshLabel.textContent = tr('menu.sshConnections');
    menu.appendChild(sshLabel);
    for (const h of state.hosts) {
      const item = document.createElement('button');
      item.className = 'popup-item';
      item.innerHTML = `<span class="pi-name"></span><span class="pi-cmd"></span>`;
      item.querySelector('.pi-name').textContent = `⇄ ${h.name || h.host}`;
      if (h.color) {
        const dot = document.createElement('span');
        dot.className = 'host-dot';
        dot.dataset.color = h.color;
        item.querySelector('.pi-name').prepend(dot);
      }
      item.querySelector('.pi-cmd').textContent =
        (groupParts(h).length ? `${groupParts(h).join(' / ')} · ` : '') + `${h.username}@${h.host}:${h.port}`;
      item.addEventListener('click', () => {
        closeProfileMenu();
        newSshThread(h);
      });
      menu.appendChild(item);
    }
  }
  // Launch sets: batch-open every item (shells/hosts + their workflows) at once.
  if (state.launchSets.length) {
    const setSep = document.createElement('div');
    setSep.className = 'popup-sep';
    menu.appendChild(setSep);
    const setLabel = document.createElement('div');
    setLabel.className = 'popup-label';
    setLabel.textContent = tr('menu.launchSets');
    menu.appendChild(setLabel);
    for (const set of state.launchSets) {
      const item = document.createElement('button');
      item.className = 'popup-item';
      item.innerHTML = `<span class="pi-name"></span><span class="pi-cmd"></span>`;
      item.querySelector('.pi-name').textContent = `▤ ${set.name}`;
      item.querySelector('.pi-cmd').textContent = tr('set.itemsSummary', { count: set.items.length, summary: launchSetSummary(set) });
      item.addEventListener('click', () => {
        closeProfileMenu();
        runLaunchSet(set);
      });
      menu.appendChild(item);
    }
  }
  const sep = document.createElement('div');
  sep.className = 'popup-sep';
  menu.appendChild(sep);
  const manage = document.createElement('button');
  manage.className = 'popup-item muted';
  manage.textContent = tr('menu.manageProfiles');
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

// The open menu chain: index 0 is the root (#term-context-menu), each deeper
// index is a flyout submenu element appended to <body>. `level` in the helpers
// below is an index into this array.
let ctxMenuStack = [];

/** Right-clicking a terminal pane opens a workflow launcher at the cursor.
 * Workflows are grouped: each group is a submenu you hover/arrow into, mirroring
 * the panel's hierarchy. Click runs the workflow in that pane; Shift+click only
 * inserts the filled command without executing it (same as the palette). */
function openTermContextMenu(x, y, path, remote, blockCommand = '') {
  closeTermContextMenu();
  const menu = $('#term-context-menu');
  ctxMenuStack = [menu];
  fillWorkflowMenu(menu, buildWorkflowTree(state.workflows), 0, true);
  // SFTP upload / download, above the workflows, for SSH threads. Prepended
  // first so the path and block sections below land above it: they are the
  // more specific things the user pointed at.
  const focused = focusedThread();
  if (remote && focused && focused.kind === 'ssh') {
    const sep = document.createElement('div');
    sep.className = 'popup-sep';
    menu.prepend(sep);
    menu.prepend(transferSection(focused));
  }
  // Above the workflows, and only when a path is actually under the cursor or
  // selected: it is the more specific thing the user pointed at, and a row
  // that is always present but usually does nothing trains people to skip it.
  // The same goes for "save as workflow": offered only over a finished block
  // whose command line could be read.
  if (blockCommand) {
    const sep = document.createElement('div');
    sep.className = 'popup-sep';
    menu.prepend(sep);
    menu.prepend(saveWorkflowItem(blockCommand));
  }
  if (path) {
    const sep = document.createElement('div');
    sep.className = 'popup-sep';
    menu.prepend(sep);
    menu.prepend(pathSection(path, remote));
  }
  // No path under the cursor: still offer the focused local thread's working
  // directory, so "open in external terminal" is reachable from anywhere.
  if (!path && focused && focused.kind !== 'ssh' && focused.cwd) {
    const cwd = focused.cwd;
    const row = document.createElement('button');
    row.type = 'button';
    row.className = 'popup-item';
    row.setAttribute('role', 'menuitem');
    row.innerHTML = `<span class="pi-line">${icon('terminal')}<span class="pi-name"></span></span>`;
    row.querySelector('.pi-name').textContent = tr('ctx.openTerminalCwd');
    row.addEventListener('mouseenter', () => closeSubmenusFrom(1));
    row.addEventListener('click', async () => {
      closeTermContextMenu();
      try {
        await invoke('open_external_terminal', { path: cwd });
      } catch (e) {
        toast(String(e), true);
      }
    });
    const sep = document.createElement('div');
    sep.className = 'popup-sep';
    menu.prepend(sep);
    menu.prepend(row);
  }
  // Copy / paste on top of everything, so paste is one click away even when
  // right-click is set to open this menu.
  if (focused) {
    const sep = document.createElement('div');
    sep.className = 'popup-sep';
    menu.prepend(sep);
    menu.prepend(clipboardSection(focused));
  }
  showRootMenu(menu, x, y);
}

/** The "copy" / "paste" rows at the top of the terminal's right-click menu.
 * Copy is disabled (not hidden, so the rows don't shift) without a selection. */
function clipboardSection(thread) {
  const frag = document.createDocumentFragment();
  const add = (text, iconName, enabled, onClick) => {
    const item = document.createElement('button');
    item.type = 'button';
    item.className = 'popup-item';
    item.setAttribute('role', 'menuitem');
    item.disabled = !enabled;
    item.innerHTML = `<span class="pi-line">${icon(iconName)}<span class="pi-name"></span></span>`;
    item.querySelector('.pi-name').textContent = text;
    item.addEventListener('mouseenter', () => closeSubmenusFrom(1));
    item.addEventListener('click', () => {
      closeTermContextMenu();
      onClick();
    });
    frag.appendChild(item);
  };
  add(tr('ctx.copy'), 'copy', thread.term.hasSelection(), () => copyFocusedSelection());
  add(tr('ctx.paste'), 'contentPaste', true, () => pasteIntoFocused());
  return frag;
}

/** The "save as workflow" row of the right-click menu, naming the command
 * so it is clear which one — the pointer may sit several lines into a block's
 * output, far from the prompt it belongs to. */
function saveWorkflowItem(command) {
  const item = document.createElement('button');
  item.type = 'button';
  item.className = 'popup-item ctx-save-workflow';
  item.setAttribute('role', 'menuitem');
  item.innerHTML = `<span class="pi-line">${icon('zap')}<span class="pi-name"></span></span>`;
  const short = command.length > 48 ? `${command.slice(0, 48)}…` : command;
  item.querySelector('.pi-name').textContent = tr('ctx.saveWorkflow', { cmd: short });
  item.addEventListener('click', () => {
    closeTermContextMenu();
    saveAsWorkflow(command);
  });
  return item;
}

/** The finished command block under a point in a thread's terminal, or null.
 * Row math as in pathAtPoint; the block is the one whose range (prompt line
 * through the line before the next prompt) contains that buffer line. */
function blockAtPoint(thread, x, y) {
  const screen = thread.hostEl.querySelector('.xterm-screen');
  if (!screen || !thread.blocks?.length) return null;
  const rect = screen.getBoundingClientRect();
  if (!rect.width || !rect.height) return null;
  if (x < rect.left || x >= rect.right || y < rect.top || y >= rect.bottom) return null;
  const row = Math.floor((y - rect.top) / (rect.height / thread.term.rows));
  const line = thread.term.buffer.active.viewportY + row;
  return thread.blocks.find((b) => {
    if (!b.done || !b.sawOutput || b.marker.isDisposed) return false;
    const { start, end } = blockRange(thread.term, thread.blocks, b);
    return line >= start && line <= end;
  }) || null;
}

/* ---------------- SFTP file transfer ---------------- */

/** Human-readable byte count. Transfers are the one place in this app where
 * the raw number is useless — nobody reads 1073741824. */
function humanBytes(n) {
  if (!Number.isFinite(n) || n < 0) return '—';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let v = n;
  let u = 0;
  while (v >= 1024 && u < units.length - 1) { v /= 1024; u++; }
  return `${u === 0 ? v : v.toFixed(1)} ${units[u]}`;
}

let xferSeq = 0;

/** Runs one transfer behind a progress modal.
 *
 * `invokeTransfer` is given the generated transfer id and returns the backend
 * promise. The modal's only button cancels: a transfer is not something you
 * "OK", and the backend stops between chunks, so the partial file is the
 * user's to deal with rather than something we silently leave looking whole
 * (the backend reports the cancel as an error for exactly that reason). */
async function runTransfer(title, subtitle, invokeTransfer) {
  const transferId = `xfer-${++xferSeq}-${Date.now()}`;
  let settled = false;
  openModal({
    title,
    okLabel: tr('sftp.cancel'),
    hideCancel: true,
    body: [{ tag: 'note', label: subtitle }, { tag: 'progress' }],
    onOk: () => { invoke('sftp_cancel', { transferId }).catch(() => {}); },
    onCancel: () => { invoke('sftp_cancel', { transferId }).catch(() => {}); },
  });

  const unlisten = await window.__TAURI__.event.listen('sftp:progress', (ev) => {
    const p = ev.payload;
    if (!p || p.transferId !== transferId || settled) return;
    const bar = $('#modal-body .xfer-bar');
    const text = $('#modal-body .xfer-text');
    if (!bar || !text) return;
    if (p.total > 0) {
      bar.value = Math.min(100, (p.transferred / p.total) * 100);
      text.textContent = `${humanBytes(p.transferred)} / ${humanBytes(p.total)}`;
    } else {
      // Size unknown: leave the bar indeterminate rather than inventing a
      // denominator, and show what has moved.
      bar.removeAttribute('value');
      text.textContent = humanBytes(p.transferred);
    }
  });

  try {
    const done = await invokeTransfer(transferId);
    settled = true;
    closeModal(false);
    toast(tr('sftp.done', { bytes: humanBytes(done.bytes), path: done.remotePath }));
    return true;
  } catch (e) {
    settled = true;
    closeModal(false);
    toast(String(e), true);
    return false;
  } finally {
    unlisten();
  }
}

/* ---- drag & drop / context-menu upload ---- */

/** Quotes a local path for pasting at a shell prompt: bare when it is plain,
 * otherwise single-quoted (POSIX) or double-quoted (Windows). */
function shellQuotePath(p) {
  if (/^[A-Za-z0-9_\-.\/:\\~+=@%]+$/.test(p)) return p;
  return navigator.userAgent.includes('Windows')
    ? `"${p.replace(/"/g, '""')}"`
    : `'${p.replace(/'/g, `'\\''`)}'`;
}

function baseName(p) {
  const s = String(p).replace(/[\\/]+$/, '');
  const i = Math.max(s.lastIndexOf('/'), s.lastIndexOf('\\'));
  return i >= 0 ? s.slice(i + 1) : s;
}

/** The pane body under a physical-pixel window position (Tauri's drag-drop
 * events report physical coordinates), or null. */
function paneBodyAtPhysical(pos) {
  if (!pos) return null;
  const scale = window.devicePixelRatio || 1;
  const el = document.elementFromPoint(pos.x / scale, pos.y / scale);
  return el ? el.closest('.pane-body') : null;
}

function clearDropTargets() {
  document.querySelectorAll('.pane-body.drop-target').forEach((el) => el.classList.remove('drop-target'));
}

/** OS file drops. Tauri delivers them as window events rather than DOM drop
 * events (the webview never sees the files). A drop on an SSH pane uploads
 * over that thread's SFTP; on a local pane it pastes the quoted path(s) at
 * the prompt, like most terminals do. */
appWindow.onDragDropEvent((ev) => {
  const pl = ev.payload;
  if (pl.type === 'enter' || pl.type === 'over') {
    const body = paneBodyAtPhysical(pl.position);
    clearDropTargets();
    if (body) body.classList.add('drop-target');
    return;
  }
  clearDropTargets();
  if (pl.type !== 'drop' || !pl.paths?.length) return;
  const body = paneBodyAtPhysical(pl.position);
  const idx = body ? state.panes.findIndex((p) => p.body === body) : state.focusedPane;
  if (idx < 0) return;
  if (idx !== state.focusedPane) focusPane(idx);
  const thread = threadById(state.panes[idx].threadId);
  if (!thread) return;
  if (thread.kind === 'ssh') {
    uploadPathsToThread(thread, pl.paths);
  } else {
    const text = pl.paths.map(shellQuotePath).join(' ');
    invoke('session_write', { id: thread.id, data: text }).catch((e) => toast(String(e), true));
    thread.term.focus();
  }
}).catch(() => {});

/** Context-menu "Upload…": native picker (files, or one folder), then the
 * same flow as a drop. */
async function pickAndUpload(thread, directory = false) {
  let paths;
  try {
    paths = await window.__TAURI__.dialog.open({ multiple: !directory, directory });
  } catch (e) {
    toast(String(e), true);
    return;
  }
  if (!paths) return;
  uploadPathsToThread(thread, Array.isArray(paths) ? paths : [paths]);
}

/** Confirms the remote destination (the shell's OSC 7 working directory when
 * known, else the SFTP login directory), then uploads each path one after
 * another through the existing progress modal. Sequential on purpose: one
 * SFTP pipeline saturates the link, and a failure stops the rest rather than
 * leaving a half-landed batch to untangle. */
async function uploadPathsToThread(thread, paths) {
  if (thread.kind !== 'ssh') {
    toast(tr('sftp.needSsh'), true);
    return;
  }
  let dir = thread.cwd || '';
  if (!dir) {
    try {
      dir = (await invoke('sftp_list', { sessionId: thread.id, path: '.' })).path;
    } catch (e) {
      toast(String(e), true);
      return;
    }
  }
  const names = paths.map(baseName);
  const list = names.length > 4 ? `${names.slice(0, 4).join(', ')} …` : names.join(', ');
  openModal({
    title: tr('sftp.drop.title'),
    okLabel: tr('common.ok'),
    body: [
      fieldNote(tr('sftp.drop.files', { n: paths.length, list })),
      field(tr('sftp.drop.dir'), 'dir', dir, { required: true }),
      fieldNote(tr('sftp.drop.hint')),
    ],
    onOk: (v) => {
      const remoteDir = (v.dir || '').trim();
      const sessionId = thread.id;
      // Deferred a turn: submitModal empties #modal-body after onOk, and the
      // progress modal must not be opened underneath that.
      setTimeout(async () => {
        for (const p of paths) {
          // eslint-disable-next-line no-await-in-loop
          const ok = await runTransfer(
            tr('sftp.upload.title'),
            tr('sftp.drop.item', { name: baseName(p), dir: remoteDir }),
            (transferId) => invoke('sftp_upload_into', {
              sessionId, localPath: String(p), remoteDir, transferId,
            }),
          );
          if (!ok) break;
        }
        thread.term?.focus();
      }, 0);
    },
  });
}

/** SFTP rows for the terminal's right-click menu (SSH threads only — a
 * local shell has nothing to transfer to). */
function transferSection(thread) {
  const frag = document.createDocumentFragment();
  const label = document.createElement('div');
  label.className = 'popup-label';
  label.textContent = tr('menu.transfer');
  frag.appendChild(label);
  const mk = (ico, text, onClick) => {
    const item = document.createElement('button');
    item.type = 'button';
    item.className = 'popup-item';
    item.setAttribute('role', 'menuitem');
    item.innerHTML = `<span class="pi-line">${icon(ico)}<span class="pi-name"></span></span>`;
    item.querySelector('.pi-name').textContent = text;
    item.addEventListener('click', () => { closeTermContextMenu(); onClick(); });
    return item;
  };
  frag.appendChild(mk('upload', tr('menu.upload'), () => pickAndUpload(thread, false)));
  frag.appendChild(mk('folder', tr('menu.uploadDir'), () => pickAndUpload(thread, true)));
  frag.appendChild(mk('download', tr('menu.download'), () => openRemoteBrowser(thread.cwd || undefined)));
  return frag;
}

/** The SSH thread a transfer would run on, or null with a toast explaining
 * why not. Transfers ride the focused thread's existing connection, so there
 * has to be one and it has to be SSH. */
function transferThread() {
  const t = focusedThread();
  if (!t || t.kind !== 'ssh') {
    toast(tr('sftp.needSsh'), true);
    return null;
  }
  return t;
}

/** Upload: pick the local file first (that is the one the OS can offer a
 * picker for), then ask where it goes on the server. */
async function sftpUpload() {
  const thread = transferThread();
  if (!thread) return;
  let localPath;
  try {
    localPath = await window.__TAURI__.dialog.open({ multiple: false, directory: false });
  } catch (e) {
    toast(String(e), true);
    return;
  }
  if (!localPath) return; // cancelled
  const name = String(localPath).split(/[\\/]/).pop();
  const remotePath = await promptText(tr('sftp.upload.title'), tr('sftp.remotePath'), `./${name}`,
    tr('sftp.remotePath.hint'));
  if (remotePath === null) return;
  await runTransfer(
    tr('sftp.upload.title'),
    `${localPath}  →  ${remotePath}`,
    (transferId) => invoke('sftp_upload', {
      sessionId: thread.id, localPath: String(localPath), remotePath, transferId,
    }),
  );
}

/** Download: ask for the remote path, then let the OS pick where to put it. */
async function sftpDownload() {
  const thread = transferThread();
  if (!thread) return;
  const remotePath = await promptText(tr('sftp.download.title'), tr('sftp.remotePath'), '',
    tr('sftp.remotePath.hint'));
  if (remotePath === null || !remotePath.trim()) return;
  const suggested = remotePath.split('/').pop() || 'download';
  let localPath;
  try {
    localPath = await window.__TAURI__.dialog.save({ defaultPath: suggested });
  } catch (e) {
    toast(String(e), true);
    return;
  }
  if (!localPath) return; // cancelled
  await runTransfer(
    tr('sftp.download.title'),
    `${remotePath}  →  ${localPath}`,
    (transferId) => invoke('sftp_download', {
      sessionId: thread.id, remotePath, localPath: String(localPath), transferId,
    }),
  );
}

/** Remote file browser: navigate the server's filesystem over the SFTP
 * channel of the focused SSH thread, and transfer from where you are.
 *
 * Typing a path from memory was the only way to reach a file, which is fine
 * when you already know it and useless when you do not — the case a browser
 * exists for. Navigation goes by the path the *server* resolved, so `~` and
 * `..` never accumulate in what is displayed.
 *
 * It is deliberately read-only apart from transferring: no delete, rename or
 * mkdir. Those are destructive operations on someone's server and want their
 * own confirmation design; a browser that can lose files is worse than one
 * that cannot yet create a directory. */
let browserState = null;

async function openRemoteBrowser(startPath) {
  const thread = transferThread();
  if (!thread) return;
  browserState = { sessionId: thread.id, path: startPath || '.', parent: null, entries: [], busy: false };
  openModal({
    title: tr('sftp.browser.title'),
    okLabel: tr('common.close'),
    hideCancel: true,
    body: [{ tag: 'browser' }],
    onOk: () => { browserState = null; },
    onCancel: () => { browserState = null; },
  });
  await browserGo(browserState.path);
}

/** Loads one directory and repaints. Failures leave the previous listing on
 * screen — losing your place because one directory was unreadable is worse
 * than the error. */
async function browserGo(path) {
  if (!browserState || browserState.busy) return;
  browserState.busy = true;
  browserSetStatus(tr('sftp.browser.loading'));
  try {
    const res = await invoke('sftp_list', { sessionId: browserState.sessionId, path });
    browserState.path = res.path;
    browserState.parent = res.parent;
    browserState.entries = res.entries;
    renderBrowser();
  } catch (e) {
    browserSetStatus(String(e), true);
  } finally {
    browserState.busy = false;
  }
}

function browserSetStatus(text, isError) {
  const el = $('#modal-body .rb-status');
  if (!el) return;
  el.textContent = text;
  el.classList.toggle('err', !!isError);
}

/** Unix mtime (seconds) as a compact local date. */
function browserDate(mtime) {
  if (!mtime) return '';
  const d = new Date(mtime * 1000);
  const p = (n) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}

function renderBrowser() {
  const root = $('#modal-body .rb');
  if (!root || !browserState) return;
  const { path, parent, entries } = browserState;
  $('#modal-body .rb-path').textContent = path;
  $('#modal-body .rb-path').title = path;

  const list = $('#modal-body .rb-list');
  list.innerHTML = '';

  if (parent !== null && parent !== undefined) {
    list.appendChild(browserRow({ name: '..', path: parent, isDir: true }, true));
  }
  for (const e of entries) list.appendChild(browserRow(e, false));
  if (!entries.length) {
    const empty = document.createElement('li');
    empty.className = 'rb-empty';
    empty.textContent = tr('sftp.browser.empty');
    list.appendChild(empty);
  }
  browserSetStatus(tr('sftp.browser.count', { n: entries.length }));
}

/** One row. Directories navigate; files download. A row is a button so the
 * keyboard reaches it the same way the mouse does. */
function browserRow(entry, isUp) {
  const li = document.createElement('li');
  const btn = document.createElement('button');
  btn.type = 'button';
  btn.className = `rb-row${entry.isDir ? ' dir' : ''}`;
  btn.innerHTML = `
    <span class="rb-ico">${icon(entry.isDir ? 'folder' : 'file')}</span>
    <span class="rb-name"></span>
    <span class="rb-size"></span>
    <span class="rb-date"></span>`;
  btn.querySelector('.rb-name').textContent =
    entry.name + (entry.isSymlink ? ' →' : '');
  btn.querySelector('.rb-size').textContent =
    entry.isDir || entry.size == null ? '' : humanBytes(entry.size);
  btn.querySelector('.rb-date').textContent = isUp ? '' : browserDate(entry.mtime);
  btn.title = entry.path;
  btn.addEventListener('click', () => {
    if (entry.isDir) browserGo(entry.path);
    else browserDownloadEntry(entry);
  });
  li.appendChild(btn);
  return li;
}

/** Downloads the clicked file: the OS picks where, the name is suggested. */
async function browserDownloadEntry(entry) {
  const state0 = browserState;
  let localPath;
  try {
    localPath = await window.__TAURI__.dialog.save({ defaultPath: entry.name });
  } catch (e) {
    toast(String(e), true);
    return;
  }
  if (!localPath) return; // cancelled
  const sessionId = state0.sessionId;
  // The transfer takes over the modal, so the browser is closed first and
  // reopened afterwards at the same directory — stacking two modals on the
  // one #modal element would leave the browser half-erased. If the browser
  // was dismissed while the OS dialog was up, it stays dismissed: reopening
  // a window the user just closed is not a courtesy.
  const back = browserState ? state0.path : null;
  closeModal(false);
  browserState = null;
  await runTransfer(
    tr('sftp.download.title'),
    `${entry.path}  →  ${localPath}`,
    (transferId) => invoke('sftp_download', {
      sessionId, remotePath: entry.path, localPath: String(localPath), transferId,
    }),
  );
  if (back !== null) await openRemoteBrowser(back);
}

/** Uploads into the directory currently shown. */
async function browserUploadHere() {
  const state0 = browserState;
  if (!state0) return;
  let localPath;
  try {
    localPath = await window.__TAURI__.dialog.open({ multiple: false, directory: false });
  } catch (e) {
    toast(String(e), true);
    return;
  }
  if (!localPath) return;
  const name = String(localPath).split(/[\\/]/).pop();
  const remotePath = `${state0.path.replace(/\/+$/, '')}/${name}`;
  const sessionId = state0.sessionId;
  const back = browserState ? state0.path : null;
  closeModal(false);
  browserState = null;
  await runTransfer(
    tr('sftp.upload.title'),
    `${localPath}  →  ${remotePath}`,
    (transferId) => invoke('sftp_upload', {
      sessionId, localPath: String(localPath), remotePath, transferId,
    }),
  );
  if (back !== null) await openRemoteBrowser(back);
}

/** One-field text prompt. Resolves to the trimmed value, or null on cancel.
 *
 * The resolve is deferred by a turn on purpose. `submitModal` awaits `onOk`
 * and *then* empties `#modal-body`; a caller that opens its own modal the
 * moment this resolves would have that body wiped out from under it. Handing
 * the result back on a macrotask puts the caller strictly after the close. */
function promptText(title, label, value, hint) {
  return new Promise((resolve) => {
    const body = [field(label, 'value', value, { required: true })];
    if (hint) body.push(fieldNote(hint));
    const later = (v) => setTimeout(() => resolve(v), 0);
    openModal({
      title,
      okLabel: tr('common.ok'),
      body,
      onOk: (values) => later((values.value || '').trim()),
      onCancel: () => later(null),
    });
  });
}

/* ---------------- filesystem paths in terminal output ---------------- */

/* Absolute paths only. A relative path (`src/lib.rs`) has to be resolved
 * against the shell's working directory. `thread.cwd` now carries that (OSC 7,
 * see createThread), but only for local shells with integration enabled — for
 * an SSH thread, or one whose shell reports nothing, it is empty, and
 * resolving against the app's own cwd would silently open a different file.
 * Widening this to relative paths therefore means handling the unknown-cwd
 * case first, which is why it still stops at absolute ones. */
const PATH_PATTERNS = [
  // POSIX absolute, and `~` / `~/…` (expanded by the backend, which is the
  // side that knows this machine's home).
  /(?:^|[\s"'`([{<>=,])((?:~|\/)[^\s"'`)\]}<>|]*)/g,
  // Windows drive-absolute (C:\… and C:/…) and UNC (\\server\share).
  /(?:^|[\s"'`([{<>=,])([A-Za-z]:[\\/][^\s"'`)\]}<>|]*|\\\\[^\s"'`)\]}<>|]+)/g,
];

/* Trailing characters that end a sentence rather than a path. Stripped from
 * the right, repeatedly, because build output produces things like
 * `(/usr/lib/libfoo.so).` — the dot and paren are punctuation, not filename. */
const PATH_TRAILING_JUNK = /[.,:;!?)\]}'"`]+$/;

/* Compiler and grep output pin a location as `path:line` or `path:line:col`.
 * The digits are not part of the filename, and leaving them attached makes
 * every such path fail to resolve. */
const PATH_LINE_SUFFIX = /:\d+(?::\d+)?$/;

/** Trims a raw match down to the path itself, or returns '' if nothing
 * usable is left. */
function cleanPathMatch(raw) {
    let p = raw.replace(PATH_TRAILING_JUNK, '');
    p = p.replace(PATH_LINE_SUFFIX, '');
    p = p.replace(PATH_TRAILING_JUNK, '');
    // A bare `/` or `~` is not worth linking, and neither is a scheme's
    // slashes (`//host` out of `https://host`).
    if (p.length < 2 || p === '~/' || p.startsWith('//')) return '';
    return p;
}

/** Finds absolute paths in one line of terminal text, as `{ text, start, end }`
 * with 0-based, end-exclusive column offsets.
 *
 * URLs are blanked out first: `https://host/a/b` contains `/a/b`, and linking
 * that as a path would put a second, wrong link on top of the one
 * WebLinksAddon already owns. */
function findPathsInLine(line) {
  const masked = line.replace(/[a-z][a-z0-9+.-]*:\/\/\S*/gi, (m) => ' '.repeat(m.length));
  const out = [];
  for (const re of PATH_PATTERNS) {
    re.lastIndex = 0;
    let m;
    while ((m = re.exec(masked)) !== null) {
      const start = m.index + m[0].length - m[1].length;
      const text = cleanPathMatch(m[1]);
      if (!text) continue;
      // Overlapping hits (a Windows path also matching the POSIX pattern
      // after its drive letter) keep the first, longest one.
      if (out.some((o) => start < o.end && start + text.length > o.start)) continue;
      out.push({ text, start, end: start + text.length });
    }
  }
  return out;
}

/** Menu rows for one path: show it in the OS file manager, or copy it.
 *
 * Built as a fragment because two menus need exactly these rows — the popup a
 * linked path opens on click, and the top of the terminal's right-click menu —
 * and two copies of them would drift apart.
 *
 * `remote` paths (an SSH thread's output) only offer the copy: the path names
 * a file on the server, and revealing it locally would open an unrelated file
 * that happens to share the name, or nothing at all. Saying why beats a
 * button that quietly fails. */
function pathSection(path, remote) {
  const frag = document.createDocumentFragment();
  const label = document.createElement('div');
  label.className = 'popup-label';
  label.textContent = path;
  label.title = path;
  frag.appendChild(label);

  const add = (text, iconName, onClick) => {
    const item = document.createElement('button');
    item.className = 'popup-item';
    item.setAttribute('role', 'menuitem');
    item.innerHTML = `<span class="pi-line">${icon(iconName)}<span class="pi-name"></span></span>`;
    item.querySelector('.pi-name').textContent = text;
    // Dismiss any open workflow flyout on hover, the same as the workflow rows
    // this sits above in the right-click menu.
    item.addEventListener('mouseenter', () => closeSubmenusFrom(1));
    item.addEventListener('click', () => {
      closeTermContextMenu();
      onClick();
    });
    frag.appendChild(item);
  };

  if (remote) {
    const note = document.createElement('div');
    note.className = 'popup-label';
    note.textContent = tr('path.remoteNote');
    frag.appendChild(note);
  } else {
    add(tr('path.reveal'), 'folder', async () => {
      try {
        await invoke('reveal_path', { path });
      } catch (e) {
        toast(String(e), true);
      }
    });
    add(tr('path.openTerminal'), 'terminal', async () => {
      try {
        await invoke('open_external_terminal', { path });
      } catch (e) {
        toast(String(e), true);
      }
    });
  }

  add(tr('path.copy'), 'copy', async () => {
    try {
      await navigator.clipboard.writeText(path);
      toast(tr('path.copied'));
    } catch (e) {
      toast(tr('toast.copyFailed', { e }), true);
    }
  });
  return frag;
}

function openPathMenu(x, y, path, remote) {
  closeTermContextMenu();
  const menu = $('#term-context-menu');
  ctxMenuStack = [menu];
  menu.innerHTML = '';
  menu.appendChild(pathSection(path, remote));
  showRootMenu(menu, x, y);
}

/** The path under a pixel position, or ''.
 *
 * xterm publishes no hit-test, but its grid is uniform, so the screen
 * element's box divided by the terminal's own `cols`/`rows` gives the cell.
 * Measuring the element rather than the font is what keeps this in step with
 * whatever the renderer actually laid out — zoom, fractional DPI, or a
 * fallback font being used for that row. */
function pathAtPoint(thread, x, y) {
  const screen = thread.hostEl.querySelector('.xterm-screen');
  if (!screen) return '';
  const rect = screen.getBoundingClientRect();
  if (!rect.width || !rect.height) return '';
  if (x < rect.left || x >= rect.right || y < rect.top || y >= rect.bottom) return '';
  const col = Math.floor((x - rect.left) / (rect.width / thread.term.cols));
  const buffer = thread.term.buffer.active;
  const row = Math.floor((y - rect.top) / (rect.height / thread.term.rows));
  const line = buffer.getLine(buffer.viewportY + row);
  if (!line) return '';
  const hit = findPathsInLine(line.translateToString(true))
    .find((h) => col >= h.start && col < h.end);
  return hit ? hit.text : '';
}

/** A path the user selected, or ''.
 *
 * Only a selection that is *entirely* one path counts. Revealing something
 * found inside a selected sentence would act on a path the user never pointed
 * at, which is worse than offering nothing. */
function pathFromSelection(term) {
  const selected = (term.getSelection() || '').trim();
  if (!selected || /\s/.test(selected)) return '';
  const hits = findPathsInLine(selected);
  return hits.length === 1 && hits[0].text === selected ? selected : '';
}

/** Registers the path link provider on one terminal. Clicking a path opens
 * the menu above rather than acting immediately: "copy" and "reveal" are both
 * plausible intents, and picking one for the user gets it wrong half the
 * time. */
function registerPathLinks(term, remote) {
  term.registerLinkProvider({
    provideLinks(bufferLineNumber, callback) {
      const line = term.buffer.active.getLine(bufferLineNumber - 1);
      if (!line) return callback(undefined);
      // Wrapped lines are scanned as-is: joining them would need the whole
      // logical row, and a path split across a wrap is rare enough that
      // half-linking it is worse than not linking it.
      const text = line.translateToString(true);
      const hits = findPathsInLine(text);
      if (!hits.length) return callback(undefined);
      callback(hits.map((h) => ({
        // xterm ranges are 1-based and inclusive on both ends.
        range: {
          start: { x: h.start + 1, y: bufferLineNumber },
          end: { x: h.end, y: bufferLineNumber },
        },
        text: h.text,
        activate: (ev) => {
          ev.preventDefault();
          openPathMenu(ev.clientX, ev.clientY, h.text, remote);
        },
      })));
    },
  });
}

/** Reveals the shared root menu at the cursor and arms its dismissal. Shared
 * by every menu that uses `#term-context-menu`, so they all get the same
 * clamping, focus handling and outside-click behaviour. */
function showRootMenu(menu, x, y) {
  // Position at the cursor, clamped inside the window (unhide first so the
  // menu has measurable dimensions).
  menu.classList.remove('hidden');
  menu.style.left = `${Math.max(8, Math.min(x, window.innerWidth - menu.offsetWidth - 8))}px`;
  menu.style.top = `${Math.max(8, Math.min(y, window.innerHeight - menu.offsetHeight - 8))}px`;
  // Focus the first row so ↑/↓/→/←/Enter/Escape work immediately.
  menu.querySelector('button.popup-item:not(:disabled)')?.focus();
  setTimeout(() => window.addEventListener('mousedown', termCtxOutsideClick), 0);
}

/** Populates a menu element from a workflow-tree node: subgroups (as flyout
 * openers) first, then the node's own workflows. The root also gets the header
 * label and the manage/register footer. */
function fillWorkflowMenu(menuEl, node, level, isRoot) {
  menuEl.innerHTML = '';
  if (isRoot) {
    const label = document.createElement('div');
    label.className = 'popup-label';
    label.textContent = tr('menu.runWorkflow');
    menuEl.appendChild(label);
    if (!state.workflows.length) {
      const empty = document.createElement('div');
      empty.className = 'popup-empty';
      empty.textContent = tr('menu.workflowsEmpty');
      menuEl.appendChild(empty);
    }
  }
  for (const [, child] of node.groups) menuEl.appendChild(groupMenuItem(child, level));
  for (const w of node.workflows) menuEl.appendChild(workflowMenuItem(w, level));
  if (isRoot) {
    const sep = document.createElement('div');
    sep.className = 'popup-sep';
    menuEl.appendChild(sep);
    const manage = document.createElement('button');
    manage.className = 'popup-item muted';
    manage.setAttribute('role', 'menuitem');
    manage.textContent = state.workflows.length ? tr('menu.manageWorkflows') : tr('menu.registerWorkflow');
    manage.addEventListener('mouseenter', () => closeSubmenusFrom(1));
    manage.addEventListener('click', () => {
      closeTermContextMenu();
      if (state.workflows.length) setView('workflows');
      else editWorkflow(null);
    });
    menuEl.appendChild(manage);
  }
}

/** A leaf (runnable) workflow row. Hovering it collapses any deeper submenu. */
function workflowMenuItem(w, level) {
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
  item.title = tr('menu.wfHint', { desc: w.description || w.command });
  item.addEventListener('mouseenter', () => closeSubmenusFrom(level + 1));
  item.addEventListener('click', (ev) => {
    closeTermContextMenu();
    runWorkflow(w, !ev.shiftKey);
  });
  return item;
}

/** A group row that opens a flyout submenu on hover / click / →. */
function groupMenuItem(node, level) {
  const item = document.createElement('button');
  item.className = 'popup-item popup-group';
  item.setAttribute('role', 'menuitem');
  item.setAttribute('aria-haspopup', 'true');
  item.innerHTML = `
    <span class="pi-line">${icon('folder')}<span class="pi-name"></span><span class="pi-count"></span>${icon('chevronRight')}</span>`;
  item.querySelector('.pi-name').textContent = node.name;
  item.querySelector('.pi-count').textContent = countWorkflows(node);
  const open = () => openWorkflowSubmenu(item, node, level + 1);
  item._openSubmenu = open;
  item.addEventListener('mouseenter', open);
  item.addEventListener('click', open);
  return item;
}

/** Opens (or replaces) the flyout submenu for a group at `level`, positioned to
 * the parent item's right and flipped left / clamped up when it would overflow. */
function openWorkflowSubmenu(parentItem, node, level) {
  closeSubmenusFrom(level);
  ctxMenuStack[level - 1]?.querySelectorAll('.popup-group.open')
    .forEach((el) => el.classList.remove('open'));
  parentItem.classList.add('open');

  const sub = document.createElement('div');
  sub.className = 'popup-menu term-context-submenu';
  sub.setAttribute('role', 'menu');
  sub._parentItem = parentItem;
  document.body.appendChild(sub);
  fillWorkflowMenu(sub, node, level, false);
  sub.addEventListener('keydown', termMenuKeydown);

  const r = parentItem.getBoundingClientRect();
  let left = r.right - 4;
  if (left + sub.offsetWidth > window.innerWidth - 8) left = Math.max(8, r.left - sub.offsetWidth + 4);
  let top = r.top - 5;
  if (top + sub.offsetHeight > window.innerHeight - 8) top = Math.max(8, window.innerHeight - sub.offsetHeight - 8);
  sub.style.left = `${left}px`;
  sub.style.top = `${top}px`;

  ctxMenuStack[level] = sub;
  ctxMenuStack.length = level + 1;
  return sub;
}

/** Removes every open submenu at `level` or deeper, and clears the "open"
 * highlight on the parent menu's group rows. Never removes the root (index 0). */
function closeSubmenusFrom(level) {
  const lvl = Math.max(1, level);
  ctxMenuStack[lvl - 1]?.querySelectorAll?.('.popup-group.open')
    .forEach((el) => el.classList.remove('open'));
  for (let i = ctxMenuStack.length - 1; i >= lvl; i--) {
    ctxMenuStack[i]?.remove();
    ctxMenuStack[i] = undefined;
  }
  ctxMenuStack.length = Math.min(ctxMenuStack.length, lvl);
}

function closeTermContextMenu() {
  for (let i = ctxMenuStack.length - 1; i >= 1; i--) ctxMenuStack[i]?.remove();
  ctxMenuStack = [];
  const root = $('#term-context-menu');
  root.classList.add('hidden');
  root.querySelectorAll('.popup-group.open').forEach((el) => el.classList.remove('open'));
  window.removeEventListener('mousedown', termCtxOutsideClick);
}

function termCtxOutsideClick(ev) {
  if (ctxMenuStack.some((m) => m && m.contains(ev.target))) return;
  closeTermContextMenu();
}

/** Effective right-click action: the `right_click` setting, with empty or
 * unknown values following the platform (menu on macOS, copy/paste elsewhere). */
function rightClickMode() {
  const v = state.settings.right_click;
  if (v === 'copy_paste' || v === 'menu') return v;
  return IS_MAC ? 'menu' : 'copy_paste';
}

$('#terminals').addEventListener('contextmenu', (ev) => {
  const body = ev.target.closest('.pane-body');
  if (!body) return; // no action here — the global suppressor below handles it
  ev.preventDefault();
  // Run in the pane that was right-clicked, not whichever had focus.
  const idx = state.panes.findIndex((p) => p.body === body);
  if (idx >= 0 && idx !== state.focusedPane) focusPane(idx);
  // A selection wins over whatever the pointer happens to be over: selecting a
  // path and then right-clicking it is a deliberate aim, and it also covers the
  // paths the link detector does not recognize.
  const thread = idx >= 0 ? threadById(state.panes[idx].threadId) : null;
  // Windows Terminal style: copy the selection (clearing it), else paste.
  // Shift+right-click always opens the menu so it stays reachable.
  if (thread && !ev.shiftKey && rightClickMode() === 'copy_paste') {
    if (thread.term.hasSelection()) copyFocusedSelection({ clear: true });
    else pasteIntoFocused();
    return;
  }
  const path = thread
    ? pathFromSelection(thread.term) || pathAtPoint(thread, ev.clientX, ev.clientY)
    : '';
  const block = thread ? blockAtPoint(thread, ev.clientX, ev.clientY) : null;
  const blockCommand = block ? blockCommandText(thread.term, block) : '';
  openTermContextMenu(ev.clientX, ev.clientY, path, thread?.kind === 'ssh', blockCommand);
});

// Global fallback: suppress the webview's native context menu (Reload /
// Inspect Element / …) everywhere it has no defined action. Only two kinds
// of places DO have one — a text input/textarea (real cut/copy/paste) and a
// terminal pane body (its own workflow-launcher menu, handled above) — so
// this only needs to special-case those two and preventDefault on everything
// else (header chrome, sidebar rows, cards, buttons, blank space, …).
document.addEventListener('contextmenu', (ev) => {
  if (ev.target.closest('.pane-body')) return;
  if (ev.target.closest('input, textarea')) return;
  ev.preventDefault();
});

/** Keyboard nav shared by the root menu and every flyout submenu: ↑/↓ move
 * within a menu (wrapping), → opens a group's submenu, ← returns to the parent,
 * Escape closes everything. Enter/Space activate the focused row natively. */
function termMenuKeydown(ev) {
  const menu = ev.currentTarget;
  const items = [...menu.querySelectorAll('button.popup-item:not(:disabled)')];
  const idx = items.indexOf(document.activeElement);
  const cur = items[idx];
  const level = ctxMenuStack.indexOf(menu);
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
  } else if (ev.key === 'ArrowRight' || (ev.key === 'Enter' && cur?.classList.contains('popup-group'))) {
    if (cur?.classList.contains('popup-group')) {
      ev.preventDefault();
      cur._openSubmenu().querySelector('button.popup-item')?.focus();
    }
  } else if (ev.key === 'ArrowLeft') {
    if (level > 0) {
      ev.preventDefault();
      const parentItem = menu._parentItem;
      closeSubmenusFrom(level);
      parentItem?.focus();
    }
  }
}
$('#term-context-menu').addEventListener('keydown', termMenuKeydown);

// A stale menu position makes no sense after a resize — just close it.
window.addEventListener('resize', closeTermContextMenu);
window.addEventListener('resize', applyTaskPaneWidth);
// Watch the strip and every tab rather than the window: the room also changes
// when #shell-tools hides or the platform inset applies, and the tabs' own
// widths change with the UI language.
{
  const ro = new ResizeObserver(() => updateHeaderTabsOverflow(true));
  ro.observe($('#header-tabs'));
  document.querySelectorAll('.hdr-tab').forEach((t) => ro.observe(t));
}
$('#header-tabs').addEventListener('scroll', () => updateHeaderTabsOverflow());
// A mouse wheel only scrolls vertically; turn it sideways over the tab strip so
// clipped tabs are reachable without a trackpad.
$('#header-tabs').addEventListener('wheel', (e) => {
  const nav = e.currentTarget;
  if (nav.scrollWidth <= nav.clientWidth || Math.abs(e.deltaX) > Math.abs(e.deltaY)) return;
  e.preventDefault();
  nav.scrollLeft += e.deltaY;
}, { passive: false });

/* ---------------- window controls (custom titlebar) ---------------- */

async function refreshMaxIcon() {
  const maxBtn = document.querySelector('#win-controls .win-btn[data-win="maximize"]');
  if (!maxBtn) return;
  const isMax = await appWindow.isMaximized();
  // Restore (overlapping squares) vs maximize (single square).
  maxBtn.textContent = isMax ? '❐' : '□';
  maxBtn.title = isMax ? tr('win.restore') : tr('win.max');
}

for (const btn of document.querySelectorAll('.win-btn')) {
  btn.addEventListener('click', async () => {
    const action = btn.dataset.win;
    if (action === 'minimize') await appWindow.minimize();
    else if (action === 'maximize') { await appWindow.toggleMaximize(); refreshMaxIcon(); }
    // No confirmation here: `onCloseRequested` below runs for this close too,
    // and asking in both places would prompt twice.
    else if (action === 'close') await appWindow.close();
  });
}

/* Closing kills every live shell/SSH session, so confirm while any are open.
 *
 * This hangs off the window event rather than our ✕ button because that button
 * is not the only way out: macOS keeps its native traffic lights (we draw none
 * there), and ⌘W / the Window menu close the same way. Guarding the event
 * covers all of them.
 *
 * Registering this listener changes how the window closes. Tauri sees a
 * frontend listener for `close-requested`, so it *always* prevents the close
 * and waits for us — nothing shuts the window unless this handler ends in
 * `destroy()`. Two consequences the code has to respect: `destroy` needs its
 * own ACL permission (see capabilities/default.json — with only `close`
 * granted the window could never be closed at all), and a handler that throws
 * or never settles traps the user in the app with no way out. Hence the
 * catch-all below: if confirming breaks, quitting still wins. */
let quitting = false;
appWindow.onCloseRequested(async (event) => {
  if (!state.threads.length) return; // nothing running — let the default close proceed
  event.preventDefault();
  if (quitting) return; // a second click while the dialog is up must not stack dialogs
  quitting = true;
  let quit = false;
  try {
    quit = await confirmModal(tr('confirm.sessionsRunning', { count: state.threads.length }),
                              tr('confirm.quit'));
  } catch (e) {
    quit = true; // a broken confirm is no reason to make the app unquittable
  }
  quitting = false;
  // `destroy()` rather than `close()`: close() re-enters this same handler,
  // which only works while a flag says "already asked" — destroy ends it here.
  if (quit) await appWindow.destroy();
});

// Double-clicking the empty part of the titlebar toggles maximize (native feel).
$('#topbar').addEventListener('dblclick', (ev) => {
  if (ev.target.closest('button, select, input, .win-controls')) return;
  appWindow.toggleMaximize().then(refreshMaxIcon);
});

appWindow.onResized(() => refreshMaxIcon());

const IS_MAC = /Mac/i.test(navigator.userAgent);

// Tag the body with the OS so CSS can place window controls natively. On macOS
// the traffic lights are the *real* ones, drawn by the OS over the transparent
// title bar (`titleBarStyle: "Overlay"`, see tauri.macos.conf.json) — the CSS
// hides our drawn controls there and insets the bar so it clears them.
(function detectPlatform() {
  const ua = navigator.userAgent;
  const os = IS_MAC ? 'macos' : /Win/i.test(ua) ? 'windows' : 'linux';
  document.body.classList.add(`platform-${os}`);
  // The topbar's palette chip is filled in by `renderPaletteKeyUi`.
})();

/* The app-level modifier is ⌘ on macOS and Ctrl everywhere else.
 *
 * It deliberately is not "Ctrl or ⌘": on macOS Ctrl belongs to the shell —
 * Ctrl+C is SIGINT, Ctrl+D is EOF, Ctrl+R is history search — so treating it
 * as an app modifier there would quietly eat the keys a terminal exists to
 * deliver. Requiring the *other* modifier to be up keeps ⌃⌘-style chords from
 * matching by accident. */
function appMod(ev) {
  return IS_MAC ? ev.metaKey && !ev.ctrlKey : ev.ctrlKey && !ev.metaKey;
}

/** The Windows Terminal default clipboard keys, on Windows/Linux only:
 * Ctrl+V and Shift+Insert paste; Ctrl+Insert copies; Ctrl+C copies *only when
 * `term` has a selection* — with nothing selected it stays SIGINT for the
 * shell. (Ctrl+Shift+C/V are the app shortcuts handled separately.) Returns
 * 'copy', 'paste' or null. Pure on the event + the selection state, so the
 * terminal's key handler and the window handler agree on what is claimed. */
function winClipboardAction(ev, term) {
  if (IS_MAC || ev.altKey || ev.metaKey) return null;
  const key = ev.key.toLowerCase();
  if (ev.ctrlKey && !ev.shiftKey) {
    if (key === 'v') return 'paste';
    if (key === 'insert') return 'copy';
    if (key === 'c') return term && term.hasSelection() ? 'copy' : null;
  } else if (ev.shiftKey && !ev.ctrlKey && key === 'insert') {
    return 'paste';
  }
  return null;
}

window.addEventListener('keydown', (ev) => {
  const mod = appMod(ev) && ev.shiftKey;
  // macOS convention: plain ⌘C/⌘V are copy/paste (no Shift, unlike Ctrl+Shift+C
  // on Windows/Linux where plain Ctrl+C has to stay SIGINT).
  const plainMod = appMod(ev) && !ev.shiftKey && !ev.altKey;
  const key = ev.key.toLowerCase();
  if (paletteKeyIs('open', ev)) {
    // Wins over every branch below: a custom open key that is also another
    // app shortcut (say Ctrl+Shift+T) opens the palette, not the other thing.
    ev.preventDefault();
    palette.open ? closePalette() : openPalette();
  } else if (IS_MAC && plainMod && key === 'c' && !isAppTextEntry(ev.target)) {
    // Only claim ⌘C when there is a terminal selection to copy; otherwise let
    // the webview's own handling (and the native Edit menu) have it.
    if (focusedThread()?.term.getSelection()) {
      ev.preventDefault();
      copyFocusedSelection();
    }
  } else if (IS_MAC && plainMod && key === 'v' && !isAppTextEntry(ev.target)) {
    ev.preventDefault();
    pasteIntoFocused();
  } else if (!IS_MAC && !palette.open && !modalCtx && !shortcutsOverlay.open
             && !isAppTextEntry(ev.target) && ev.target?.closest?.('.term-host')
             && !workflowForShortcut(ev)
             && winClipboardAction(ev, focusedThread()?.term)) {
    // Windows Terminal clipboard keys. A registered workflow shortcut keeps
    // precedence (checked above, same as in the terminal's key handler).
    // preventDefault stops the native paste event from pasting a second time.
    ev.preventDefault();
    if (winClipboardAction(ev, focusedThread()?.term) === 'copy') copyFocusedSelection({ clear: true });
    else pasteIntoFocused();
  } else if (plainMod && key === 'p'
             && !ev.defaultPrevented && !ev.target?.closest?.('.term-host')) {
    // Plain Ctrl+P (⌘P on macOS) outside a terminal would trigger the
    // webview's print dialog — meaningless for a terminal app, and it
    // swallowed the key the user expected to open the palette. Always
    // suppress it here, and open the palette when nothing modal is in the way
    // — unless `palette_keys.open` is customized, in which case that setting
    // is the only way in and this branch only eats the print dialog.
    //
    // Inside a terminal this branch must NOT fire: Ctrl+P is readline's
    // "previous command", and xterm consumes it (sending \x10) and calls
    // preventDefault before the event reaches window — hence both the
    // `.term-host` and `defaultPrevented` guards. The palette's own input
    // likewise handles Ctrl+P (move selection up) and preventDefaults, so
    // navigating the open palette never toggles it shut.
    ev.preventDefault();
    if (!modalCtx && !paletteKeys.custom.open) palette.open ? closePalette() : openPalette();
  } else if (mod && key === 't') {
    ev.preventDefault();
    newLocalThread();
  } else if (mod && key === 'w') {
    ev.preventDefault();
    const t = focusedThread();
    if (t) closeThread(t.id);
  } else if (mod && key === 'd') {
    ev.preventDefault();
    toggleSplitDir('v');
  } else if (mod && key === 'e') {
    ev.preventDefault();
    toggleSplitDir('h');
  } else if (mod && key === 'b') {
    ev.preventDefault();
    toggleBroadcast();
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
  } else if (palette.open && paletteKeyIs('close', ev)) {
    closePalette();
  } else if (ev.key === 'Escape' && termSearch.open) {
    closeTermSearch();
  } else if (!palette.open && !modalCtx && !shortcutsOverlay.open && !isAppTextEntry(ev.target)) {
    // Registered workflow shortcut (runs on the focused terminal).
    const wf = workflowForShortcut(ev);
    if (wf) {
      ev.preventDefault();
      runWorkflow(wf, true);
    }
  }
});

/* ---------------- copy & paste ---------------- */

async function copyFocusedSelection({ clear = false } = {}) {
  const thread = focusedThread();
  const sel = thread?.term.getSelection();
  if (!sel) return;
  // Windows Terminal clears the selection once it is copied (Ctrl+C and
  // right-click), which is also the cue that the copy happened.
  if (clear) thread.term.clearSelection();
  try {
    await navigator.clipboard.writeText(sel);
    toast(tr('toast.copied'));
  } catch (e) {
    toast(tr('toast.copyFailed', { e }), true);
  }
}

async function pasteIntoFocused() {
  const thread = focusedThread();
  if (!thread) return;
  let text;
  try {
    text = await navigator.clipboard.readText();
  } catch (e) {
    toast(tr('toast.pasteFailed', { e }), true);
    return;
  }
  if (!text) return;
  // Multi-line clipboard content can auto-run commands. Confirm first, and
  // route through term.paste() so bracketed-paste (\e[200~) wraps it when the
  // shell/editor requested it — writing straight to the PTY bypassed that.
  if (text.trimEnd().includes('\n')) {
    const lines = text.trimEnd().split('\n').length;
    if (!(await confirmModal(tr('confirm.pasteLines', { lines }), tr('confirm.paste')))) return;
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
  setTermSearchMatch(true); // don't reopen still flagged from the last query
  $('#term-search').classList.add('hidden');
  focusedThread()?.term.focus();
}

/** Flags the search box when the query matched nothing. Without this the bar
 * looks identical whether a term was found or not — the only other signal is
 * a selection somewhere off-screen in the scrollback. */
function setTermSearchMatch(found) {
  const bar = $('#term-search');
  bar.classList.toggle('no-match', !found);
  $('#term-search-input').setAttribute('aria-invalid', String(!found));
  bar.title = found ? '' : tr('termsearch.noMatch');
}

function termFindNext(incremental) {
  const thread = focusedThread();
  const query = $('#term-search-input').value;
  if (!query) return setTermSearchMatch(true); // empty box is not a failed search
  setTermSearchMatch(!!thread?.search.findNext(query, { incremental }));
}

function termFindPrevious() {
  const thread = focusedThread();
  const query = $('#term-search-input').value;
  if (!query) return setTermSearchMatch(true);
  setTermSearchMatch(!!thread?.search.findPrevious(query, { incremental: false }));
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
    el.textContent = tr('block.foldedRows', { rows });
    el.addEventListener('click', () => toggleFold(term, blocks, block));
  });
  block.foldDeco = deco;
  block.collapsed = true;
}

/** Extracts the typed command from a finished block's prompt line by
 * dropping everything up to (and including) the last common prompt
 * terminator ("$ ", "❯ ", "# ", "% ", "> "). Heuristic by design: OSC 133
 * A-D marks where commands run but never carries their text, and this works
 * for local AND SSH sessions alike. A command that itself contains a
 * terminator sequence may be truncated — an accepted trade-off. */
function blockCommandText(term, block) {
  if (block.marker.isDisposed) return '';
  const line = term.buffer.active.getLine(block.marker.line);
  if (!line) return '';
  const text = line.translateToString(true).trimEnd();
  let cut = -1;
  for (const sep of ['$ ', '❯ ', '# ', '% ', '> ']) {
    const i = text.lastIndexOf(sep);
    if (i >= 0 && i + sep.length > cut) cut = i + sep.length;
  }
  return (cut >= 0 ? text.slice(cut) : text).trim();
}

/** Where a thread's commands run, for the history: the saved SSH host's
 * name, or the local profile's name (empty when unknown). */
function threadHostLabel(thread) {
  if (!thread) return '';
  if (thread.kind === 'ssh') {
    return hostById(thread.sshHostId)?.name || thread.title || '';
  }
  return state.profiles.find((p) => p.id === thread.profileId)?.name || '';
}

/** Persists a finished block's command into the cross-session history (and
 * the in-memory mirror the palette searches), with where it ran, its exit
 * status and how long it took. `thread` may be missing for a prompt replayed
 * while the thread was still being built. Fire-and-forget. */
function recordBlockCommand(term, block, thread, kind, exit) {
  const command = blockCommandText(term, block);
  if (!command) return;
  const meta = {
    host: threadHostLabel(thread),
    exit: Number.isNaN(exit) ? null : exit,
    duration_ms: block.startedAt ? Math.max(0, Date.now() - block.startedAt) : 0,
  };
  const prev = state.history.find((h) => h.command === command);
  state.history = [
    { command, kind, at: Math.floor(Date.now() / 1000), ...meta, runs: (prev?.runs || 0) + 1 },
    ...state.history.filter((h) => h.command !== command),
  ];
  invoke('add_history_entry', { command, kind, meta }).catch(() => {});
}

/** Desktop notification for a command that ran at least the configured
 * threshold and finished while the user wasn't looking — the thread is not
 * shown in any pane, or the window itself is unfocused. A command watched in
 * the foreground never notifies. Fire-and-forget: a platform without a
 * notification center just drops it. */
function maybeNotifyBlockDone(threadId, term, block, exit) {
  const s = state.settings;
  if (s.notify_long_commands === false || !block.startedAt) return;
  const elapsedMs = Date.now() - block.startedAt;
  const threshold = Math.max(0, Number(s.notify_threshold_secs) || 0) * 1000;
  if (elapsedMs < threshold) return;
  const visible = state.panes.some((p) => p.threadId === threadId);
  if (visible && document.hasFocus()) return;
  const thread = threadById(threadId);
  const ok = exit === 0;
  const command = blockCommandText(term, block) || tr('notify.command.unknown');
  const title = tr(ok ? 'notify.done.ok' : 'notify.done.err', {
    thread: thread ? thread.title : '',
    exit: Number.isNaN(exit) ? '?' : exit,
  });
  const body = tr('notify.done.body', { command, elapsed: formatElapsed(elapsedMs) });
  invoke('notify', { title, body }).catch(() => {});
}

/** "1m 05s" / "12s" style duration for notifications and the history. */
function formatElapsed(ms) {
  const total = Math.round(ms / 1000);
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const sec = total % 60;
  if (h > 0) return `${h}h ${String(m).padStart(2, '0')}m`;
  if (m > 0) return `${m}m ${String(sec).padStart(2, '0')}s`;
  return `${sec}s`;
}

/** Renders the small always-on-hover toolbar (copy / fold / exit chip)
 * pinned to a finished block's prompt-line right edge. */
function renderBlockToolbar(term, blocks, block, exit) {
  // Wide enough (in cells) for copy + fold + chip side by side — xterm's
  // decoration box defaults to 1 cell, which would force our flex row to
  // wrap. `layer: 'top'` keeps the buttons clickable above the text canvas.
  const deco = term.registerDecoration({ marker: block.marker, width: 13, layer: 'top' });
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
      <button type="button" class="term-block-btn copy" title="${tr('block.copy.title')}">${icon('copy')}</button>
      <button type="button" class="term-block-btn save" title="${tr('block.save.title')}">${icon('zap')}</button>
      <button type="button" class="term-block-btn fold" title="${tr('block.fold.title')}">${icon('chevronDown')}</button>
      <span class="term-block-chip ${ok ? 'ok' : 'err'}" title="${ok ? tr('block.chip.ok') : tr('block.chip.err', { exit })}">${ok ? '✓' : `✗ ${Number.isNaN(exit) ? '?' : exit}`}</span>`;
    el.querySelector('.copy').addEventListener('click', async (ev) => {
      ev.stopPropagation();
      try {
        await navigator.clipboard.writeText(blockText(term, blocks, block));
        toast(tr('block.copied'));
      } catch (e) {
        toast(tr('toast.copyFailed', { e }), true);
      }
    });
    el.querySelector('.save').addEventListener('click', (ev) => {
      ev.stopPropagation();
      saveAsWorkflow(blockCommandText(term, block));
    });
    const foldBtn = el.querySelector('.fold');
    if (!block.bodyMarker) {
      foldBtn.disabled = true;
      foldBtn.title = tr('block.noOutput.title');
    } else {
      foldBtn.addEventListener('click', (ev) => {
        ev.stopPropagation();
        toggleFold(term, blocks, block);
        foldBtn.classList.toggle('active', block.collapsed);
        foldBtn.title = block.collapsed ? tr('block.expand.title') : tr('block.fold.title');
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

// The app's built-in shortcuts, grouped for the help overlay. Built fresh each
// render so it reflects the current language. Workflow shortcuts are
// user-defined (set in the workflow editor) so they're described, not listed.
function shortcutGroups() {
  return [
    { group: tr('sc.group.general'), items: [
      paletteKeys.custom.open
        ? [prettyShortcut(displayCombo(paletteKeyFirst('open'))), tr('sc.item.palette.custom')]
        : ['Mod+Shift+P', tr('sc.item.palette', { mod: IS_MAC ? '⌘' : 'Ctrl' })],
      ['Mod+Shift+T', tr('sc.item.newThread')],
      ['Mod+Shift+W', tr('sc.item.closeThread')],
      ['Mod+Shift+/', tr('sc.item.shortcuts')],
    ] },
    { group: tr('sc.group.pane'), items: [
      ['Mod+Shift+D', tr('sc.item.split')],
      ['Mod+Shift+E', tr('sc.item.splitH')],
      ['Mod+Shift+B', tr('sc.item.broadcast')],
      ['Mod+Shift+↑', tr('sc.item.focusUp')],
      ['Mod+Shift+↓', tr('sc.item.focusDown')],
    ] },
    { group: tr('sc.group.terminal'), items: [
      // macOS uses the plain ⌘ forms it expects; Ctrl+Shift there would be
      // taken from the shell.
      // Windows/Linux follow Windows Terminal: plain Ctrl+V pastes and Ctrl+C
      // copies when something is selected (else it stays SIGINT).
      ...(IS_MAC ? [
        ['Mod+C', tr('sc.item.copy')],
        ['Mod+V', tr('sc.item.paste')],
      ] : [
        ['Ctrl+C', tr('sc.item.copyWin')],
        ['Ctrl+Shift+C', tr('sc.item.copy')],
        ['Ctrl+Insert', tr('sc.item.copyAlt')],
        ['Ctrl+V', tr('sc.item.paste')],
        ['Ctrl+Shift+V', tr('sc.item.pasteAlt')],
        ['Shift+Insert', tr('sc.item.pasteAlt')],
      ]),
      ['Mod+Shift+F', tr('sc.item.find')],
      ['Ctrl+Alt+↑', tr('sc.item.prevBlock')],
      ['Ctrl+Alt+↓', tr('sc.item.nextBlock')],
    ] },
    { group: tr('sc.group.workflows'), items: [
      [tr('sc.key.anyKey'), tr('sc.item.wfShortcut')],
      [tr('sc.key.rightClick'), rightClickMode() === 'copy_paste' ? tr('sc.item.rightClickCopyPaste') : tr('sc.item.rightClick')],
    ] },
  ];
}

/** Renders a "Mod+Shift+P" style combo as separate <kbd> chips, using mac
 * glyphs on macOS. Non-combo strings (e.g. 任意のキー) become a single chip.
 *
 * `Mod` is the app modifier (⌘ on macOS, Ctrl elsewhere) and `Ctrl` is the
 * literal control key — they are different keys on macOS, so a shortcut that
 * really uses Ctrl there (block jump) must not be relabelled ⌘. */
function kbdCombo(combo) {
  const map = IS_MAC
    ? { Mod: '⌘', Ctrl: '⌃', Shift: '⇧', Alt: '⌥', Meta: '⌘' }
    : { Mod: 'Ctrl', Ctrl: 'Ctrl', Shift: 'Shift', Alt: 'Alt', Meta: 'Meta' };
  return combo.split('+')
    .map((t) => `<kbd>${map[t] || t}</kbd>`)
    .join('<span class="sc-plus">+</span>');
}

function renderShortcuts() {
  const body = $('#sc-body');
  body.innerHTML = '';
  for (const g of shortcutGroups()) {
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

function onSessionData(id, bytes) {
  const thread = threadById(id);
  if (!thread) {
    // Output for a session whose thread hasn't been built yet — hold it.
    const e = preCreateEntry(id);
    if (e.size + bytes.length <= PRE_CREATE_LIMIT) {
      e.chunks.push(bytes);
      e.size += bytes.length;
    }
    return;
  }
  thread.term.write(bytes);
  // Flag activity only for threads not currently shown in any pane; avoid a
  // re-render on every single chunk once the flag is already set.
  if (!thread.activity && !state.panes.some((p) => p.threadId === id)) {
    thread.activity = true;
    renderThreads();
  }
}

function onSessionExit(id, code) {
  const thread = threadById(id);
  if (!thread) {
    // The session died before its thread was built (a shell that fails to
    // start, say). Remember it so `createThread` can show the output and then
    // close, rather than leaving a row for a process that is already gone.
    preCreateEntry(id).exit = code;
    return;
  }
  handleSessionExit(thread, code);
}

/* Terminal output arrives over one binary channel — see TauriSink in
 * src-tauri/src/lib.rs for the frame layout. Tauri hands anything past 1KB to
 * the webview as a real ArrayBuffer, so a 64KB batch crosses as 64KB of bytes
 * instead of 87KB of base64 that this thread would then decode a byte at a
 * time. Data and exit share the channel so an exit can never overtake the
 * output that preceded it. */
const FRAME_DATA = 0;
const FRAME_EXIT = 1;
const frameDecoder = new TextDecoder();

const outputChannel = new Channel();
outputChannel.onmessage = (msg) => {
  const buf = msg instanceof ArrayBuffer ? msg : (ArrayBuffer.isView(msg) ? msg.buffer : null);
  if (!buf || buf.byteLength < 2) return;
  const view = new Uint8Array(buf);
  const idLen = view[1];
  const body = 2 + idLen;
  if (view.length < body) return;
  const id = frameDecoder.decode(view.subarray(2, body));
  if (view[0] === FRAME_DATA) onSessionData(id, view.subarray(body));
  else if (view[0] === FRAME_EXIT && view.length >= body + 4) {
    onSessionExit(id, new DataView(buf, view.byteOffset + body, 4).getInt32(0, true));
  }
};
// Registered before boot() so a restored session's first bytes already have
// somewhere to go; the events below cover the gap until this lands.
invoke('subscribe_output', { channel: outputChannel }).catch(() => {});

/* Fallback transport: what the backend emits until the channel above is
 * registered. Same handlers, one base64 decode in front. */
listen('session:data', (ev) => onSessionData(ev.payload.id, b64ToBytes(ev.payload.data)));

/** Announces an exit and tears the thread down. Shared by the live event and
 * the replay of an exit that landed before the thread existed. */
function handleSessionExit(thread, code) {
  // A vanishing SSH thread can look like data loss, so surface it — a dropped
  // connection (code -1 from the backend) is flagged as an error.
  if (thread.kind === 'ssh') {
    // A drop (not a clean exit) on a host that asked for it: keep the thread
    // and bring the session back under it.
    if (code === -1 && hostById(thread.sshHostId)?.auto_reconnect) {
      scheduleReconnect(thread);
      return;
    }
    if (code === -1) toast(tr('thread.disconnected', { title: thread.title }), true);
    else toast(tr('thread.exited', { title: thread.title, code }));
  }
  closeThread(thread.id, { kill: false });
}

/* ---------------- SSH auto-reconnect ---------------- */

// Seconds before each retry. Short first, so a blip costs little; capped,
// so a server that is really down is not hammered.
const RECONNECT_BACKOFF = [2, 4, 8, 16, 30];

function reconnectBanner(thread, text, color = '33') {
  thread.term.writeln(`\r\n\x1b[${color}m${text}\x1b[0m`);
}

/** Starts the retry cycle for a thread whose session just dropped. The
 * terminal stays as it is — scrollback, pane, color — and gets the new
 * session when one comes up. Closing the thread cancels. */
function scheduleReconnect(thread) {
  if (thread.reconnect) return;
  const r = { attempt: 0, timer: 0, secrets: {}, prompted: false };
  thread.reconnect = r;
  const sec = RECONNECT_BACKOFF[0];
  reconnectBanner(thread, tr('reconnect.start', { sec, n: 1, max: RECONNECT_BACKOFF.length }));
  renderThreads();
  r.timer = setTimeout(() => attemptReconnect(thread), sec * 1000);
}

function reconnectGiveUp(thread, reason) {
  reconnectBanner(thread, tr('reconnect.gaveUp', { reason }), '31');
  toast(tr('toast.reconnectGaveUp', { title: thread.title, reason }), true);
  thread.reconnect = null;
  closeThread(thread.id, { kill: false });
}

async function attemptReconnect(thread) {
  const r = thread.reconnect;
  if (!r || !threadById(thread.id)) return; // closed meanwhile
  const host = hostById(thread.sshHostId);
  if (!host) { reconnectGiveUp(thread, tr('reconnect.reason.hostGone')); return; }
  const chain = resolveChainHosts(host);
  if (chain.error) { reconnectGiveUp(thread, chain.error); return; }
  try {
    // Empty secrets: the backend fills in whatever the hops have saved. No
    // approved fingerprints: the keys were verified when this thread first
    // connected, so anything but a match is a reason to stop, not to trust.
    const info = await invoke('create_ssh_session', {
      hostId: host.id, secrets: r.secrets, fingerprints: {},
      cols: thread.term.cols, rows: thread.term.rows,
    });
    const oldId = thread.id;
    thread.rebind(info.id);
    for (const p of state.panes) if (p.threadId === oldId) p.threadId = info.id;
    thread.logPath = info.log_path || '';
    if (info.log_error) toast(tr('toast.logFailed', { e: info.log_error }), true);
    // Output that arrived before the rebind. If the new session already died,
    // that is one more failed attempt, not a fresh cycle.
    const { exited } = drainPreCreate(info.id, thread.term);
    if (exited !== null) throw new Error(tr('thread.disconnected', { title: thread.title }));
    thread.reconnect = null;
    reconnectBanner(thread, tr('reconnect.ok'), '32');
    toast(tr('toast.reconnected', { title: thread.title }));
    renderThreads();
    persistSessionSnapshot();
  } catch (e) {
    const msg = String(e?.message || e);
    if (parseHostKeyError(msg)) { reconnectGiveUp(thread, tr('reconnect.reason.hostKey')); return; }
    // Authentication failed and nothing saved could be used: ask once. A
    // cancel here is the user saying stop.
    if (!r.prompted && /auth/i.test(msg)) {
      r.prompted = true;
      const secrets = await promptChainSecrets(chain.hosts, {});
      if (secrets === null) { reconnectGiveUp(thread, tr('reconnect.reason.cancelled')); return; }
      r.secrets = secrets;
      attemptReconnect(thread);
      return;
    }
    r.attempt++;
    if (r.attempt >= RECONNECT_BACKOFF.length) {
      reconnectGiveUp(thread, tr('reconnect.reason.max', { max: RECONNECT_BACKOFF.length }));
      return;
    }
    const sec = RECONNECT_BACKOFF[r.attempt];
    reconnectBanner(thread, tr('reconnect.retry', { msg: msg.slice(0, 80), sec, n: r.attempt + 1, max: RECONNECT_BACKOFF.length }));
    renderThreads();
    r.timer = setTimeout(() => attemptReconnect(thread), sec * 1000);
  }
}

listen('session:exit', (ev) => onSessionExit(ev.payload.id, ev.payload.code));

(async function boot() {
  // Paint every static [data-icon] placeholder in the chrome first.
  initIcons();
  // The refresh/loadSettings helpers each swallow their own backend errors
  // (keeping defaults), so one failing store can't leave a blank window.
  await loadSettings();
  // Needed before the settings panel renders and before the startup check, so
  // both know whether this build is allowed to update itself.
  await loadDistInfo();
  applyAutoUpdateAvailability();
  // Profiles must load before the first thread so the default profile applies.
  await Promise.all([refreshWorkflows(), refreshHosts(), refreshProfiles(), refreshLaunchSets(),
    refreshHistory(), refreshServers()]);
  const pane = createPane();
  $('#pane-area').appendChild(pane.root);
  state.panes.push(pane);
  focusPane(0);
  updateSplitUi();
  setView('shell');
  refreshMaxIcon();
  // Restore the previous session's local threads (SSH needs credentials, so
  // those are only counted and reported). Falls back to one default thread.
  let restored = false;
  if (state.settings.restore_session !== false) {
    try {
      const snap = await invoke('get_session_snapshot');
      const items = snap?.threads || [];
      const locals = items.filter((t) => !t.ssh_host_id);
      if (locals.length) {
        restoringSession = true;
        try {
          for (const item of locals) {
            await newLocalThread({ profileId: item.profile_id || null });
          }
        } finally {
          restoringSession = false;
        }
        persistSessionSnapshot();
        // Show the first restored thread, not the last one created.
        if (state.threads.length) assignThread(0, state.threads[0].id);
        const sshCount = items.length - locals.length;
        if (sshCount > 0) toast(tr('restore.sshSkipped', { count: sshCount }));
        restored = state.threads.length > 0;
      }
    } catch (_e) {
      restoringSession = false;
    }
  }
  if (!restored) await newLocalThread();
  // Silent startup update check, delayed so it never competes with the first
  // paint or session restore. No-ops when the updater isn't usable.
  if (state.settings.auto_update_check !== false) {
    setTimeout(() => checkForUpdates(false), 3000);
  }
})().catch((e) => toast(tr('boot.error', { e }), true));
