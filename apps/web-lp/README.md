# Senju Term — Web LP

Senju Term(デスクトップアプリ)の紹介用ランディングページです。

- **ビルドステップなし**: 素の HTML / CSS のみ。プロジェクト方針(フロントエンドはバンドラなし)に合わせています。
- **配色**: デスクトップアプリと同じダークティールのパレット(`--bg #0d1117` / `--accent #00e5be`)を踏襲。

## 構成

| ファイル | 役割 |
| --- | --- |
| `index.html` | ランディングページ本体(ヒーロー / 機能 / ワークフロー / SSH / ダウンロード) |
| `styles.css` | スタイル(色トークン・レイアウト・レスポンシブ) |
| `assets/screenshot-*.png` | 実機のスクリーンショット(`desktop-app/scripts/lp_shots.mjs` で生成、下記参照) |

## スクリーンショットの再生成

`assets/` 配下の画像は手描きのモックではなく、`desktop-app/ui` を `__TAURI__` スタブ付きのヘッドレス Chromium で実際に起動して撮影した本物の画面です。デスクトップアプリの UI を変更した際は、見た目のズレを防ぐため再生成してください:

```sh
cd apps/desktop-app
node scripts/lp_shots.mjs /tmp/lp-shots   # フル画面のスクリーンショットを出力
cd /tmp/lp-shots
for f in hero workflows hosts; do convert $f.png -trim +repage ${f}.png; done
cp hero.png      ../../web-lp/assets/screenshot-hero.png
cp workflows.png ../../web-lp/assets/screenshot-workflows.png
cp hosts.png     ../../web-lp/assets/screenshot-ssh.png
```

`convert -trim`([ImageMagick](https://imagemagick.org/))は、単色背景の余白を自動でトリミングします(アプリのキャンバス背景が単色なため、内容の外側だけが除去されます)。

## ローカルで確認

任意の静的サーバで開けます。例:

```sh
cd apps/web-lp
python3 -m http.server 8000
# → http://localhost:8000
```

外部依存(CDN・フォント・スクリプト)は読み込んでいないため、ファイルを直接ブラウザで開いても表示できます。

## デプロイ

`web-lp/` 配下の静的ファイルをそのまま任意のホスティング(GitHub Pages / Cloudflare Pages / Vercel / Netlify など)へ配置するだけで公開できます。ビルドコマンドは不要です。
