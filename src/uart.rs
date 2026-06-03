// NS16550A UART（QEMU virt 固定地址 0x1000_0000）

pub const UART_BASE: usize = 0x10000000;

const UART_CLOCK: u32 = 11_059_200;
const BAUD_RATE: u32 = 115_200;

const RBR: usize = 0x0; // 读 = 接收缓冲（DLAB=0）
const THR: usize = 0x0; // 写 = 发送保持（DLAB=0）
const DLL: usize = 0x0; // 写/读 = 除数锁存低字节（DLAB=1）
const DLM: usize = 0x1; // 写/读 = 除数锁存高字节（DLAB=1）
const IER: usize = 0x1;
const FCR: usize = 0x2; // 写 = FCR
const IIR: usize = 0x2; // 读 = IIR
const LCR: usize = 0x3;
const MCR: usize = 0x4;
const LSR: usize = 0x5;
const MSR: usize = 0x6;
const SCR: usize = 0x7;

const LCR_DLAB: u8 = 0x80;

const FCR_ENABLE:  u8 = 0x01;
const FCR_CLR_RX:  u8 = 0x02;
const FCR_CLR_TX:  u8 = 0x04;
const FCR_TRIG_14: u8 = 0xC0;

const LSR_THRE: u8 = 0x20;
const LSR_DR:   u8 = 0x01;

pub struct Uart {
    base: *mut u8,
}

// 单核嵌入式环境，裸指针安全
unsafe impl Sync for Uart {}

impl Uart {
    /// 从基地址创建实例（不初始化硬件）
    pub const fn new(base: usize) -> Self {
        Self { base: base as *mut u8 }
    }

    /// 初始化 UART 硬件：禁中断 → 使能 FIFO → 设波特率 → 8N1
    pub fn init(&self) {
        let divisor: u16 = (UART_CLOCK / (16 * BAUD_RATE)) as u16;
        let dll = (divisor & 0xFF) as u8;
        let dlh = ((divisor >> 8) & 0xFF) as u8;

        unsafe {
            // 禁用中断
            self.write_reg(IER, 0x00);

            // 启用 FIFO，清空收发缓冲区
            self.write_reg(FCR, FCR_ENABLE | FCR_CLR_RX | FCR_CLR_TX | FCR_TRIG_14);

            // 打开 DLAB → 写除数锁存器
            self.write_reg(LCR, LCR_DLAB);
            self.write_reg(DLL, dll);
            self.write_reg(DLM, dlh);

            // 清除 DLAB，8N1
            self.write_reg(LCR, 0x03);
        }
    }

    /// 发送单字节（阻塞，等待发送缓冲区空）
    #[inline]
    pub fn putc(&self, c: u8) {
        while unsafe { self.read_reg(LSR) } & LSR_THRE == 0 {}
        unsafe { self.write_reg(THR, c) }
    }

    /// 发送字符串（`\n` → `\r\n`）
    pub fn puts(&self, s: &str) {
        for &b in s.as_bytes() {
            if b == b'\n' {
                self.putc(b'\r');
            }
            self.putc(b);
        }
    }

    /// 接收单字节（阻塞）
    #[inline]
    pub fn getc(&self) -> u8 {
        while unsafe { self.read_reg(LSR) } & LSR_DR == 0 {}
        unsafe { self.read_reg(RBR) }
    }

    /// 尝试接收单字节（非阻塞），无数据返回 `None`
    #[inline]
    pub fn try_getc(&self) -> Option<u8> {
        if unsafe { self.read_reg(LSR) } & LSR_DR != 0 {
            Some(unsafe { self.read_reg(RBR) })
        } else {
            None
        }
    }

    #[inline]
    unsafe fn read_reg(&self, offset: usize) -> u8 {
        self.base.add(offset).read_volatile()
    }

    #[inline]
    unsafe fn write_reg(&self, offset: usize, val: u8) {
        self.base.add(offset).write_volatile(val)
    }
}

pub static UART: Uart = Uart::new(UART_BASE);
