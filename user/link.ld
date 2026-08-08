/* 用户程序链接脚本 — 静态 ET_EXEC，装载基址 0x10000（与内核 USER_CODE_VA 一致） */
OUTPUT_ARCH(riscv)
ENTRY(_start)

SECTIONS
{
  . = 0x10000;

  .text : { *(.text._start) *(.text*) }
  /* 段页分离：各段 ALIGN(4096) 起始——否则相邻 PT_LOAD 共享页（text 尾 +
     rodata 头落同页），loader 逐段映射第二次 map 报 AlreadyMapped */
  .rodata : ALIGN(4096) { *(.rodata*) *(.srodata*) *(.srodata.cst*) }
  .data : ALIGN(4096) { *(.data*) *(.sdata*) }
  .bss (NOLOAD) : ALIGN(4096) { *(.sbss*) *(.bss*) *(COMMON) }

  /* 非 alloc 元数据：loader 只认 PT_LOAD，丢弃避免进段引发重叠。
     .eh_frame 为 Rust 函数的展开表（panic=abort 不需要），不丢弃会与 .text
     重叠（无 SECTIONS 归属 → 落到当前地址）。 */
  /DISCARD/ : {
    *(.riscv.attributes)
    *(.note.gnu.property)
    *(.eh_frame*)
    *(.gcc_except_table*)
    *(.comment)
    *(.rela.*)
  }

  . = ALIGN(16);
  _end = .;
}
