// MMIO 寄存器访问抽象
//
// 通过关联类型 `T` 声明寄存器宽度，提供默认的 volatile 读写方法。

/// MMIO 寄存器读写接口
///
/// 实现者只需声明 `type T`（寄存器类型）和 `base()`（MMIO 基址），
/// 即可获得 `read()` / `write()` 方法。
///
/// # 示例
///
/// ```ignore
/// impl Mmio for Uart {
///     type T = u8;
///     fn base(&self) -> *mut u8 { self.base }
/// }
/// ```
pub trait Mmio {
    /// 寄存器访问类型（u8 / u16 / u32 / u64）
    type T: Copy;

    /// MMIO 区域基址指针
    fn base(&self) -> *mut u8;

    /// 从 `offset` 处 volatile 读取一个寄存器
    #[inline]
    unsafe fn read(&self, offset: usize) -> Self::T {
        (self.base().add(offset) as *const Self::T).read_volatile()
    }

    /// 向 `offset` 处 volatile 写入一个寄存器
    #[inline]
    unsafe fn write(&self, offset: usize, val: Self::T) {
        (self.base().add(offset) as *mut Self::T).write_volatile(val)
    }
}
