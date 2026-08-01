# 排查方法论

`atom` 内核（riscv64 S-mode，QEMU virt）的排查路径指引。上一会话在实现任务栈
守护页 + trap 同步异常终止时暴露了系统性缺陷（延迟显现 / 手算位运算出错 /
没用 gdbstub / panic 上下文不足 / 多 demo 日志淹没），本文件把应对措施落成
可用的工具与流程。

## 1. 内建边界断言 — 把破坏提前到事发当场

"延迟显现"类 bug（`write_bytes` 越界清零页表、spawn 时页表完好、首次调度才崩）
的根治手段是**边界断言**：在破坏发生的操作附近校验不变量，debug 构建下断言
立即炸出破坏点，而不是事后凭日志推断。

全部用 `debug_assert!`——`cargo build`（dev profile）默认 `debug-assertions = true`
时生效，release 构建整体裁剪（辅助函数标 `#[cfg(debug_assertions)]`）。

| 位置（src/scheduler.rs） | 校验的不变量 |
| --- | --- |
| `spawn_impl`（`space.map` 后） | 栈基址 `translate` 命中预期物理帧；栈顶页已映射；**守护页必须未映射**——一旦被映射，栈溢出防护静默失效 |
| `wake_task` | `frame_phys(t)` 落在 DRAM 恒等区——跨任务物理写的前提，防止把 sepc 写进垃圾地址 |
| `reclaim_zombies` | 僵尸栈基址页对齐且在 DRAM——回收 layout 与 spawn 分配时一致，防止把垃圾地址还给分配器 |
| `scheduler()`（`switch_space` 后） | 读 `satp` 校验 PPN == 目标空间 `root_page()`——硬件状态切换成功 |

排查手法：怀疑"映射 / 物理地址 / 地址空间切换"被破坏时，在 debug 构建下跑到该
场景——断言在破坏瞬间炸出并打印现场（见 §2），而不是延迟到首次调度才崩。

## 2. panic 输出字段解读

`src/panic.rs` 的 handler 经 SBI 直写控制台（绕过所有 S-mode 锁，崩溃时不死锁），
默认全量输出。字段含义与定位价值：

| 字段 | 含义 | 定位价值 |
| --- | --- | --- |
| `scause` | 异常原因（含解码） | 中断 vs 异常、缺页 / 非法指令 / ecall 等 |
| `sepc` | 出错指令地址 | 定位哪条指令 |
| `sstatus` | SPP / SPIE / SIE | 崩溃来自 S/U、中断是否使能 |
| `stval` | 故障地址 | 缺页的访问地址 / 非法指令编码 |
| `sp` | 栈指针 | **判断栈是否越界**——上次靠它拿到"sp 在栈顶上方"关键线索 |
| `satp` | 根页表 PPN（含 MODE 解码） | 崩溃时活动地址空间——是内核空间还是某任务空间 |
| `s0` | 帧指针 | backtrace 起点，独立于回溯展示 |
| `Backtrace` | frame-pointer 展开 | 调用链 |

交叉判断技巧：`sp` 落在任务栈窗口（`0xC000_0000` 附近）而 `satp` 指向内核空间
根页表 → 栈 / 地址空间不一致；`satp` 的 PPN 不在已知空间集合 → 页表被写坏。

## 3. QEMU gdbstub + watchpoint — "谁写坏了内存"

对"谁写坏了页表 / 栈 / 结构体"类问题，全程日志 dump 推断低效——gdb
watchpoint 直接在写入发生时命中写入者。

### 启动调试会话

```bash
# 终端 1：QEMU 加 -s（gdbstub 监听 :1234）+ -S（启动即暂停，等 gdb）
qemu-system-riscv64 -machine virt -bios default \
    -kernel target/riscv64gc-unknown-none-elf/debug/atom \
    -nographic -s -S
```

```bash
# 终端 2：gdb 连接。Arch/CachyOS 官方 gdb 包内置全部架构支持，直接用：
gdb target/riscv64gc-unknown-none-elf/debug/atom
(gdb) target remote :1234
```

`scripts/debug.gdb` 是可直接运行的模板（`gdb -x scripts/debug.gdb`）。

### 流程

1. `-S` 使 QEMU 暂停在复位向量（`0x1000`，OpenSBI 之前，M-mode）。
2. 内核 ELF 符号就是 `0x8020xxxx` 物理地址（link.ld 直接链接，无高半区符号），
   等 OpenSBI 跑完停在入口即可加载符号：

   ```
   (gdb) break *0x80200000    # 内核入口 _start
   (gdb) continue
   ```

   之后正常按符号设断点（`break early` / `break main`）。
3. 单 hart 内核，`info registers` 看 x1(sp)/x2(sp)/x8(s0) 等。

### watchpoint 关键点

内核 DRAM **恒等映射（VA == PA）**，所以 `watch *0x8020xxxx` 直接监听**物理地址**——
无论当前哪个地址空间活动，物理写入都会命中，无需换算虚拟地址。

```
(gdb) watch *0x80201000        # 监听该物理地址被写
(gdb) continue
# 命中后：写入者现场
(gdb) info registers
(gdb) x/8gx 0x80201000         # 被写坏的内存内容
```

- QEMU RISC-V 的硬件 watchpoint 数量有限（一般约 4 个）；`delete 1` 清理旧的。
- 经典流程：**先 dump 被破坏的内存 → 对破坏地址设 watch → 重跑 → 命中即写入者**。
- 若 watch 命中在地址不匹配（如只命中部分访问），改用 `awatch` / 扩大监测窗口。

## 4. 排查方法论清单

- **最小复现**：`src/main.rs` 顶部的 `DEMO_*` const 开关一次只开一个——多个任务
  日志互相淹没时谁都看不出异常。锁定目标场景后再逐步叠加。
- **二分探针**：怀疑某操作破坏内存，在其前后插探针（打印或断言），把破坏边界
  缩小到两个探针之间。
- **API 语义确认**：Rust 的坑常在"参数单位"。经典案例：
  `core::ptr::write_bytes(ptr, 0, count)` 的 `count` 是**元素数**——对
  `*mut TrapFrame` 传 `count=264` 会写 264×264 字节越栈顶；写裸指针字节必须
  `ptr as *mut u8`。遇到可疑 API 先查签名确认语义，别凭直觉。
- **位运算用脚本别心算**：PPN / VPN / 偏移手算极易错（上次把正常页表误判成
  垃圾值，钻进"分配器双重分配"错误方向）。用 python：

  ```python
  vpn = lambda v, l: (v >> (12 + l * 9)) & 0x1FF
  ppn = 0x80200000 >> 12
  ```

- **先看 panic 首帧的 sp / satp**：判断崩溃在哪个空间、栈是否越界，再读 backtrace。
- **断言炸出的位置就是破坏点**：debug 构建下断言直指破坏发生的操作；日志只反映
  破坏之后的状态。

## 相关文件

- `src/scheduler.rs` — 边界断言实现（spawn / wake / reclaim / switch）
- `src/panic.rs` — panic 输出（sp / satp / s0 增强）
- `src/main.rs` — `DEMO_*` 最小复现开关
- `scripts/debug.gdb` — gdbstub 会话模板
