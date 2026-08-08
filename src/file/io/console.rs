// file/io/console — 输出目标决策（唯一输出决策点）
//
// 分层：device（设备表）← console（输出决策 + 串行化）← print（格式化宏）
//       ← log / 各模块
//
// console 只回答一个问题："格式化结果打向哪"。
//   - 设备表非空（至少一个 console 设备注册）→ preferred 设备，带 OUT 锁（S-mode）
//   - 设备表空（boot 早期，allocator 未就绪）→ sbi 无锁直写（M-mode）
// "表空即早期"：console 设备注册全部需要 allocator（io::uart::register 的
// Box::leak/format!），故 allocator 未就绪时表必然为空，表空自然回落 sbi——
// 无需显式阶段状态机（Early/Ready 的职责由"表空"这一数据结构本身表达）。
//
// 无锁输出（panic / lockdep）不经本模块——调用方直接 `sbi::mprintln!`。

use core::fmt;

use crate::lock::SpinLock;

/// 输出串行化锁 — 关中断，保证唯一写者。与设备表锁不同时持有
/// （`preferred_writer` 内部取表后即释放，见 device.rs），锁序无嵌套。
static OUT: SpinLock<()> = SpinLock::new(());

/// 格式化输出 — 决策目标：表空 → sbi 无锁；非空 → preferred 设备（带锁）。
pub fn write(args: fmt::Arguments) {
    match super::device::preferred_writer() {
        Some(w) => {
            let _guard = OUT.lock();
            let _ = w.write_fmt(args);
        }
        None => crate::sbi::write_fmt(args),
    }
}

/// 原始字节输出 — 同上（字节语义，非 UTF-8 原样直写，`\n` → `\r\n` 由目标转换）。
pub fn write_bytes(bytes: &[u8]) {
    match super::device::preferred_writer() {
        Some(w) => {
            let _guard = OUT.lock();
            w.write_bytes(bytes);
        }
        None => crate::sbi::write_bytes(bytes),
    }
}

/// 输出到指定设备（显式路由）— 设备不存在静默丢弃。调试专用（tprint!/tprintln!）。
pub(crate) fn write_to(name: &'static str, args: fmt::Arguments) {
    let _guard = OUT.lock();
    let Some(w) = super::device::find_writer(name) else {
        return;
    };
    let _ = w.write_fmt(args);
}
