# Source-only release 流程

本專案只發布 source-only GitHub Release，不發布 crates.io package、macOS application bundle 或未簽署 binary。Release 由 GitHub Actions 從 tag 建立，GitHub 自動附加 source archives。

## 準備 release

1. 確認 `main` 為最新且 worktree clean。
2. 將 `CHANGELOG.md` 的 Unreleased 項目移至新版本，使用 `YYYY-MM-DD` 日期。
3. 將 `Cargo.toml` 與 `tauri.conf.json` 的版本同步為相同 Semantic Version。
4. 執行完整驗證：

   ```bash
   cargo fmt -- --check
   cargo test
   cargo clippy --all-targets --all-features -- -D warnings
   cargo package
   cargo deny check
   gitleaks detect --source .
   scripts/session-ux-acceptance.sh
   ```

   `cargo package` 要求 worktree clean，在版本檔尚未提交時會失敗。先以 `cargo package --allow-dirty` 通過驗證，提交後再跑一次無旗標的 `cargo package` 確認。

5. 提交版本變更：

   ```bash
   git add CHANGELOG.md Cargo.lock Cargo.toml tauri.conf.json
   git commit -m "chore(release): v0.4.0"
   ```

## 建立 release

建立並推送 annotated tag，格式固定為 `v<major>.<minor>.<patch>`：

```bash
git tag -a v0.4.0 -m "v0.4.0"
git push origin v0.4.0
```

`Source release` workflow 由 `v*` tag 觸發，驗證版本格式後建立 GitHub Release 並產生 release notes。Workflow 不建置或上傳 binary。

確認 workflow 成功且 release 存在：

```bash
gh run list --workflow release.yml --limit 1
gh release view v0.4.0
```

## 失敗處理

- 任一驗證失敗時不得建立 tag。
- Workflow 在 release 建立前失敗時，修正後刪除 local/remote tag，再以新 commit 建立 tag；不得讓相同 tag 指向不同公開 commit。
- Release 已公開後若發現問題，保留既有 tag 並建立 patch release，不覆寫舊 release。

## 授權與安全設定

`Cargo.toml` 的 SPDX expression、兩份完整 license text 與 `NOTICE` 是 repository 雙授權的權威證據：

```bash
rg -n '^license = "MIT OR Apache-2.0"$' Cargo.toml
test -f LICENSE-MIT
test -f LICENSE-APACHE
test -f NOTICE
```

GitHub 的 `licenseInfo` 只提供平台偵測到的單一 license，目前顯示 `Apache-2.0`，不得用來判定雙授權是否成立。

安全漏洞通報管道見 `SECURITY.md`。
