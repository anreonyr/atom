# TODO — 待办清单

> 记录日期: 2026-08-01
> 来源: scheduler/trap 评审修复（评审第 8/9 节）后的独立 review
> 状态说明: 以下两项均为 **pre-existing 问题**（git diff 对照 HEAD 确认非本轮改动引入），
> 本轮只做记录，未擅自修复（涉及 asm/scheduler 语义改动，需单独排期）。

---

## P1 — 语义正确性（上下文切换 / 调度状态机）

### P1.1 trap_vector 入口 `li t0/t1` 污染被抢占任务的 t0/t1

- **文件**: `src/trap.rs` — `trap_vector` naked asm 第 ① 段栈检查
- **当前**: 守护页检查用 t0/t1 装载常量：
  ```asm
  li     t0, {base}      ; 0xC0000000
  li     t1, {guard}     ; 0xBFFFF000
  bgeu   sp, t0, 2f
  bltu   sp, t1, 2f
  ```
  随后保存帧时 `sd t0, 32(sp)` / `sd t1, 40(sp)` 存入的是**被污染的值**——被抢占任务恢复后 t0=0xC0000000、t1=0xBFFFF000，原始寄存器内容丢失。
- **影响**: 严格违反上下文切换"保存/恢复完整寄存器"语义。t0/t1 是 caller-saved，编译器在调用点之间不保证其存活，实际运行中几乎无感（demo 全路径正常）；但中断可在任意指令边界发生，任务恰在 t0/t1 持有活值时被抢占即出错。属潜伏 bug，非本轮引入。
- **目标**: 检查用临时寄存器不污染任务状态。候选方案：
  1. **sscratch 中转** — 入口 `csrrw` 交换，检查完恢复；需内核引入 sscratch 机制 + per-hart 初始化（标准做法，改动最大）
  2. **单寄存器检查** — 两个常量复用同一寄存器（先 `li t0, base` 比较，再 `li t0, guard` 比较），污染面从 t0+t1 缩到仅 t0
  3. **内存/CSR 常量** — 检查值放 `.rodata`（`la t0, const_slot` 仍用 t0……同 2）
- **状态**: ☐

### P1.2 `sleep` 的 wfi 提前返回路径残留 Blocked 状态

- **文件**: `src/scheduler.rs` — `sleep()` / `sleep_wfi()`
- **当前**: `sleep()` 置 CURRENT 任务 `state = Blocked` + `wake_tick = 未来时刻` 后开 SIE 进入 `wfi`。wfi 是 hint：若中断已挂起则**直接完成（不阻塞）**→ `sleep()` 正常返回，但任务 state 仍是 `Blocked` 且 wake_tick 在未来。
- **影响**: 下次抢占时 `scheduler()` 看到 `state == Blocked` 且未到期 → 误 park 进 `SLEEP_LIST`，任务"假装睡眠"直到 wake_tick 才被唤醒。实践中挂起中断通常在 wfi 后的下一条指令边界立即 trap（任务被抢占 → 正常入睡眠列表），掩盖了此路径；但若中断在 trap 交付前被清除，任务会带着 `Blocked` 状态继续运行。语义不严谨，属潜伏竞态。
- **目标**: sleep 返回路径保证 state 一致。候选方案：
  1. **resume 统一重置** — `wake_task` 本就置 `Ready`；在 `sleep()` 返回前检测无法自证是否被 park……需改 `sleep` 的唤醒协议（如 resume 点重置 CURRENT state）
  2. **wfi 后校验** — `sleep_wfi()` 返回后若仍处运行态（未被 park），主动把 CURRENT state 置回 `Ready`
- **状态**: ☐

---

## 备注

- 两项均已在 `ROADMAP.md` 的「已知限制」之外单独记录，避免与路线图功能项混淆。
- 修复时建议按 P1.2 → P1.1 顺序（前者纯 scheduler 状态逻辑、可独立测试；后者动 naked asm 入口，需 QEMU 全路径回归）。
