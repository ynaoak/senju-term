# Senju Term

[Warp](https://github.com/warpdotdev/warp) にインスパイアされた、**カスタムコマンド(ワークフロー)管理**と **SSH 接続管理**を組み込んだクロスプラットフォームターミナルです。

- **実装言語**: Rust(バックエンド/コア)+ HTML/JS(描画は xterm.js)
- **対応 OS**: Windows / macOS / Linux(Tauri 2)
- ローカルシェルは [portable-pty](https://crates.io/crates/portable-pty)(Windows は ConPTY、Unix は openpty)
- SSH は [russh](https://crates.io/crates/russh)(Pure Rust。OpenSSL / libssh2 への依存なし)

## 主な機能

### ターミナル / スレッド管理
- セッションを「スレッド」として左ペインで一覧管理(ローカルシェル / SSH が同居)。クリックで表示切替、非表示スレッドもバックグラウンドで動作し続けます
- **上下ペイン分割**(`Ctrl+Shift+D`): 各ペインのヘッダーにあるドロップダウン、または左ペインのスレッド一覧から、どのスレッドを表示するか個別に選択可能。同じスレッドを両ペインが取り合った場合は自動でスワップ。境界はドラッグでリサイズ
- xterm.js による 256 色 / True Color 描画、10,000 行スクロールバック
- フォントサイズ・使用シェルの設定(JSON ファイルとして永続化)

### カスタムコマンド管理(Warp の Workflows 相当)
- コマンドテンプレートを名前・説明・タグ付きで登録/編集/削除/検索
- `{{名前}}` / `{{名前:既定値}}` 形式のプレースホルダに対応。実行時に入力ダイアログで値を埋めます
- 「実行」(Enter 送信つき)と「挿入」(入力欄に置くだけ)の 2 モード
- 初回起動時にサンプルワークフローを登録

### SSH 接続管理
- ホスト(名前 / ホスト / ポート / ユーザー / 認証方式)を登録・管理
- 認証方式: パスワード / 秘密鍵ファイル(パスフレーズ対応)/ ssh-agent
- **パスワード・パスフレーズは保存されません**(接続時に入力し、その接続にのみ使用)
- 接続はタブとして開き、リサイズ・keepalive(30 秒)に対応

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

- v1 では SSH ホストキーの検証を行いません(TOFU ですらなく常に受理)。known_hosts 検証はロードマップにあります。信頼できるネットワークでのみ使用してください
- パスワード・パスフレーズは永続化されませんが、OS のキーチェーン統合は未実装です
- Warp のようなコマンド単位のブロック UI・AI 機能は未実装です(ロードマップ)

## テスト

```bash
cargo test -p senju-core   # プレースホルダ展開・ストア・PTY セッションの実挙動テスト
```
