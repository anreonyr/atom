.section .text
.globl _start

.equ UART_BASE, 0x10000000
.equ UART_LSR, 0x05

_start:
    # UART 基地址（QEMU virt 固定）
    li   t0, UART_BASE

    # 等待发送器就绪（LSR bit5）
wait:
    lb   t1, UART_LSR(t0)        # 读 LSR 寄存器（偏移 0x05）
    andi t1, t1, 0x20        # 取 bit5
    beqz t1, wait            # 未就绪则循环

    # 发送字符 'A'
    li   t1, 0x41            # ASCII 'A'
    sb   t1, 0x00(t0)        # 写入 THR（偏移 0x00）

    # 程序结束，死循环
loop:
    j    loop
