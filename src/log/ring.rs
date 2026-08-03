// 最近日志环形快照 — ring buffer 存储层
//
// 保存最近 RING_CAP 条日志（LogMessage，定长 owned 结构——log_line 构造后
// 直接 move 入 ring，无独立快照类型/二次拷贝）。
// console 未就绪的早期日志不丢失；log_read() 按快照语义读取，供 /dev/log。
// RING 锁与输出锁 OUT_LOCK 各自独立、不嵌套持有（死锁安全性见 mod.rs 头注释）。

use core::fmt::Write as _;

use crate::lock::SpinLock;

use super::buf::Buf;
use super::palette::label_str;
use super::record::{LogMessage, MSG_CAP};

/// 环形缓冲容量（日志条数）
pub(super) const RING_CAP: usize = 128;
/// 日志读取时的单行缓冲容量（header + 缩进 msg + 换行）
pub(super) const LINE_CAP: usize = 40 + MSG_CAP + 1;

/// 日志环形缓冲 — 最近 RING_CAP 条日志。
pub(super) struct LogRing {
    entries: [LogMessage; RING_CAP],
    /// 下一个写入位置
    head: usize,
    /// 已存条数（≤ RING_CAP，写满后恒为 RING_CAP）
    count: usize,
    /// 下一条记录的 seq（全局递增，检测丢消息）
    next_seq: u64,
}

impl LogRing {
    const fn new() -> Self {
        LogRing {
            entries: [LogMessage::empty(); RING_CAP],
            head: 0,
            count: 0,
            next_seq: 1,
        }
    }

    /// 入环 — 分配 seq 后存储（move，零拷贝）。
    pub(super) fn push(&mut self, m: LogMessage) {
        let mut m = m;
        m.seq = self.next_seq;
        self.next_seq += 1;
        self.entries[self.head] = m;
        self.head = (self.head + 1) % RING_CAP;
        if self.count < RING_CAP {
            self.count += 1;
        }
    }

    /// 当前已记录 seq 范围（最旧 → 最新）——`None` 表示 ring 为空。
    fn seq_range(&self) -> Option<(u64, u64)> {
        if self.count == 0 {
            return None;
        }
        let newest = self.next_seq - 1;
        let oldest = newest - self.count as u64 + 1;
        Some((oldest, newest))
    }

    /// 按写入顺序（最旧 → 最新）迭代。
    fn iter(&self) -> impl Iterator<Item = &LogMessage> + '_ {
        let start = if self.count == RING_CAP { self.head } else { 0 };
        let n = self.count;
        (0..n).map(move |i| &self.entries[(start + i) % RING_CAP])
    }
}

/// 日志环形缓冲（关中断保护；与输出锁独立，不嵌套持有）。
pub(super) static RING: SpinLock<LogRing> = SpinLock::new(LogRing::new());

/// 当前已记录日志的 seq 范围（最旧 → 最新）——ring 为空时返回 `None`。
///
/// 读取方在两次 `log_read` 前后各取一次范围，对比新旧 seq 即可检测
/// ring 覆盖导致的丢消息：`最新 - 最旧 + 1 > RING_CAP` 说明期间发生过覆盖。
///
/// 预留 API：当前无调用方（/dev/log 保持快照语义，seq 检测留待消费）。
#[allow(dead_code)]
pub fn log_seq_range() -> Option<(u64, u64)> {
    RING.lock().seq_range()
}

/// 将环形缓冲中的日志按快照语义拷贝到 `buf`。
///
/// `offset` 为从最旧条目起的字节偏移（由 VFS 维护），返回实际写入字节数。
/// 每条日志输出为两行：header 行 + 缩进 4 空格的 msg 行。
pub fn log_read(offset: usize, buf: &mut [u8]) -> usize {
    let ring = RING.lock();
    let mut out = 0usize;
    let mut pos = 0usize;
    for e in ring.iter() {
        let mut line = Buf::<LINE_CAP>::new();
        let _ = write!(
            line,
            "[{}] ({}) {}.{:06}\n    {}\n",
            label_str(e.level),
            e.module(),
            e.ts.sec(),
            e.ts.usec(),
            e.msg(),
        );
        let len = line.len();
        if pos + len > offset {
            let start = offset.saturating_sub(pos);
            let n = (len - start).min(buf.len() - out);
            buf[out..out + n].copy_from_slice(&line.as_slice()[start..start + n]);
            out += n;
            if out == buf.len() {
                break;
            }
        }
        pos += len;
    }
    out
}
