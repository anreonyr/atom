// CPU / hart 抽象
//
// 提供当前 hart 标识。单 hart 阶段返回固定值 0；多 hart 启动协议就绪后，
// 改为从 tp 寄存器（OpenSBI 约定 tp = hartid）或 mhartid 读取。
//
// RelLock 等需要区分持有者 hart 的锁依赖此接口。

/// hart 标识符。
///
/// 0 是合法 hart id；锁实现内部用 `id + 1` 之类映射区分"空闲"，
/// 具体见各锁实现。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct HartId(usize);

impl HartId {
    /// 构造一个 hart 标识。
    pub const fn new(id: usize) -> Self {
        HartId(id)
    }

    /// 返回底层数值 id。
    pub const fn as_usize(self) -> usize {
        self.0
    }
}

/// 获取当前 hart 的标识。
///
/// # Safety
///
/// 当前为单 hart stub，恒返回 `HartId(0)`。多 hart 阶段需保证
/// hart 启动协议已就位（tp / mhartid 可信），否则返回值无意义。
// TODO: 多 hart 时从 tp / mhartid 读取真实 hartid
#[inline(always)]
pub unsafe fn hart_id() -> HartId {
    HartId::new(0)
}
