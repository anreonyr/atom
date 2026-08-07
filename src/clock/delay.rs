// 忙等延时 — 无任务上下文可用的延时（boot 早期/关中断/驱动初始化）
//
// 自旋读时钟源直至目标时刻：不阻塞、不依赖 tick/调度器。
// 与任务级 schedule::sleep（阻塞 + 唤醒状态机）语义区分——
// delay 是纯时间服务（clock），sleep 是任务状态机操作（scheduler）。
// 依赖单向：delay → convert（换算）+ source（时刻），无循环。

use core::time::Duration;

use super::{convert, source};

/// 忙等延时 `d` 时长。
///
/// 关中断/无任务上下文（boot 早期、驱动 probe）下亦可用。
/// 精度取决于时钟源粒度（`time` CSR 连续计数）；自旋期间不释放 CPU，
/// 且不响应抢占（中断关闭时）——不可与 [`super::super::schedule::sleep`]
/// 混用语义。
pub fn delay(d: Duration) {
    let target = source::now().wrapping_add(convert::duration_to_ticks(d));
    while source::now() < target {
        core::hint::spin_loop();
    }
}
