// 平台配置 — 从 DTB 探测或使用硬编码回退
//
// 引导流程：
//   1. `platform::init(dtb_ptr)` — 尝试解析 DTB，失败则缓存诊断并回退
//   2. `platform::config()` — 此后可安全调用，返回静态引用
//   3. `platform::report_diag()` — 日志就绪后输出诊断信息
//
// DTB 探测在 Phase 1（allocator / MMU）之前运行，解码期间仅使用栈变量。

use crate::dtb;
use crate::lock::BareLock;

/// RISC-V 页大小（所有 Sv 分页模式通用）。
pub const PAGE_SIZE: usize = 4096;

/// QEMU virt 平台默认配置（DTB 不可用时的回退值）。
pub mod qemu_virt {
    pub const DRAM_BASE: usize = 0x8000_0000;
    pub const DRAM_SIZE: usize = 8 * 1024 * 1024;  // 8 MiB
    pub const UART_BASE: usize = 0x1000_0000;
    pub const UART_IRQ: u32   = 10;
    pub const CLINT_BASE: usize = 0x0200_0000;
    pub const PLIC_BASE: usize  = 0x0C00_0000;
    pub const PLIC_SIZE: usize  = 0x30_0000;        // 3 MiB, 覆盖 S-mode 上下文
    pub const TIMEBASE_FREQ: u64 = 10_000_000;       // 10 MHz
}

/// 平台硬件配置（只读，初始化后不可变）。
#[derive(Debug)]
pub struct PlatformConfig {
    /// DRAM 物理基址
    pub dram_base: usize,
    /// DRAM 总大小 (bytes)
    pub dram_size: usize,
    /// NS16550A UART MMIO 基址
    pub uart_base: usize,
    /// UART PLIC 中断号
    pub uart_irq: u32,
    /// CLINT MMIO 基址
    pub clint_base: usize,
    /// PLIC MMIO 基址
    pub plic_base: usize,
    /// PLIC MMIO 区域大小（须覆盖 S-mode context）
    pub plic_size: usize,
    /// 定时器频率 (Hz)，用于 CLINT ticks_per_sec
    pub timebase_freq: u64,
    /// 固件保留的 DRAM 起始大小 — 由 `_kernel_start - dram_base` 运行时推导。
    pub firmware_reserve: usize,
    /// 内核栈保留大小（从 DRAM 末尾向下预留）。
    pub stack_reserve: usize,
    /// CPU / hart 数量。
    pub hart_count: usize,
}

impl PlatformConfig {
    /// 用 QEMU virt 硬编码默认值构造。
    fn default_qemu_virt() -> Self {
        Self {
            dram_base: qemu_virt::DRAM_BASE,
            dram_size: qemu_virt::DRAM_SIZE,
            uart_base: qemu_virt::UART_BASE,
            uart_irq: qemu_virt::UART_IRQ,
            clint_base: qemu_virt::CLINT_BASE,
            plic_base: qemu_virt::PLIC_BASE,
            plic_size: qemu_virt::PLIC_SIZE,
            timebase_freq: qemu_virt::TIMEBASE_FREQ,
            firmware_reserve: 0,  // 由 init() 中链接符号推导覆盖
            stack_reserve: 32 * 1024,
            hart_count: 1,  // QEMU virt 默认单核
        }
    }
}

// ── 全局配置 ────────────────────────────────────────────────

/// 全局平台配置 — 引导早期写入一次，此后只读。
static mut PLATFORM: Option<PlatformConfig> = None;

/// DTB 解析诊断信息缓存 — probe 阶段填充，Phase 2 后输出。
/// 仅在引导期任务上下文访问，从不被中断处理程序碰，故用 BareLock。
static DTB_DIAG: BareLock<Option<&'static str>> = BareLock::new(None);

/// 探测并初始化平台配置。
///
/// 首选从 DTB 解析；若 `dtb_ptr` 为 0 或解析失败则回退到 QEMU virt 默认值，
/// 同时缓存诊断信息供后续日志输出。
///
/// # Safety
///
/// 必须在引导早期、单 hart 下调用恰好一次，在任何读取 `config()` 之前。
pub unsafe fn init(dtb_ptr: usize) {
    let mut cfg = if dtb_ptr != 0 {
        match probe_dtb(dtb_ptr) {
            Ok(cfg) => {
                *DTB_DIAG.lock() = None;
                cfg
            }
            Err(e) => {
                // 缓存诊断信息，在 init::run() Phase 2 后由 report_diag() 输出
                *DTB_DIAG.lock() = Some(e.description());
                PlatformConfig::default_qemu_virt()
            }
        }
    } else {
        PlatformConfig::default_qemu_virt()
    };

    // 从链接符号推导固件保留大小（DRAM_BASE 到 _kernel_start 之间）
    extern "C" {
        static _kernel_start: u8;
    }
    cfg.firmware_reserve = &raw const _kernel_start as usize - cfg.dram_base;

    PLATFORM = Some(cfg);
}

/// 获取平台配置的静态引用。
///
/// # Panics
///
/// 若 `init()` 未被调用。
#[inline]
pub fn config() -> &'static PlatformConfig {
    // SAFETY: 引导早期单 hart 写入，此后只读；中断使能前已写入完毕。
    // 使用 addr_of! 避免 static_mut_refs 警告。
    unsafe {
        (*core::ptr::addr_of!(PLATFORM))
            .as_ref()
            .expect("platform::init() not called")
    }
}

/// 输出缓存的 DTB 诊断信息（在日志初始化后调用）。
pub fn report_diag() {
    // SAFETY: 仅引导期任务上下文调用，不会从中断上下文争用 DTB_DIAG。
    let mut diag = unsafe { DTB_DIAG.lock() };
    if let Some(msg) = *diag {
        crate::warn!("DTB parse failed, using fallback: {}", msg);
        *diag = None;
    }
}

// ── DTB 探测 ────────────────────────────────────────────────

/// 探测错误类型——保留原始错误以便诊断。
#[derive(Debug)]
enum ProbeError {
    Header(dtb::header::Error),
    Walk(dtb::walk::WalkError),
    MissingMemory(&'static str),
}

impl ProbeError {
    fn description(&self) -> &'static str {
        match self {
            ProbeError::Header(e) => match e {
                dtb::header::Error::BadMagic(_) => "bad FDT magic",
                dtb::header::Error::BadVersion(_) => "unsupported FDT version",
                dtb::header::Error::OutOfBounds { .. } => "FDT block out of bounds",
            },
            ProbeError::Walk(e) => match e {
                dtb::walk::WalkError::Truncated => "FDT structure truncated",
                dtb::walk::WalkError::Unbalanced => "FDT nodes unbalanced",
                dtb::walk::WalkError::BadStringOffset(_) => "FDT string offset out of bounds",
            },
            ProbeError::MissingMemory(s) => s,
        }
    }
}

impl From<dtb::header::Error> for ProbeError {
    fn from(e: dtb::header::Error) -> Self { ProbeError::Header(e) }
}

impl From<dtb::walk::WalkError> for ProbeError {
    fn from(e: dtb::walk::WalkError) -> Self { ProbeError::Walk(e) }
}

/// `probe_dtb` 允许的最大节点嵌套深度。
const MAX_DEPTH: usize = 8;

/// 当前节点识别标记。
#[derive(Clone, Copy, PartialEq, Eq)]
enum NodeKind {
    Memory,
    Uart,
    Clint,
    Plic,
}

/// 检查 compatible 属性（null-separated 字符串列表）是否包含 target。
fn compatible_contains(compat: &[u8], target: &str) -> bool {
    let mut start = 0usize;
    for (i, &b) in compat.iter().enumerate() {
        if b == 0 {
            if start < i {
                if let Ok(s) = core::str::from_utf8(&compat[start..i]) {
                    if s == target {
                        return true;
                    }
                }
            }
            start = i + 1;
        }
    }
    // 处理末尾无 null 的情况
    if start < compat.len() {
        if let Ok(s) = core::str::from_utf8(&compat[start..]) {
            return s == target;
        }
    }
    false
}

/// DTB 探测状态机。
///
/// 通过缓冲 `pending_reg` / `pending_interrupt` 解决属性顺序依赖：
/// - `reg` / `interrupts` 到达时暂存
/// - `compatible` / `device_type` 到达时设置 `current_kind`
/// - 节点离开 (`finish_node`) 时消费缓冲值
struct ProbeState {
    /// addr_cells[d] — 适用于深度 d 的子节点
    addr_cells: [u32; MAX_DEPTH],
    /// size_cells[d] — 适用于深度 d 的子节点
    size_cells: [u32; MAX_DEPTH],
    /// 当前深度（0 = 根节点）
    depth: usize,

    /// 当前节点类型 — 由 device_type 或 compatible 设置
    current_kind: Option<NodeKind>,

    /// 缓冲 reg 属性值（base, size）— 在 finish_node 中根据 current_kind 消费
    pending_reg: Option<(u64, u64)>,
    /// 缓冲 interrupts 属性值 — 在 finish_node 中根据 current_kind 消费
    pending_interrupt: Option<u32>,

    // ── 探测结果 ──
    dram_base: u64,
    dram_size: u64,
    uart_base: u64,
    uart_irq: u32,
    clint_base: u64,
    plic_base: u64,
    plic_size: u64,
    timebase_freq: u64,

    has_memory: bool,
    has_uart_base: bool,
    has_uart_irq: bool,
    has_clint: bool,
    has_plic: bool,
    has_timebase: bool,
}

impl ProbeState {
    fn new() -> Self {
        Self {
            addr_cells: [2; MAX_DEPTH],
            size_cells: [2; MAX_DEPTH],
            depth: 0,
            current_kind: None,
            pending_reg: None,
            pending_interrupt: None,
            dram_base: 0,
            dram_size: 0,
            uart_base: 0,
            uart_irq: 0,
            clint_base: 0,
            plic_base: 0,
            plic_size: 0,
            timebase_freq: 0,
            has_memory: false,
            has_uart_base: false,
            has_uart_irq: false,
            has_clint: false,
            has_plic: false,
            has_timebase: false,
        }
    }

    /// 进入节点：继承父节点的 addr_cells/size_cells，复位节点状态。
    fn enter_node(&mut self) {
        if self.depth >= MAX_DEPTH {
            return;
        }
        if self.depth + 1 < MAX_DEPTH {
            self.addr_cells[self.depth + 1] = self.addr_cells[self.depth];
            self.size_cells[self.depth + 1] = self.size_cells[self.depth];
        }
        self.depth += 1;
        self.current_kind = None;
        self.pending_reg = None;
        self.pending_interrupt = None;
    }

    /// 离开节点：根据 current_kind 消费缓冲的 reg/interrupts。
    fn finish_node(&mut self) {
        if self.depth == 0 {
            return;
        }

        // 消费缓冲的 reg
        if let Some((base, size)) = self.pending_reg.take() {
            match self.current_kind {
                Some(NodeKind::Memory) => {
                    self.dram_base = base;
                    self.dram_size = size;
                    self.has_memory = true;
                }
                Some(NodeKind::Uart) => {
                    self.uart_base = base;
                    self.has_uart_base = true;
                }
                Some(NodeKind::Clint) => {
                    self.clint_base = base;
                    self.has_clint = true;
                }
                Some(NodeKind::Plic) => {
                    self.plic_base = base;
                    self.plic_size = size;
                    self.has_plic = true;
                }
                None => {} // 未识别节点，丢弃 reg
            }
        }

        // 消费缓冲的 interrupt
        if let Some(irq) = self.pending_interrupt.take() {
            if self.current_kind == Some(NodeKind::Uart) {
                self.uart_irq = irq;
                self.has_uart_irq = true;
            }
        }

        self.depth -= 1;
    }

    /// 处理属性：设置 current_kind 或缓存 reg/interrupts。
    fn property(&mut self, name: &str, value: &[u8]) {
        match name {
            // ── 地址上下文（影响子节点的 reg 解析）─────────
            "#address-cells" => {
                if let Some(v) = dtb::cell::read_cell_u32(value, 0) {
                    if self.depth < MAX_DEPTH {
                        self.addr_cells[self.depth] = v;
                    }
                }
            }
            "#size-cells" => {
                if let Some(v) = dtb::cell::read_cell_u32(value, 0) {
                    if self.depth < MAX_DEPTH {
                        self.size_cells[self.depth] = v;
                    }
                }
            }

            // ── 节点类型识别 ────────────────────────────
            "device_type" => {
                if let Ok(s) = core::str::from_utf8(value) {
                    if s.trim_end_matches('\0') == "memory" {
                        self.current_kind = Some(NodeKind::Memory);
                    }
                }
            }
            "compatible" => {
                if compatible_contains(value, "ns16550a") || compatible_contains(value, "ns16550") {
                    self.current_kind = Some(NodeKind::Uart);
                } else if compatible_contains(value, "riscv,clint0")
                    || compatible_contains(value, "sifive,clint0")
                {
                    self.current_kind = Some(NodeKind::Clint);
                } else if compatible_contains(value, "riscv,plic0")
                    || compatible_contains(value, "sifive,plic0")
                {
                    self.current_kind = Some(NodeKind::Plic);
                }
            }

            // ── 寄存器地址（缓存到 finish_node 消费）───
            "reg" => {
                let ac = self.parent_addr_cells();
                let sc = self.parent_size_cells();
                let mut off = 0usize;
                if let Some(base) = dtb::cell::read_cells(value, &mut off, ac) {
                    if let Some(size) = dtb::cell::read_cells(value, &mut off, sc) {
                        self.pending_reg = Some((base, size));
                    }
                }
            }

            // ── 中断号（缓存到 finish_node 消费）───────
            "interrupts" => {
                if let Some(irq) = dtb::cell::read_cell_u32(value, 0) {
                    self.pending_interrupt = Some(irq);
                }
            }

            // ── 定时器频率 ──────────────────────────────
            "timebase-frequency" => {
                if let Some(freq) = dtb::cell::read_cell_u32(value, 0) {
                    self.timebase_freq = freq as u64;
                    self.has_timebase = true;
                }
            }

            _ => {}
        }
    }

    /// 当前深度的父节点 addr_cells（用于解析 reg）。
    fn parent_addr_cells(&self) -> u32 {
        if self.depth <= 1 { 2 } else { self.addr_cells[self.depth - 2] }
    }

    /// 当前深度的父节点 size_cells（用于解析 reg）。
    fn parent_size_cells(&self) -> u32 {
        if self.depth <= 1 { 2 } else { self.size_cells[self.depth - 2] }
    }

    /// 从 QEMU virt 默认值出发，用探测值覆盖已发现字段。
    fn finish(self) -> Result<PlatformConfig, ProbeError> {
        if !self.has_memory {
            return Err(ProbeError::MissingMemory("no /memory node found in DTB"));
        }

        let mut cfg = PlatformConfig::default_qemu_virt();
        cfg.dram_base = self.dram_base as usize;
        cfg.dram_size = self.dram_size as usize;
        if self.has_uart_base { cfg.uart_base = self.uart_base as usize; }
        if self.has_uart_irq { cfg.uart_irq = self.uart_irq; }
        if self.has_clint { cfg.clint_base = self.clint_base as usize; }
        if self.has_plic {
            cfg.plic_base = self.plic_base as usize;
            cfg.plic_size = self.plic_size as usize;
        }
        if self.has_timebase { cfg.timebase_freq = self.timebase_freq; }
        Ok(cfg)
    }
}

/// 从 DTB 物理地址解析平台配置。
///
/// 使用 `FdtIter` 迭代器消费 token 流，将属性缓存在 `ProbeState` 中，
/// 在节点离开时根据已识别的设备类型消费。
///
/// # Safety
///
/// `dtb_ptr` 必须指向有效的 FDT 头部。
unsafe fn probe_dtb(dtb_ptr: usize) -> Result<PlatformConfig, ProbeError> {
    let header = dtb::FdtHeader::validate(dtb_ptr)?;
    let mut iter = dtb::FdtIter::new(header);
    let mut state = ProbeState::new();

    loop {
        let token = match iter.next() {
            Some(Ok(t)) => t,
            Some(Err(e)) => return Err(e.into()),
            None => break,
        };

        match token {
            dtb::Token::BeginNode(name) => {
                // 跳过 /chosen 子树以减少遍历开销
                if name == "chosen" {
                    iter.skip_subtree();
                    continue;
                }
                state.enter_node();
            }
            dtb::Token::EndNode => {
                state.finish_node();
            }
            dtb::Token::Property { name, value } => {
                state.property(name, value);
            }
            dtb::Token::End => break,
            dtb::Token::Nop => {}
        }
    }

    state.finish()
}
