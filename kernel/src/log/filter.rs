// 级别与过滤 — 日志级别定义 + 三层过滤 + console 级别
//
// 过滤三层：
//   1. 编译期：COMPILE_MAX_LEVEL 以上的调用在宏展开中被移除（零开销）
//   2. 运行时全局：set_max_level() 动态调整，默认 Info
//   3. 运行时模块级：set_module_rules() 按模块路径前缀覆盖全局级别（最长前缀匹配）
//
// console 级别（CONSOLE_LEVEL，默认 Info）：独立于 ring 记录级别——ring 记录所有
// 通过上述过滤的日志，console 仅显示级别 ≤ console_level 的（Linux console_loglevel
// 对应物）。/dev/log 可读到 console 未显示的细日志。
//
// 运行时级别用 AtomicU8 无锁读写；模块规则表用 OnceLock 写一次读多次（日志路径无锁）。

use core::sync::atomic::{AtomicU8, Ordering};

use crate::lock::OnceLock;

/// 日志级别（按严重程度递增）
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[repr(u8)]
pub enum LogLevel {
    Error = 0,
    Warn = 1,
    Info = 2,
    Debug = 3,
    Trace = 4,
}

/// 编译期最高日志级别——高于此级别的调用在宏展开中被编译器移除。
///
/// 发布构建时可改为 `LogLevel::Info` 以完全消除 debug/trace 代码。
pub const COMPILE_MAX_LEVEL: LogLevel = LogLevel::Trace;

/// 运行时最高日志级别（默认 Info：Error + Warn + Info 可见）
static RUNTIME_LEVEL: AtomicU8 = AtomicU8::new(LogLevel::Info as u8);

/// 设置运行时最高日志级别（全局默认；模块级规则可覆盖）
pub fn set_max_level(level: LogLevel) {
    RUNTIME_LEVEL.store(level as u8, Ordering::Relaxed);
}

/// 获取当前运行时最高日志级别
pub fn max_level() -> LogLevel {
    level_from_u8(RUNTIME_LEVEL.load(Ordering::Relaxed))
}

/// console 显示的最高日志级别（默认 Info）——Linux `console_loglevel` 对应物。
///
/// ring buffer 记录所有通过 [`set_max_level`] 过滤的日志；console 仅显示
/// 级别 ≤ 此值的（ring 与 console 级别分离，`/dev/log` 可读到 console 未显示的细日志）。
static CONSOLE_LEVEL: AtomicU8 = AtomicU8::new(LogLevel::Info as u8);

/// 设置 console 显示的最高日志级别（默认 Info：Error + Warn + Info 上 console）。
///
/// main() 把 console 降到 Warn 实现启动日志静默（boot info 不刷屏，ring 仍
/// 记录全量，/dev/log 可查）；set_max_level(Debug/Trace) 调试场景下也可单独
/// 压 console 噪音。
pub fn set_console_level(level: LogLevel) {
    CONSOLE_LEVEL.store(level as u8, Ordering::Relaxed);
}

/// 获取当前 console 显示的最高日志级别。
pub fn console_level() -> LogLevel {
    level_from_u8(CONSOLE_LEVEL.load(Ordering::Relaxed))
}

/// 该级别是否应输出到 console（`level <= console_level()`）。
pub(super) fn console_shows(level: LogLevel) -> bool {
    (level as u8) <= console_level() as u8
}

/// 将原始值映射回 LogLevel（值域受 set_max_level/规则表约束，恒合法）
fn level_from_u8(v: u8) -> LogLevel {
    match v {
        0 => LogLevel::Error,
        1 => LogLevel::Warn,
        2 => LogLevel::Info,
        3 => LogLevel::Debug,
        4 => LogLevel::Trace,
        // 不可达（值域受 set_max_level/规则表约束）；防御性回落，避免越界 panic
        _ => LogLevel::Trace,
    }
}

/// 模块级过滤规则 — 按模块路径前缀控制日志级别。
pub struct ModuleRule {
    /// 模块路径前缀（`module_path!()` 值，如 `"driver::serial"`）
    pub prefix: &'static str,
    /// 该模块下生效的最高日志级别
    pub level: LogLevel,
}

/// 模块级过滤规则表（boot 时设置一次，之后只读——日志路径无锁）。
static MODULE_RULES: OnceLock<&'static [ModuleRule]> = OnceLock::new();

/// 设置模块级过滤规则。
///
/// 规则按最长前缀匹配：日志的 `module_path!()` 命中多条规则时取前缀最长者；
/// 无命中则回落全局 `set_max_level()` 的默认级别。
pub fn set_module_rules(rules: &'static [ModuleRule]) {
    MODULE_RULES.set(rules).ok();
}

/// 计算模块的有效最高级别（模块规则命中取规则值，否则取全局默认）。
pub(super) fn effective_level(module: &str) -> LogLevel {
    // module_path!() 以 crate 名为前缀（如 "atom::memory::allocator::frame"），
    // 规则前缀与 crate 名无关——匹配前剥离首段。
    let module = module
        .split_once("::")
        .map(|(_, rest)| rest)
        .unwrap_or(module);
    let mut best: Option<(usize, u8)> = None;
    if let Some(rules) = MODULE_RULES.get() {
        for rule in rules.iter() {
            if module == rule.prefix
                || module
                    .strip_prefix(rule.prefix)
                    .is_some_and(|rest| rest.starts_with("::"))
            {
                let len = rule.prefix.len();
                if best.map_or_else(|| true, |(bl, _)| len > bl) {
                    best = Some((len, rule.level as u8));
                }
            }
        }
    }
    best.map_or_else(max_level, |(_, l)| level_from_u8(l))
}
