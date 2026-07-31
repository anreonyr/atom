# QEMU gdbstub 会话模板
#
# 用法（先起 QEMU）：
#   终端 1: qemu-system-riscv64 -machine virt -bios default \
#              -kernel target/riscv64gc-unknown-none-elf/debug/atom \
#              -nographic -s -S
#   终端 2: gdb -x scripts/debug.gdb target/riscv64gc-unknown-none-elf/debug/atom
#
# 或在本会话内手工执行: target remote :1234
# 详细说明见 docs/debugging.md §3

# 连接 gdbstub（QEMU -s 监听 :1234）
target remote :1234

# 关闭交互打扰
set pagination off
set confirm off

# 起步快照：当前寄存器（暂停在复位向量 0x1000，OpenSBI 之前）
info registers

# 内核入口断点：让 OpenSBI 跑完、停在内核 _start（0x80200000，符号已加载）
break *0x80200000

# ── watchpoint 示例（"谁写坏了内存"）────────────────────────
# 内核 DRAM 恒等映射（VA==PA），watch 直接监听物理地址。
# 复现时对目标地址取消注释，continue 后命中即写入者。
# watch *0x80201000

continue
