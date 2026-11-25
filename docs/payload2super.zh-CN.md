# 用全量 OTA 组装可刷写的 super.img —— lp-rs 端到端实践

> 全文基于一次真实操作：OnePlus CPH2487（ColorOS 16 / V16.0.0）8.2 GB 全量
> OTA，其 payload.bin 不含 super 整块、且缺失 `my_preload` / `my_company`
> 两个动态分区。目标：把 payload 里的"货" + 机型几何"图纸"组装成一张完整的
> `super.img`，一条 `fastboot flash super` 完成更新，替代 fastbootd 下
> 15 次 `resize + flash` 的传统流程。
>
> 文中所有时间、字节数、哈希结果均为实测。

---

## 0. 为什么要这么玩

| 传统姿势 | 本文姿势 |
|---|---|
| fastbootd 逐分区刷：`resize-logical-partition` ×15 + `flash` ×15，中途任何一步失败 = 分区表半改状态 | 本地组装完整 `super.img`，`fastboot flash super` 一次覆写，失败重刷即可，天然可回滚 |
| 卡刷依赖官方包完整性：OTA 没带的分区（如 `my_preload`）你无能为力 | 缺哪个补哪个：任意来源的同机型分区镜像都能焊进 super |
| AOSP 工具链（lpmake 等）只有 Linux 预编译，Windows 要 WSL/自编译 | lp-rs 纯 Rust、MSVC 原生、`cargo install lp-tools-rs`，或直接下载 release 二进制 |

适用条件（缺一不可，见 §2）：你有机型的 **LP 几何表**（9008 包元数据 /
真机 dump / 社区模板库），以及**全部动态分区的新镜像**。

---

## 1. 原理：super = 一张表 + 一堆货

`super` 分区不是文件系统，是 **LP（Logical Partition）容器**——角色相当于
磁盘的 GPT，表里登记的"分区"再各自装镜像：

```text
偏移0        4KiB                        1MiB
┌─────────┬─────────────────────────┬──────────────────────────────┐
│ 保留区   │ Geometry×2(主/备份)       │ 数据区（镜像按 extent 顺序摆放，  │
│         │ + Metadata×N个slot       │  1MiB 对齐）                   │
│         │ (每个含: Header/分区表/   │ system→system_ext→vendor→...  │
│         │  extent表/组表/设备表)     │  (*_b 是 0 尺寸占位)           │
└─────────┴─────────────────────────┴──────────────────────────────┘
```

三条决定生死的性质：

1. **逻辑分区内容原样落盘。** 镜像（erofs/ext4 + verity）就是普通字节流，
   组装 = 按表抄写 + 建表。只要表对、货对，`flash super` 与逐分区 flash 的
   最终字节完全等价——AVB 校验链对"这次组装"毫无感知。
2. **表先于货。** 开机时 init 读 metadata → device-mapper 造
   `/dev/block/dm-N`，内核才"看见"那 17 个分区。表错一个字节全盘错位。
3. **payload.bin 里没有表。** OTA 只携带"货"（分区新镜像 + 差分指令）。
   device-size、组名与上限、slot 数、分区归属这些几何信息，只存在于设备/
   官方刷写包里——这正是"从 OTA 组 super"最容易被忽略的前置材料。

**Virtual A/B 补充**：Android 13+ 的 ColorOS 用 VAB。super 表里每个分区有
`_a`（有内容）和 `_b`（0 字节占位）两项；组上限远大于已用空间，差额留给
update_engine 做快照（COW）。所以组装时要 `--metadata-slots 3`（第三槽给
快照元数据）+ VAB 标志，照抄原表即可，见 §4.3。

---

## 2. 材料清单

| # | 材料 | 从哪来 | 本文实例 |
|---|---|---|---|
| 1 | 全部动态分区新镜像 | `payload.bin` dump | 15 个来自 OTA |
| 2 | OTA 缺失的动态分区镜像 | 真机 super / 同代官方包 dump | `my_preload`(2.1MB)、`my_company`(335KB) 手动塞入 |
| 3 | LP 几何表 | ① 9008 包里的 super metadata 裸块；② 真机 `dd` super 头部；③ 本仓库 [`devices/`](../devices/) 模板 | `super_meta.raw`（6736 B，从一加官方 9008 包扒出） |
| 4 | 工具 | `cargo install lp-tools-rs`（lpdump/lpunpack/lpmake）+ 任一 payload dumper（或本仓库 `payload-ls`） | 见 §3 |

> 判定"全部"的口径是 **表（材料3）的分区清单**，不是 OTA/care_map 的清单——
> care_map 只回答"这次 OTA 动了谁"，而 `flash super` 是整盘覆写：表里有、
> 货里没有的分区，不补 = 清零。

---

## 3. 工具链（lp-rs 全家福）

| 工具 | 干什么 | 本文用在哪 |
|---|---|---|
| `lpdump` | 读 super 表（raw/sparse/裸 metadata 都行），`--json` 出机器可读全表 | 解 9008 元数据、成品验收 |
| `lpunpack` | 从 super 反向抠分区 | 抽检回环 |
| `lpmake` | **组装 super**（v0.2 起支持 `--config` 机型模板） | 主菜 |
| `lpadd` / `lpflash` | 往已有 super 里换单个分区 / 刷表 | 后续微调 |
| `payload-ls` | 读 payload.bin 容器 + manifest，可列可导 | 核对分区表 |

与 AOSP C++ 工具的行为对齐已实测：同一 9008 表组装后 `lpdump` 对照，
block device / groups / 34 个分区条目 1:1 相同（仅 size 随新镜像变化）。

---

## 4. 实操流水账

### 4.1 dump payload

任一 Rust dumper 均可（`payload_dumper` / `unpayload` / 本仓库
`payload-ls -a`）。建议只导动态分区，省 30% 磁盘：

```bash
payload-ls payload.bin                     # 先看表
payload-ls payload.bin -a ./imgs           # 全量导出（本包 op 全为 XZ/BZ/RAW）
```

导出后核对：`ls imgs/` 里应有 §2 材料1 的 15 个镜像，文件名即分区基名
（payload manifest 里无 `_a` 后缀，LP 表里有——模板里做映射）。

### 4.2 解析几何表

9008 包里的 super metadata 裸块（一般叫 `super_*.img/.raw`，只有几十 KB
——那就是纯表头），或真机头部：

```bash
# 真机替代路径：root 后
adb shell su -c "dd if=/dev/block/by-name/super of=/sdcard/super_head.bin bs=1M count=16"

lpdump --json super_meta.raw > cph2487.meta.json
```

从 JSON 抄出四个数：`block_devices[0].size`（本文 15,032,385,536 = 14 GiB）、
`alignment`（1 MiB）、`metadata`（65536 × 3 slot，flags 含 VAB）、
`groups` 两项 max_size（15,028,191,232）。

### 4.3 写机型模板 `devices/<model>.json`

完整 schema 与采集说明见 [`devices/README.md`](../devices/README.md)。
模板要点：分区顺序、`readonly` 属性、组归属照原表 1:1；`image` 字段写 dump
目录的基名；`*_b` 一律省略 image（自动 0 尺寸占位）。CPH2487 实例：

```json
{
  "device":   { "size": 15032385536, "alignment": 1048576, "block_size": 4096 },
  "metadata": { "size": 65536, "slots": 3, "virtual_ab": true },
  "groups":   [ { "name": "qti_dynamic_partitions_a", "maximum_size": 15028191232 },
                { "name": "qti_dynamic_partitions_b", "maximum_size": 15028191232 } ],
  "partitions": [
    { "name": "system_a", "attrs": "readonly", "group": "qti_dynamic_partitions_a", "image": "system.img" },
    { "name": "system_b", "attrs": "readonly", "group": "qti_dynamic_partitions_b" },
    "...其余 15 组照抄，含塞入的 my_preload.img / my_company.img"
  ]
}
```

**缺件自查**：模板里每个 `image` 必须存在。`payload2super.ps1` 会在组装前
列出全部缺件并提示往 dump 目录里补图——这是最容易翻车的一步。

### 4.4 组装

一条命令（或 `docs/scripts/payload2super.ps1` 包一层预检+验收）：

```bash
lpmake --config devices/cph2487.json --images-dir ./extracted_partitions \
       --sparse -o super_CPH2487_11.H.59.img
```

实测：**10,382,589,952 B 内容 → sparse 10,348,518,152 B，1 min 42 s**
（全程流式，无临时大文件；分区 size 自动从镜像文件推导）。

### 4.5 三重验收（发版截图就靠它）

```bash
lpdump --json super_xxx.img        # ① 与 §4.2 原表逐字段对照（size 应变为新版）
lpunpack -p my_preload_a super_xxx.img ./check   # ② 抽几个分区回环
sha256sum check/my_preload_a.img extracted/my_preload.img   # ③ 必须逐字节相等
```

本次实测：回环抽检 `product/my_preload/my_company/my_manifest` 4/4 命中，
且**手动塞入的两个分区原样穿过组装流程**（注入正确性的直接证据）。

### 4.6 刷写

`super` 是逻辑容器，**只在 fastbootd 可见**（bootloader fastboot 没有它）：

```powershell
# docs/scripts/flash_super.ps1：自动 reboot fastbootd、按 current-slot 刷
fastboot flash super super_CPH2487_11.H.59.img     # 一条，顶原来 15 条
# 静态分区照常逐个刷（boot/vbmeta/dtbo/modem… 小文件）
fastboot --set-active=a
fastboot reboot
```

10 GB sparse 经 fastboot 展开，USB2 约 5-8 分钟、USB3 显著更快；刷完别急
着拔线看进度条错觉，fastboot 报 finished 才算。

---

## 5. 踩坑实录（全部真实发生）

1. **OTA 缺分区不报错，直接静默清零**。`flash super` 整盘覆写，manifest
   没有 `my_preload`，不注入就是空分区。症状在开机后很久才出现（预装应用/
   企业配置损坏）。→ 永远以 LP 表为缺件清单。
2. **`payload_inspector.py` 的头部图例（u64 签名长度、blob 在 manifest 前）
   与 Android 实际不符**：真包是 u32 + blob 后置（头 24 字节）。按 Python
   老例写解析器会在第一个 op 哈希时神秘失败。lp-rs 的
   `third_party/aosp/PROVENANCE.md` 记录了对抗验证过程。
3. **`data_sha256_hash` 覆盖含对齐 padding 的完整压缩块**，解码器提前停在
   流末尾 → 首版自研 dumper 在 system 第 182 号 op 上报哈希不匹配。修法：
   解码后把同一区间剩余字节补灌进同一 hasher。
4. **device-size/group max 不能"顺手改大"**。设备块分区是物理分区，超出即
   刷败；组上限影响后续 OTA 的 COW 空间——必须照抄真机表。
5. **分区基名与槽位后缀**：payload manifest 无后缀、LP 表带 `_a/_b`，
   模板里 `image` 字段是桥梁，别写反。
6. **AVB 不用动**：只要没改分区内容、补的图也来自同机型官方镜像，
   vbmeta 摘要链天然吻合，**不需要** `--disable-verification`；反过来，
   改过任何一个字节的，别指望这套流程能过校验。

---

## 6. FAQ

**Q：没有 9008 包，机型模板从哪来？**
A：root 真机 `dd` super 前 16 MB → `lpdump --json` → 交 PR 到 `devices/`。
没有 root：刷过同机型包后的旧 super.img 也可以。

**Q：组出来的 super.img 和官方全包的 super.img 一样吗？**
A：字节级不必相同（extent 摆放顺序可能不同），语义等价：分区内容、表、
哈希链一致，刷后效果相同。

**Q：增量包（VAB delta）能用吗？**
A：提取阶段需要 `--source-dir` 提供旧镜像 + 更多 op 解码器
（SOURCE_COPY/PUFFDIFF/ZUCCHINI…），在 lp-payload-rs 路线图上，见 §7。

**Q：不想用 fastbootd，能卡刷吗？**
A：那是另一个工程（把组装结果回焊成 payload/OTA 并过签名链），
关注 superforge 方向。

---

## 7. 后续路线

- `lp-payload-rs`：补齐 delta/source 类 op（补丁 codec 已有纯 Rust 依赖可
  用：puffdiff / lz4diff / bsdiff-android），实现"增量包直导"；
- `devices/` 机型模板库共建（小米/ColorOS/Oxygen OS 征集帖见酷安置顶）；
- egui 图形化 `superforge`（拖拽 → 模板选择 → 缺件注入 → 组装 → 验收 →
  刷机一条龙）。

---

## 附：本文实测数据

| 项 | 值 |
|---|---|
| 包 | CPH2487_11.H.59（ColorOS V16.0.0, Android 16, SP 2026-09-01） |
| payload.bin | 8,234,748,043 B / 46 分区 / 5,415 ops |
| super 成员 | 17（15 来自 OTA + 2 手动注入） |
| 组装耗时 | 103 s（`lpmake --config --sparse`） |
| 成品 | 10,348,518,152 B sparse |
| 验收 | 表对照 1:1；回环抽检 4/4 SHA256 相等；（另：全 46 分区 payload 重建 vs 磁盘 dump 交叉哈希 46/46，见 `cargo test -- --ignored`） |
