# Fork 维护手册（release_fork）

`release_fork = 上游 main + fork-only 提交`：目前只有 CI（cachix-fork.yml）
+ 本手册 + 同步脚本，无代码分叉。日常只有一种操作：**跟进上游**（rebase，保持线性）。

历史说明：`wanten/ime-popup-overflow`（允许 input-method popup 溢出窗口）
已于 2026-10-10 被上游合并（`39dc2197` + 后续修正 `d0cd58ec`），
上游实现比原分支更完整，无需再合入。旧分支留档即可。

## Fork-only 文件（rebase 时保留）

* `.github/workflows/cachix-fork.yml`：release_fork 专属构建推送，上游没有此文件。
* `.github/workflows/ci.yml`：仅改了 `on.push`，加了 `branches-ignore: [release_fork]`。
  上游全量矩阵在 release_fork push 时不需要（构建由 cachix-fork 负责），
  PR 仍会跑 CI。rebase 若提示此 hunk 冲突，保留我方 `branches-ignore` 即可。
* `FORK.md` / `sync-upstream.sh`：本手册与同步脚本，上游没有。

## 0. 一键同步（推荐）

```bash
./sync-upstream.sh
```

脚本会：检查分支干净 → 加 `upstream`（没有的话）→ fetch →
rebase 到 `upstream/main`。
**它从不 push**：看完结果自己手动推。

推完等 CI 绿，nixos 侧 `nix flake update niri` + 重建验证（第 3 节）。

## 1. 手动步骤（脚本做的事，拆开看）

```bash
git remote add upstream https://github.com/niri-wm/niri.git  # 一次性
git fetch upstream
git checkout release_fork
git rebase upstream/main     # 冲突则 git rebase --abort，分支不动
```

niri 没有 grammar lock 这类生成文件：`Cargo.lock` / `flake.lock`
直接跟随上游 rebase 即可，无需额外刷新步骤。
如果上游更新了 `flake.lock` 里的 nixpkgs，大版本构建产物会变，
CI 会全量重建一次，属于正常现象。

## 2. 改动-成本速查

| 你改了什么 | CI 代价 |
|---|---|
| 只改 `.github/` / `FORK.md` / `sync-upstream.sh` | 一次 nix 构建（`gitRev` 随提交变，躲不掉）|
| Rust 代码 / `Cargo.lock` | 全量（cargo + nix 都重建）|
| `flake.nix` / `flake.lock` | 全量（依赖图变化，闭包大变）|

## 3. nixos 侧验证（冷机等价）

```bash
nix flake update niri   # dotfiles/nixos 下
NIRI=$(readlink -f $(which niri))
nix path-info --store https://wanten.cachix.org "$NIRI"   # 打印路径 = 在缓存里
for p in $(nix-store -qR $NIRI); do
  nix path-info --store https://wanten.cachix.org "$p" >/dev/null 2>&1 && continue
  nix path-info --store https://cache.nixos.org "$p" >/dev/null 2>&1 && continue
  echo "MISS $p"
done
```

无 MISS = 任意冷机纯下载可用。`niri --version` 的 commit 应与
`release_fork` HEAD 短 hash 一致（nix 构建经 `NIRI_BUILD_COMMIT` 注入）。

## 4. 排错

* `cachix push` 报 `Nothing to push` 且构建失败 → 看完整日志找第一个
  `error: Cannot build`（`pipefail` 已开，这种情况 CI 会直接红）。
* CI 磁盘不足 → `Free Disk Space` 步骤已加，一般不用动；
  若仍满，考虑把 `nix build` 切到 `niri-debug` 之外的小闭包先验证。
* rebase 冲突（极少，fork-only 文件与上游同名时）→
  `git rebase --abort`，分支不动，手动处理后再试。
  fork-only 文件为 `.github/workflows/cachix-fork.yml` / `FORK.md` /
  `sync-upstream.sh`，上游一般不会碰这些路径。
