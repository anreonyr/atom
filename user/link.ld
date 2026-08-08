/* 用户程序链接脚本 — 静态 ET_EXEC，装载基址 0x10000（与内核 USER_CODE_VA 一致） */
OUTPUT_ARCH(riscv)
ENTRY(_start)

SECTIONS
{
  . = 0x10000;

  .text : { *(.text._start) *(.text*) }
  .rodata : { *(.rodata*) *(.srodata*) *(.srodata.cst*) }
  .data : { *(.data*) *(.sdata*) }
  .bss (NOLOAD) : { *(.sbss*) *(.bss*) *(COMMON) }

  /* 非 alloc 元数据：loader 只认 PT_LOAD，丢弃避免进段引发重叠 */
  /DISCARD/ : { *(.riscv.attributes) *(.note.gnu.property) }

  . = ALIGN(16);
  _end = .;
}
