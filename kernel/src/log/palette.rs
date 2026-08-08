// ANSI 调色板 — 日志输出全部颜色集中定义，使用处只引用语义名
//
// Color::code() 是本模块唯一的转义序列来源（裸 `\x1b` 不出现于其他模块）。
// LEVEL_META 把级别 ↔ (颜色, 标签) 绑定：console 行（level_style）与
// ring 快照（label_str）共用同一来源，新增级别时必须同步扩展。

use super::LogLevel;

/// ANSI 颜色 — 日志输出的全部前景色（转义序列唯一来源）。
#[derive(Clone, Copy)]
pub(super) enum Color {
    /// 红（ERROR 级别）
    Red,
    /// 黄（WARN 级别）
    Yellow,
    /// 绿（INFO 级别）
    Green,
    /// 青（DEBUG 级别）
    Cyan,
    /// 紫（TRACE 级别）
    Magenta,
    /// 灰（次级信息：模块、file:line、时间戳、消息体）
    Gray,
}

impl Color {
    /// ANSI SGR 转义序列（含 `ESC[` 与属性码，不含复位）。
    pub(super) const fn code(self) -> &'static str {
        match self {
            Color::Red => "\x1b[1;31m",
            Color::Yellow => "\x1b[1;33m",
            Color::Green => "\x1b[32m",
            Color::Cyan => "\x1b[36m",
            Color::Magenta => "\x1b[35m",
            Color::Gray => "\x1b[90m",
        }
    }
}

/// ANSI 属性复位（调色板共用）。
pub(super) const RESET: &str = "\x1b[0m";

/// 次级信息灰 — 模块、file:line、时间戳、消息体共用。
pub(super) const GRAY: &str = Color::Gray.code();

/// 级别展示元数据（索引 = 级别值）：(级别颜色, 标签)。
pub(super) const LEVEL_META: [(Color, &str); 5] = [
    (Color::Red, "ERROR"),     // LogLevel::Error = 0
    (Color::Yellow, "WARN"),   // LogLevel::Warn = 1
    (Color::Green, "INFO"),    // LogLevel::Info = 2
    (Color::Cyan, "DEBUG"),    // LogLevel::Debug = 3
    (Color::Magenta, "TRACE"), // LogLevel::Trace = 4
];

/// 级别颜色 + 标签（console 使用）。
pub(super) fn level_style(level: LogLevel) -> (&'static str, &'static str) {
    let (color, label) = LEVEL_META[level as usize];
    (color.code(), label)
}

/// 级别标签（无颜色，供 ring 快照读取使用）。
pub(super) fn label_str(level: LogLevel) -> &'static str {
    LEVEL_META[level as usize].1
}
