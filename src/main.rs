#![no_std]
#![no_main]

use core::panic::PanicInfo;

const UART_BASE: usize = 0x10000000;
const LSR_THRE: u8 = 0x20;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let uart = UART_BASE as *mut u8;
    let message = "Hello\n";

    // 等待发送器就绪（LSR bit5）
    while unsafe { uart.add(5).read_volatile() } & LSR_THRE == 0 {}

    // 发送字符
    unsafe {
        for c in message.bytes() {
            uart.write_volatile(c)
        }
    };

    // 死循环
    loop {}
}

#[panic_handler]
fn panic_handler(_info: &PanicInfo) -> ! {
    loop {}
}
