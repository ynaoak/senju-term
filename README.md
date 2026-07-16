# Senju Term

[Warp](https://github.com/warpdotdev/warp) にインスパイアされた、**カスタムコマンド(ワークフロー)管理**と **SSH 接続管理**を組み込んだクロスプラットフォームターミナルと、その紹介用ウェブサイトのモノレポです。

## リポジトリ構成

| ディレクトリ | 内容 |
| --- | --- |
| [`desktop-app/`](desktop-app/) | デスクトップアプリ本体(Rust + Tauri 2 / xterm.js)。Cargo ワークスペース(`crates/senju-core` + `src-tauri`)とフロントエンド(`ui/`)。 |
| [`web-lp/`](web-lp/) | 紹介用ランディングページ(素の HTML / CSS、ビルドステップなし)。 |

各ディレクトリの詳細はそれぞれの `README.md` を参照してください。

## デスクトップアプリ

- **実装言語**: Rust(バックエンド/コア)+ HTML/JS(描画は xterm.js)
- **対応 OS**: Windows / macOS / Linux(Tauri 2)
- ローカルシェルは [portable-pty](https://crates.io/crates/portable-pty)、SSH は Pure Rust の [russh](https://crates.io/crates/russh)

```sh
cd desktop-app
cargo tauri dev      # 開発起動
cargo test -p senju-core   # コアのテスト
```

詳細な機能一覧・キーボードショートカット・開発手順は [`desktop-app/README.md`](desktop-app/README.md) にあります。

## Web LP

```sh
cd web-lp
python3 -m http.server 8000   # → http://localhost:8000
```

詳細は [`web-lp/README.md`](web-lp/README.md) を参照してください。

## CI / 配布

`.github/workflows/build.yml` が、デスクトップアプリのコアテスト(Ubuntu)と、3 プラットフォーム(Ubuntu / Windows / macOS Universal)向けのビルドを実行します。`v*` タグを push すると各インストーラがドラフトリリースに添付されます。

## ライセンス

MIT License
