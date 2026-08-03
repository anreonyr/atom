// 日志消息结构 — 一条日志的完整数据（级别/时间/模块/消息体/定位/序号）
//
// LogMessage 是定长 owned 结构：log_line 构造一次（消息体直接格式化进内部
// 缓冲），console 行渲染借用、ring 存储 move（零二次拷贝）——ring 存的就是
// LogMessage 本身，无独立快照类型。
//
// 无堆约束：module/msg 是内嵌定长缓冲（Buf），file:line 中 file 来自 file!()
// 恒 'static，因此 loc 可直接保存（ring 快照也带定位信息）。
//
// 时间戳直接存单个值（微秒总数，Linux printk 的 ts_nsec 对应物）而非 sec/usec
// 双字段——显示格式在消费处拆分，存储与换算只此一处。

use core::fmt::Write as _;

use super::buf::Buf;
use super::LogLevel;

/// 模块短名容量（字节）
pub(super) const MODULE_CAP: usize = 24;
/// 单条日志消息体容量（字节）
pub(super) const MSG_CAP: usize = 256;

/// 时间戳 — 日志记录时刻（自 boot 起的微秒总数）。
///
/// 显示时拆分为 秒.微秒（见 [`sec`](Self::sec)/[`usec`](Self::usec)）。
#[derive(Clone, Copy)]
pub(super) struct Timestamp(u64);

impl Timestamp {
    /// 由微秒总数构造（读取/快照恢复用）。
    pub(super) const fn new(total: u64) -> Self {
        Timestamp(total)
    }

    /// 秒部分（`total / 1_000_000`）。
    pub(super) fn sec(self) -> u64 {
        self.0 / 1_000_000
    }

    /// 微秒余数（`total % 1_000_000`，恒 < 1_000_000）。
    pub(super) fn usec(self) -> u64 {
        self.0 % 1_000_000
    }
}

/// 日志消息 — 一条日志的完整数据：log_line 构造一次，console 借用渲染、
/// ring move 存储（同一结构贯穿，无独立快照类型）。
#[derive(Clone, Copy)]
pub(super) struct LogMessage {
    pub(super) level: LogLevel,
    pub(super) ts: Timestamp,
    /// 全局递增序号（ring push 时分配；0 = 未入 ring）
    pub(super) seq: u64,
    module: Buf<MODULE_CAP>,
    msg: Buf<MSG_CAP>,
    /// file:line 定位（file!() 恒 'static）——仅 Debug/Trace 级别附带
    pub(super) loc: Option<(&'static str, u32)>,
}

impl LogMessage {
    /// 空条目（ring 数组初始化用）。
    pub(super) const fn empty() -> Self {
        LogMessage {
            level: LogLevel::Error,
            ts: Timestamp::new(0),
            seq: 0,
            module: Buf::new(),
            msg: Buf::new(),
            loc: None,
        }
    }

    /// 构造：拷贝模块短名，消息体由调用方经 [`msg_mut`] 写入。
    pub(super) fn new(
        level: LogLevel,
        ts: Timestamp,
        module: &str,
        loc: Option<(&'static str, u32)>,
    ) -> Self {
        let mut m = Self::empty();
        m.level = level;
        m.ts = ts;
        m.loc = loc;
        let _ = m.module.write_str(module);
        m
    }

    /// 模块短名（≤ MODULE_CAP，超出截断）。
    pub(super) fn module(&self) -> &str {
        self.module.as_str()
    }

    /// 消息体（纯文本，≤ MSG_CAP）。
    pub(super) fn msg(&self) -> &str {
        self.msg.as_str()
    }

    /// 消息体写入目标（fmt::Write）。
    pub(super) fn msg_mut(&mut self) -> &mut Buf<MSG_CAP> {
        &mut self.msg
    }
}
