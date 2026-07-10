# Senju Term 全体レビュー改善チェックリスト

リポジトリ全体を機能単位(デスクトップシェル / フロントエンド / コア crate)でレビューした結果の改善項目。
影響度(high/med/low)× 労力(S/M/L)で優先度付け。順番に消化する。

## セキュリティ(最優先)

- [x] **SEC1** 接続テストが未検証ホストへパスワードを送信する (`ssh.rs` TestClient) — high/M ✅ Batch A
      → TestClient を Client と同じフェイルクローズ TOFU に。未知の鍵は認証前に UnknownHostKey を返し、UI は承認後に再テスト。
- [ ] **SEC2** CSP 未設定 (`tauri.conf.json` `csp: null`) — high/S
      → 厳格な CSP を設定(全アセットはローカル)。
- [x] **SEC3** TestClient が known_hosts 読取失敗時にフェイルオープン (`ssh.rs`) — med/S ✅ Batch A
      → VerifyError で Ok(false)、認証に進まない。
- [ ] **SEC4** WebLinksAddon 既定ハンドラが端末出力の URL を webview で開く (`app.js:183`) — high/S
      → http(s) のみ OS ブラウザで開く明示ハンドラに。
- [ ] **SEC5** ペーストが xterm を迂回し bracketed paste を無効化 (`app.js` paste) — med/S
      → `term.paste(text)` 経由に。改行含む場合は確認。
- [ ] **SEC6** パスワードがモーダル DOM に残留 (`app.js` closeModal) — med/S
      → 閉じる時に modal-body をクリア、password 欄に autocomplete=off。
- [ ] **SEC7** capability が `core:default` 全付与 (`capabilities/default.json`) — med/S
      → 実際に使う `core:event:default` + 明示 window 権限に絞る。
- [x] **SEC8** (部分) SshSecrets の Debug を redact 化(ログ流出防止) ✅ Batch A ／ zeroize 本体は今後
      → Zeroizing / Drop で消去、Debug を redact。

## コード品質・堅牢性(高影響)

- [x] **Q1** SSH connect/auth にタイムアウトなし (`ssh.rs`) ✅ Batch A → 20秒 timeout を connect+auth に
      → `tokio::time::timeout` で connect+auth+channel をラップ。
- [ ] **Q2** ブロッキング PTY write をセッションマップのロック保持中に実行 (`sessions/mod.rs`, `local.rs`) — high/M
      → ロック解放後に書く / 専用ライタスレッド。
- [ ] **Q3** store の破損 JSON を握り潰し次回保存で上書き消失 (`store.rs`) — high/S
      → NotFound と parse error を区別、破損ファイル退避、警告。
- [ ] **Q4** invoke() のエラー処理欠如で無言失敗(get_settings 失敗で起動不能) (`app.js`) — high/M
      → try/catch → toast、boot をデフォルトで継続。
- [ ] **Q5** セッション挿入/削除レース (`sessions/mod.rs`) — med/M
      → 挿入をリーダ spawn より前に。
- [ ] **Q6** `lock().unwrap()` の poison 連鎖で exit イベント消失 (`local.rs`,`ssh.rs`,`mod.rs`,`store.rs`) — med/S
      → `unwrap_or_else(PoisonError::into_inner)`。
- [ ] **Q7** 端末データ転送がチャンク毎 base64+JSON+broadcast (`lib.rs` TauriSink) — high/M
      → コアレッシング(~8-16ms/32-64KB でバッチ)。
- [ ] **Q8** 非同期作成後にペインインデックスが陳腐化するレース (`app.js`) — med/M
      → ペインオブジェクトを捕捉し attach 時に index 解決。
- [ ] **Q9** 既定プロファイル削除後に default_profile_id が陳腐化 (`app.js`) — med/S
      → 削除時にリセット+保存、isDefaultProfile を先頭フォールバック。

## UI/UX

- [ ] **U1** 確認モーダルで Escape が効かず Enter が背後ボタンを再発火 (`app.js` openModal) — high/M
      → フォーカス管理・window レベルの Esc/Enter・フォーカストラップ。
- [ ] **U2** Cancel フォーカス中の Enter が保存してしまう (`app.js`) — med/S
- [ ] **U3** 確認ボタンが常に「削除」(スレッド終了でも) (`app.js` confirmModal) — med/S
      → okLabel 引数化。
- [ ] **U4** exit でスレッドが無言消滅、SSH 切断がデータ消失に見える (`app.js`) — high/M
      → exited 状態を残す or トースト。
- [ ] **U5** ワークフローショートカットが端末を壊す組合せ/重複を許容 (`app.js`) — med/M
      → 予約・shell 必須キー・重複を拒否し理由表示。
- [ ] **U6** modal/palette に dialog セマンティクス無し (`index.html`) — med/S
      → role/aria 付与。
- [ ] **U7** ウィンドウ✕が確認なしで全セッション kill (`app.js`) — med/S
      → 実行中セッションがあれば確認。
- [ ] **U8** ナビの JP/EN 混在(Workflows だけ英語) (`index.html`) — low/S
- [ ] **R2** SSH 切断が終了コード 0(正常終了)に見える (`ssh.rs`) — med/S
      → exit-status 無しの終了を別扱い。
- [ ] **R4** 保存モデルの検証なし(空 host/port 0 等) (`store.rs`) — low/S

## デザイン

- [ ] **D1** ボタン/タブ/リスト行にキーボードフォーカス可視スタイル無し (`styles.css`) — high/S
      → `:focus-visible` グローバル。
- [ ] **D2** モーダルが画面を超えるとスクロールせずボタンが切れる (`styles.css`) — high/S
      → max-height+flex+body スクロール。
- [ ] **D3** pinned と focused のスレッド行が視覚的に同一 (`styles.css`) — med/S
- [ ] **D4** ペインドロップダウンがトークン外の #000/#fff (`styles.css`) — med/S
- [ ] **D5** 色トークンのドリフト(hex 直書き重複) (`styles.css`) — low/S

## CI / 配布(コード外・別 PR 候補)

- [ ] **CI1** clippy/fmt/`cargo check -p senju-term` の CI ゲート無し (`build.yml`) — med/S
- [ ] **CI2** actions 未ピン(`tauri-action@v0` 等) (`build.yml`) — med/S
- [ ] **CI3** release profile 未調整(lto/strip) (`Cargo.toml`) — low/S

## 対応方針

- セキュリティ・高影響のコード品質を優先し、フロント / コア / 設定を横断して小さめの PR に分けて消化。
- CI/配布・署名/updater(1.4,4.4 等)はコード変更と独立のため本チェックリストでは後回し(別途相談)。
