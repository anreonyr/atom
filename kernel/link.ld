OUTPUT_ARCH(riscv)
ENTRY(_start)

SECTIONS
{
  . = 0x80200000;

  _kernel_start = .;

  .text : {
    *(.text._start)
    *(.text*)
  }
  .rodata : {
    *(.rodata*)
  }
  .data : {
    *(.data*)
  }
  .bss : {
    *(.bss*)
  }

  _kernel_end = .;

  . = ALIGN(16);

  _bump_base = .;

  /* 专用 trap 栈：trap_handler/scheduler 跑在此处。
   * 位于恒等映射区，切换地址空间后仍可访问——任务栈是 per-task 窗口
   * （0xC0000000），switch_space 后当前栈 VA 会别名到新任务栈，trap 处理
   * 绝不能依赖当前任务栈。 */
  _trap_stack_bottom = .;
  . += 16K;
  _trap_stack_top = .;

  _early_stack_bottom = .;
  . += 4K;
  _early_stack_top = .;

}
