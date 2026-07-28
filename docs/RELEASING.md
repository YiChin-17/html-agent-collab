# Source-only release 流程

本專案目前只發布 source-only GitHub Release，不發布 crates.io package、macOS application bundle 或未簽署 binary。

## 首次公開 repository

Repository 在準備與歷史清理期間目前保持 private。完成所有公開前檢查後，先讀回目前設定，確認 description 對應 Rust、Tauri 2 與 macOS WKWebView 架構，default branch 為 `main`：

```bash
gh repo view YiChin-17/html-agent-collab --json visibility,description,defaultBranchRef
rg -n '^license = "MIT OR Apache-2.0"$' Cargo.toml
test -f LICENSE-MIT
test -f LICENSE-APACHE
test -f NOTICE
gh repo view YiChin-17/html-agent-collab --json licenseInfo
```

`Cargo.toml` 的 SPDX expression、兩份完整 license text 與 `NOTICE` 是 repository 雙授權的權威證據。GitHub `licenseInfo` 只提供平台偵測到的單一 license，可能只顯示 `Apache-2.0`，不得用來判定雙授權是否成立。

只有 maintainer 明確決定公開時，才依下列順序切換 visibility、立即啟用 GitHub private vulnerability reporting，並讀回驗證。任一步驟失敗都代表首次公開流程尚未完成：

```bash
gh repo edit YiChin-17/html-agent-collab --visibility public --accept-visibility-change-consequences
gh api --method PUT repos/YiChin-17/html-agent-collab/private-vulnerability-reporting
gh repo view YiChin-17/html-agent-collab --json visibility,description,defaultBranchRef,licenseInfo
gh api repos/YiChin-17/html-agent-collab/private-vulnerability-reporting --jq '.enabled'
```

預期最後兩個命令分別顯示 `PUBLIC`、正確的 description、`main` 與 PVR 的 `true`；`licenseInfo` 僅供記錄 GitHub 的平台偵測結果。GitHub 在 private repository 不提供此 PVR endpoint，因此準備期間不得把 HTTP 404 誤判為已啟用。

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

## 建立 release

建立並推送格式為 `v<major>.<minor>.<patch>` 的 annotated tag：

```bash
git tag -a v0.1.0 -m "v0.1.0"
git push origin v0.1.0
```

`Source release` GitHub Actions workflow 會驗證 tag、建立 GitHub Release 並產生 release notes。GitHub 自動附加 source archives；workflow 不建置或上傳 binary。

## 失敗處理

- 任一驗證失敗時不得建立 tag。
- Workflow 在 release 建立前失敗時，修正後刪除 local/remote tag，再以新 commit 建立 tag；不得讓相同 tag 指向不同公開 commit。
- Release 已公開後若發現問題，保留既有 tag 並建立 patch release，不覆寫舊 release。
