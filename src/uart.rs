// NS16550A UART（QEMU virt 固定地址 0x1000_0000）

const UART_BASE: usize = 0x10000000;
const LSR_THRE: u8 = 0x20;

/// 发送单字节（阻塞）
#[inline]
pub fn putc(c: u8) {
    let uart = UART_BASE as *mut u8;
    while unsafe { uart.add(5).read_volatile() } & LSR_THRE == 0 {}
    unsafe { uart.write_volatile(c) }
}

/// 发送字符串（\n → \r\n）
pub fn puts(s: &str) {
    for &b in s.as_bytes() {
        if b == b'\n' {
            putc(b'\r');
        }
        putc(b);
    }
}
