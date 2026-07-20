# Web LP を Cloudflare Pages にデプロイする

[`apps/web-lp/`](../apps/web-lp/) の紹介用ランディングページを [Cloudflare Pages](https://developers.cloudflare.com/pages/) に公開する手順です。

LP は **ビルドステップのない素の HTML / CSS**(外部 CDN・スクリプト依存なし、アセットは全て相対パス)なので、Cloudflare 側でのビルドは不要です。`apps/web-lp/` をそのまま配信すれば動きます。

## 前提

- 対象ディレクトリ: `apps/web-lp/`(モノレポのサブディレクトリ)
- ビルド: **不要**(コンパイル・バンドル・パッケージインストールなし)
- 配信ファイル: `index.html` / `styles.css` / `assets/*.png`

## 方法 A: Git 連携でデプロイ(推奨)

`main` への push で自動デプロイされます。

1. Cloudflare ダッシュボード → **Workers & Pages** → **Create** → **Pages** → **Connect to Git**
2. リポジトリ `ynaoak/senju-term` を選択
3. **ビルド設定**を次のように入力する:

   | 項目 | 値 |
   | --- | --- |
   | Framework preset(フレームワークプリセット) | `None` |
   | Build command(ビルドコマンド) | (空欄) |
   | Build output directory(ビルド出力ディレクトリ) | `apps/web-lp` |
   | Root directory(ルートディレクトリ / Advanced) | (空欄 = リポジトリのルート) |

4. **Save and Deploy** を押す

> **補足(ルートディレクトリを使う場合)**: 上記の代わりに、Advanced の **Root directory** を `apps/web-lp` に設定し、**Build output directory** を `/`(または空欄)にしても同じ結果になります。どちらか一方の指定で構いません。

### ブランチと自動デプロイ

- **Production branch**: `main`(`main` への push で本番デプロイ)
- それ以外のブランチへの push では **プレビューデプロイ**が作られます(PR ごとの確認用 URL)

## 方法 B: Wrangler で直接アップロード

CI や手元から Git 連携なしでデプロイする場合:

```sh
# 初回のみ: プロジェクト作成(1 度だけ)
npx wrangler pages project create senju-term-lp --production-branch main

# デプロイ(apps/web-lp の中身をそのままアップロード)
npx wrangler pages deploy apps/web-lp --project-name senju-term-lp
```

- `wrangler` は `npx` 経由で実行できます(グローバルインストール不要)。
- 認証は `npx wrangler login`、または CI では `CLOUDFLARE_API_TOKEN` / `CLOUDFLARE_ACCOUNT_ID` 環境変数で行います。

## 設定に関する注意

- **環境変数 / Node バージョン**: どちらも不要です(ビルドしないため)。Pages のビルド環境変数を設定する必要はありません。
- **リダイレクト / ルーティング**: SPA ではなく単一の `index.html` なので `_redirects` や `_routes.json` は不要です。
- **アセットのパス**: `index.html` は `./styles.css` / `./assets/…` の**相対パス**のみを参照しているため、どのルートに配置しても動作します。
- **404 ページ**: 必要なら `apps/web-lp/404.html` を追加すると Cloudflare Pages が自動で使用します(現状は未設置)。

## デプロイ後の確認

1. 発行された `*.pages.dev` の URL を開く
2. ヒーロー / 機能 / ワークフロー / SSH の各スクリーンショットが表示されること
3. アンカーリンク(機能・ワークフロー・SSH・ダウンロード)がページ内スクロールで動くこと

## 独自ドメイン(任意)

Pages プロジェクトの **Custom domains** タブからドメインを追加し、表示される DNS レコード(CNAME)を設定すれば独自ドメインで公開できます。
