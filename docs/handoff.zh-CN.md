# lp-rs 项目交接文档（Linux 接手 / 新会话模型先读这份）

> 更新：2026-11-25，v0.2.0 发版当天。本文档面向：作者切到 Linux 后开的新工具会话。
> 读者假定：一个刚 clone 本仓库的 AI 助手/新贡献者。你不需要之前任何对话记录。

## 0. 一句话现状

纯 Rust Android 动态分区 + OTA payload 工具链。**v0.2.0 已发布**：GitHub tag/Release（4 平台
lp-tools 二进制 + SHA256SUMS）、crates.io 四 crate 全部 0.2.0，CI（fmt/clippy/test/rustdoc）
与 Release workflow 全绿。main HEAD = `0d2225b`。作者 = QAQTam（仓库作者本人，中文交流）。

## 1. Linux 环境速装

```sh
# 工具链（edition 2024，rustc ≥ 1.85；本地开发与 CI 实测于 1.100）
rustup default stable

# 四个 crate 全部 cargo install 即可，无 C 依赖：
#   lp-sparse-rs / liblp-rs / lp-tools-rs(lpmake lpdump lpunpack lpadd lpflash)
#   lp-payload-rs(payload-ls)  —— xz2 用 static feature，自带 liblzma 源码，免装系统包
cargo install lp-tools-rs lp-payload-rs
# 或 GitHub Release 下 tar.gz（包名格式兼容 cargo-binstall）：
#   https://github.com/QAQTam/lp-rs/releases/tag/v0.2.0

# 刷机必踩坑：fastboot USB 权限
#   Arch: android-tools 包自带 udev 规则；deb 系装 SuperUser 的 51-android.rules
# 验证：fastboot devices（设备在 fastbootd，`fastboot reboot fastboot` 进入）
```

Windows 侧历史环境备查：MSVC rust 1.100 + `cargo install protox --features bin`（0.10.0），
仅重新生成 proto 时需要（配方见 `third_party/aosp/PROVENANCE.md`，正常开发**不需要 protoc/protox**，
生成物已入库）。

## 2. 测试素材清单（跨机拷贝务必核对指纹）

素材在 Windows 机 `D:\Projects\lp-rs` 之外，切 Linux 需自行拷过去（或重新下载 OTA）：

| 文件 | 大小 (B) | SHA256 | 用途 |
|---|---:|---|---|
| `C:\Downloads\e307de20…\payload.bin` | 8,234,748,043 | `316172a69d78fc5fc785128fd13bb5cd4333673ee3757270c902d9c3f6bce344` | CPH2487 ColorOS 16 (V16.0.0/A16, SP 2026-09-01) 全量 OTA；两个 `#[ignore]` 集成测试的输入 |
| `C:\Downloads\super_meta.10010111.raw` | 6,736 | `14eded304adf5bdd38bbd20318a3d370383b6cba67705c00c07cfacea7da30aa` | 9008 读出的裸 LP metadata，devices/cph2487.json 的几何来源，验证对照基准 |
| `C:\Downloads\e307de20…\extracted_2026-11-25_16-26-48\` | 48 个 img | （逐分区哈希由集成测试现场比对） | payload_dumper 全量 dump；其中 my_preload.img / my_company.img 是手动塞入的同机型文件，**来源未确认**，勿当权威 |
| `out/super_CPH2487_11.H.59_V16.0.0.img` | 10,348,518,152（sparse，未上 git） | — | 已组装成品；`out/` 被 gitignore，可在任何平台用下面 §5 命令重建（约 1'42"） |

## 3. 仓库地图

```
crates/lp-sparse-rs/   Android sparse 读写（流式 builder，CRC32C，零依赖 C）
crates/liblp-rs/       LP metadata 核心：super 几何/分区表/group、解析+构建+诊断
crates/lp-tools-rs/    CLI：lpmake(含 --config JSON 设备模板)/lpdump(--json)/lpunpack/lpadd/lpflash
crates/lp-payload-rs/  CrAU OTA payload 读取：RAW/BZIP2/XZ/ZSTD、scatter、op+分区级 SHA256；payload-ls
devices/               设备模板 JSON + schema 说明（贡献必读 devices/README.md）
docs/                  payload2super.zh-CN.md（中文实操长文）、scripts/（公开版 ps1）
third_party/aosp/      update_metadata.proto 原件 + PROVENANCE.md（重取/重生成配方 + 实测结论）
```

`.gitignore` 里的 `/scripts/` 是**作者私人刷机脚本目录，故意不入库**——别改这条规则、别往里塞东西再提交。

## 4. 花过大代价换来的技术结论（别重新踩）

1. **CrAU v2 头 = 24 字节**；`signature_size` 是 **u32**（不是 u64）。Brillo payload_inspector
   那份 u64/前置图例对 Android 包**不成立**（详见 PROVENANCE.md）。
2. CPH2487 包：`data_start = header + manifest + blob = 301537`（集成测试用
   `LP_RS_EXPECT_DATA_START=301537` 钉死）。
3. **op 级 `data_sha256_hash` 覆盖"含 padding 的完整数据块"**：解码器提前停时要把 blob 区间
   剩余字节继续灌同一 hasher 再比对（lib.rs 的 Tap 逻辑）。
4. **Scatter op** 按 `dst_extents × block_size` 定位重建，依赖输出预先 `set_len` 零填充。
5. `extract_partition_to_file` 用 File::create（只写句柄）；**读回校验必须重新 open**
   （Windows 上曾 os error 5）。
6. CPH2487 几何：super 15,032,385,536 B，alignment 1,048,576，block 4096，metadata 65536×3 slots，
   LP v1.2 flags=1（VAB），group `qti_dynamic_partitions_a/b: 15,028,191,232`，17×2=34 分区。
   manifest 分区名无 `_a/_b` 后缀时 image 用基名（如 system.img）。
7. prost 生成代码直接 `include!` 进 `mod proto`；该模块上的 `#![allow(clippy::doc_overindented_list_items)]`
   别删（proto 注释缩进原样保留）。rustdoc 在 `-D warnings` 下会把注释里裸 `<foo>` 当 HTML 标签，写
   doc/CLI help 时用反引号包。
8. CI 四道门本地等价命令（发版前全跑过）：见 §5；**`RUSTDOCFLAGS="-D warnings" cargo doc --workspace
   --no-deps --locked` 是最容易漏的一道**（v0.2.0 首跑 CI 就挂在这，fix 在 0d2225b）。

## 5. 验证与复现命令

```sh
# 质量门（CI 同款）
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace          # 期望 17 个 "test result: ok"，两个集成测试显示 2 ignored
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked

# 大集成测试（需 §2 素材；cross-hash 单线程约 11 分钟，XZ 为主）
LP_RS_PAYLOAD=/path/payload.bin LP_RS_IMAGES_DIR=/path/extracted LP_RS_TMP_DIR=/path/tmp \
  cargo test -p lp-payload-rs --release -- --ignored --nocapture

# 从零复现组包 + 三重验证（payload-ls / lpdump / lpmake 在 PATH）
pwsh docs/scripts/payload2super.ps1 -Payload $LP_RS_PAYLOAD -Config devices/cph2487.json   # 或 -VerifyOnly
```

三重验证（上次全过，新平台重跑也应过）：① 成品 lpdump 对比 9008 原表（34 分区 attrs/group 1:1，
extent 尺寸 == 磁盘 img）；② `_a` 总 size ≤ group max；③ lpunpack 抽检若干分区 SHA256 == 磁盘 dump。

## 6. 打磨清单（按优先级；每项含完成判据）

- [ ] **P0 `lpdump --export-config`**：把 `lpdump --json` 的 metadata 转成 devices 模板 JSON
    （入口 `crates/lp-tools-rs/src/bin/lpdump.rs`，映射规则照 `devices/cph2487.json` 反向）。
    判据：`lpdump --export-config super_meta.raw > x.json && lpmake --config x.json --dry-run`
    分区表一致；对 9008 raw 跑出的结果与 cph2487.json diff 干净。
- [ ] **P0 release 矩阵补 payload-ls**：`.github/workflows/release.yml` 的 bin 列表/打包脚本
    （现在硬编码 lp-tools 五件套）。判据：v0.2.1 tag 后 Release assets 含 payload-ls 且
    `cargo binstall lp-payload-rs` 可装。
- [ ] **P1 组装流程收进单一命令**（方向已定，先写 RFC 再动手）：`--payload` 直通或新 bin
    `payload2super`，替代 PowerShell 编排；bash 版脚本可作过渡。判据：Linux 无 pwsh 环境
    一条命令完成 payload→super 并通过 §5 验证。
- [ ] **P1 并行提取**：`extract_partition_to_file` 按分区并行（各线程独立 `Payload::open`，
    op 区间独立可 seek）。判据：§5 的 cross-hash 测试在 8 核上 ≤3 分钟且 46/46 仍全过。
- [ ] **P2 `lpsuper check`**（或 lpmake --verify）：把三重验证内建为命令。判据：对好/坏 super
    分别输出可机读的 pass/fail 列表。
- [ ] **P2 delta ops**（增量包支持）：选型已完成（puffdiff/bsdiff-android/lz4diff）；
    需要用户提供素材：增量 payload.bin + 前后两版全镜像。ZUCCHINI：unpayload(kirklin, MIT)
    的 `src/zucchini/` 有纯 Rust 实现可 vendor 评估。命名注意：superforge 方向**避开 "packer"**
    （crates.io 已有 payload_packer）。
- [ ] **P3 superforge-gui**（egui 四页：拆解/组装/验收/刷机）：依赖 P0 export-config、
    P2 check、liblp 进度回调。
- [ ] **真机 CPH2487 首刷**（作者在场再做）：flash_super 脚本的 fastboot 路径**从未真机跑过**。
    步骤底线：先 `fastboot fetch super_a`（或 9008）留底 → `-SkipStatic` 单刷 super 验证 →
    vbmeta 链全部用官方 dump，不要 --disable-verification。

## 7. 作者偏好 / 未闭环话题

- 中文交流；**仓库内面向世界的文档英文，面向酷安/B站的教程中文**（payload2super.zh-CN.md 是范例）。
- 代码风格红线：clippy `-D warnings` 清零是硬要求（有过专门的 lint 清零 commit）。
- 帖文初稿（酷安长文 + B站口播稿）：钩子清单作者看过、**尚未拍板**。开写前先问。
- 发版流程模板（下次如 0.2.1）：workspace bump（root Cargo.toml 的
  `[workspace.package] version` + `[workspace.dependencies]` 里 lp-sparse-rs/liblp-rs 两个
  version req **必须同步**）→ `cargo check --workspace` 刷 lock → §5 四道门 → 逻辑分组 commit →
  push → `git tag -a vX.Y.Z` push（自动触发 Release）→ 按依赖序 `cargo publish`：
  lp-sparse-rs → liblp-rs → lp-tools-rs → lp-payload-rs。凭证 `~/.cargo/credentials.toml`。
- 已知遗留：已发布的 0.2.0 lp-payload-rs 源码含一条会挂 CI 的裸 `<partition>` doc comment
  （0d2225b 已修，仅 crate 快照里未带——无害，别为此单独发 0.2.1，和 P0 一起做）。

## 8. 版本速查

| crate | crates.io | 说明 |
|---|---|---|
| lp-sparse-rs | 0.2.0 | sparse 镜像读写 |
| liblp-rs | 0.2.0 | LP metadata 库（lib 名 `liblp`） |
| lp-tools-rs | 0.2.0 | 五 CLI |
| lp-payload-rs | 0.2.0 | payload 读取 + payload-ls（lib 名 `lp_payload`） |

历史：0.1.0（2026-11-20 首发，前三者）→ 0.2.0（2026-11-25，加 lp-payload-rs、lpmake --config、
devices 模板、文档工作流）。同名领域 crate（payload_dumper/unpayload/otaripper/payload_packer）
是**第三方**，注意别混淆也别撞名。
