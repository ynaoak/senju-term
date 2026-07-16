# Senju Term

[Warp](https://github.com/warpdotdev/warp) にインスパイアされた、**カスタムコマンド(ワークフロー)管理**と **SSH 接続管理**を組み込んだクロスプラットフォームターミナルです。

- **実装言語**: Rust(バックエンド/コア)+ HTML/JS(描画は xterm.js)
- **対応 OS**: Windows / macOS / Linux(Tauri 2)
- ローカルシェルは [portable-pty](https://crates.io/crates/portable-pty)(Windows は ConPTY、Unix は openpty)
- SSH は [russh](https://crates.io/crates/russh)(Pure Rust。OpenSSL / libssh2 への依存なし)

## 主な機能

### ネイティブライクなカスタムタイトルバー
- OS 標準のウィンドウ枠(装飾)を無効化し、アプリ独自のタイトルバーに統合。バー全体をドラッグでウィンドウ移動、ダブルクリックで最大化/復元
- ウィンドウ操作ボタンは OS に合わせて配置: **Windows / Linux は右上に最小化・最大化・閉じる**、**macOS は左上にトラフィックライト風の丸ボタン**

### ターミナル / スレッド管理
- セッションを「スレッド」として左ペインで一覧管理(ローカルシェル / SSH が同居)。クリックで表示切替、非表示スレッドもバックグラウンドで動作し続けます
- **動的タイトル**: シェルが OSC タイトル(`\e]0;...\a`)を送ると、スレッド名が自動で追従します
- **スレッド名のリネーム**: スレッド項目をダブルクリックでインライン編集(Enter で確定 / Escape でキャンセル)。空欄で確定すると自動タイトルに戻ります。リネームすると自動タイトル追従は止まります
- **スレッドの固定(ピン留め)**: 📍 ボタンでスレッドを左ペイン上部に固定。固定中のスレッドは一覧の先頭に並び、誤って閉じないよう終了時に確認します
- **非表示スレッドのアクティビティ表示**: 表示していないスレッドに出力があると、一覧のタイトル横に teal の●が点灯します
- **上下ペイン分割**(`Ctrl+Shift+D`): 各ペインのヘッダーにあるドロップダウン、または左ペインのスレッド一覧から、どのスレッドを表示するか個別に選択可能。同じスレッドを両ペインが取り合った場合は自動でスワップ。境界はドラッグでリサイズ
- **コピー & 貼り付け**(`Ctrl+Shift+C` / `Ctrl+Shift+V`): 選択範囲をシステムクリップボードへコピー、クリップボードの内容をアクティブなスレッドへ貼り付け
- **ターミナル内検索**(`Ctrl+Shift+F`): xterm.js の addon-search でスクロールバックを含めて文字列検索。前へ / 次へ移動可能
- xterm.js による 256 色 / True Color 描画。フォントファミリー・スクロールバック行数(既定 10,000 行)は設定画面から変更可能

### ターミナルプロファイル(Windows Terminal 相当)
- 起動するシェルを「プロファイル」として登録・管理(名前 / 実行ファイル / 引数 / 作業ディレクトリ)
- 「＋ 新規」の **▾ メニューから起動するプロファイル、または登録済みの SSH 接続先を選択**(Windows Terminal のドロップダウン相当)。初回起動時に OS に応じた既定プロファイルを自動生成(Windows: PowerShell / コマンドプロンプト、Unix: bash / zsh / fish のうち存在するもの)
- **既定のプロファイルを設定可能**。「＋ 新規」ボタンやショートカットはこの既定プロファイルで起動します

### カスタムコマンド管理(Warp の Workflows 相当)
- コマンドテンプレートを名前・説明・タグ付きで登録/編集/削除/検索
- `{{名前}}` / `{{名前:既定値}}` 形式のプレースホルダに対応。実行時に入力ダイアログで値を埋めます
- 「実行」(Enter 送信つき)と「挿入」(入力欄に置くだけ)の 2 モード
- **キーボードショートカット登録**(Ctrl / Alt / Meta を含む任意のキー)で、シェル表示中にワンキー実行
- **クイックボタン表示**: 各ワークフローを「シェル表示のクイック起動バー」にボタンとして常時表示できます
- 初回起動時にサンプルワークフローを登録

### SSH 接続管理
- ホスト(名前 / ホスト / ポート / ユーザー / 認証方式)を登録・管理
- 認証方式: パスワード / 秘密鍵ファイル(パスフレーズ対応)/ ssh-agent / **秘密鍵 + パスワード併用**(`AuthenticationMethods publickey,password` のような多段認証。公開鍵で認証後、サーバが要求すればパスワードで認証を完了)
- **保存前に「接続テスト」**: ホスト編集画面から、到達性・認証・ホスト鍵(SHA256 フィンガープリント)を保存前に確認できます。テスト用パスワード / パスフレーズは保存されず、テストではホスト鍵を `known_hosts` に記録しません
- **パスワード・パスフレーズは保存されません**(接続時に入力し、その接続にのみ使用)
- 接続はタブとして開き、リサイズ・keepalive(30 秒)に対応
- **OpenSSH 互換の known_hosts 検証(TOFU)**: `~/.ssh/known_hosts` と照合し、記録された鍵と異なる鍵を検知した場合は MITM の可能性があるとして常に接続を拒否します。未知のホストへの初回接続時は、鍵種別と SHA256 フィンガープリントを表示する確認ダイアログが出て、承認すると鍵を `known_hosts` に保存した上で接続します

### ウィンドウサイズ・位置の記憶
- 直前の終了時点のウィンドウサイズ・位置(最大化状態を含む)を記憶し、次回起動時に復元します([tauri-plugin-window-state](https://github.com/tauri-apps/plugins-workspace/tree/v2/plugins/window-state) を使用)

### コマンドパレット
- `Ctrl+Shift+P` で起動。ワークフロー・SSH ホスト・アクションを横断ファジー検索
- `Enter` で実行、`Shift+Enter` でワークフローを挿入のみ

## キーボードショートカット

| キー | 動作 |
| --- | --- |
| `Ctrl+Shift+P` | コマンドパレット |
| `Ctrl+Shift+T` | 新しいローカルスレッド |
| `Ctrl+Shift+W` | フォーカス中のスレッドを終了 |
| `Ctrl+Shift+D` | ペインの上下分割 / 分割解除 |
| `Ctrl+Shift+C` / `V` | 選択範囲をコピー / クリップボードから貼り付け |
| `Ctrl+Shift+F` | ターミナル内検索 |
| `Ctrl+Shift+↑` / `↓` | 上 / 下のペインへフォーカス移動 |

(シェル側の `Ctrl+K` などのキーバインドを潰さないよう、アプリのショートカットはすべて `Ctrl+Shift` 系です)

## ビルド方法

前提: [Rust](https://rustup.rs/) と、Linux の場合は Tauri のシステム依存パッケージ。

```bash
# Linux (Debian/Ubuntu) のみ必要
sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev

# 開発実行
cargo install tauri-cli --version "^2"
cargo tauri dev

# 配布用ビルド (Windows: .msi/.exe, macOS: .dmg/.app, Linux: .deb/.rpm/.AppImage)
cargo tauri build
```

フロントエンドはバンドラ不要の静的ファイル(`ui/`)で、xterm.js は `ui/vendor/` にベンダリング済みです。npm は不要です。

GitHub Actions(`.github/workflows/build.yml`)で 3 OS のビルドを自動実行します。`v*` タグを push するとドラフトリリースに各 OS のインストーラが添付されます。

## アーキテクチャ

```
crates/senju-core/   GUI 非依存のコア (cargo test でテスト可能)
  ├─ models.rs       Workflow / SshHost / Settings
  ├─ template.rs     {{placeholder}} の抽出・展開
  ├─ store.rs        JSON ファイル永続化 (アトミック書き込み)
  └─ sessions/       セッション管理
      ├─ local.rs    portable-pty によるローカルシェル
      └─ ssh.rs      russh による SSH シェル
src-tauri/           Tauri アプリ (コマンド定義・イベント転送)
ui/                  フロントエンド (xterm.js + vanilla JS)
```

- バックエンド → フロントエンドの出力は base64 で `session:data` イベントとして転送(マルチバイト文字が読み取り境界で分割されても壊れないようバイト列のまま渡し、xterm.js 側でデコード)
- 設定・ワークフロー・ホストはアプリ設定ディレクトリ配下の JSON(`workflows.json` / `ssh-hosts.json` / `settings.json`)

## セキュリティ上の注意 / 既知の制限

- SSH ホストキーは OpenSSH 互換の `~/.ssh/known_hosts` で検証します(TOFU)。記録済みの鍵と異なる鍵を提示するホストへは、たとえ「信頼」操作をしていても常に接続を拒否します(MITM 対策)
- パスワード・パスフレーズは永続化されませんが、OS のキーチェーン統合は未実装です
- Warp のようなコマンド単位のブロック UI・AI 機能は未実装です(ロードマップ)

## テスト

```bash
cargo test -p senju-core   # プレースホルダ展開・ストア・PTY セッションの実挙動テスト
```
