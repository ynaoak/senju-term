# Senju Term

[Warp](https://github.com/warpdotdev/warp) にインスパイアされた、**カスタムコマンド(ワークフロー)管理**と **SSH 接続管理**を組み込んだクロスプラットフォームターミナルです。

- **実装**: Rust(コア/バックエンド)+ Tauri 2 + xterm.js
- ローカルシェルは [portable-pty](https://crates.io/crates/portable-pty)、SSH は Pure Rust の [russh](https://crates.io/crates/russh)

このリポジトリは**配布用**です。ビルド済みインストーラは [Releases](https://github.com/ynaoak/senju-term/releases/latest) からダウンロードできます。

## ダウンロード

| ファイル | 用途 |
| --- | --- |
| `*-setup.exe` | Windows インストーラ(推奨・自動アップデート対応) |
| `*.msi` | Windows MSI インストーラ |
| `*portable.zip` | Windows ポータブル版(インストール・自動アップデートなし) |
| `*.AppImage` | Linux(自動アップデート対応) |
| `*.deb` | Debian / Ubuntu パッケージ |
| `*.rpm` | Fedora / RHEL パッケージ |

macOS 向けバイナリは現在準備中です。それまでは下記のソースビルドをご利用ください(Tauri 2 のクロスプラットフォーム対応により macOS でもビルドできます)。

## ソースからビルド

前提: [Rust(stable)](https://rustup.rs/) と [tauri-cli v2](https://v2.tauri.app/)(`cargo install tauri-cli --locked`)

```sh
cd apps/desktop-app
cargo tauri dev     # 開発起動
cargo tauri build   # インストーラ生成
```

詳細は [`apps/desktop-app/README.md`](apps/desktop-app/README.md) を参照してください。

## ライセンス

[MIT](LICENSE)
