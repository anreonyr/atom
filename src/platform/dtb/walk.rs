// FDT 结构块迭代器
//
// 将 FDT 结构块作为 token 流迭代，零分配设计。
// 实现 `Iterator` trait，产生 `Result<Token, WalkError>`。

use super::header::FdtHeader;
use super::resolve::StringTable;

/// FDT 结构块 token 常量。
mod token {
    pub const BEGIN_NODE: u32 = 0x0000_0001;
    pub const END_NODE: u32   = 0x0000_0002;
    pub const PROP: u32       = 0x0000_0003;
    pub const NOP: u32        = 0x0000_0004;
    pub const END: u32        = 0x0000_0009;
}

/// 遍历过程中断错误。
#[derive(Debug)]
pub enum WalkError {
    /// 结构块数据被截断（游标越界）
    Truncated,
    /// 节点嵌套不匹配（END_NODE 多于 BEGIN_NODE）
    Unbalanced,
    /// 字符串块偏移越界
    #[allow(dead_code)]
    BadStringOffset(u32),
}

/// FDT 结构块 token。
///
/// 迭代器按深度优先顺序产生 token。调用方可用 `skip_subtree()`
/// 跳过当前节点的所有子节点（包括后代）。
#[derive(Debug)]
pub enum Token<'a> {
    /// 进入节点，附带节点名称。
    BeginNode(&'a str),
    /// 离开当前节点。
    EndNode,
    /// 属性：name 来自字符串块，value 来自结构块（保持大端序原样）。
    Property { name: &'a str, value: &'a [u8] },
    /// 无操作（对齐填充）。
    Nop,
    /// 结构块结束标记——迭代终止。
    End,
}

/// FDT 结构块迭代器。
///
/// 实现 `Iterator<Item = Result<Token<'static>, WalkError>>`。
/// 内部维护 `skip_depth` 状态：>0 时消费 token 但不 yield，用于子树跳过。
pub(crate) struct FdtIter {
    /// 结构块基址（字节指针，用于计算偏移）
    struct_base: *const u8,
    /// 当前大端序 u32 读取位置
    cursor: *const u32,
    /// 结构块结束位置（越界边界）
    end: *const u32,
    /// 字符串表解析器
    strings: StringTable,
    /// skip_subtree 状态：>0 时跳过所有 token，直到对应的 END_NODE
    skip_depth: u32,
    /// 是否已 yield END token
    finished: bool,
}

// 迭代器内部游标裸指针在单 hart 下安全
unsafe impl Send for FdtIter {}

impl FdtIter {
    /// 从已验证的 FDT 头部创建迭代器。
    ///
    /// # Safety
    ///
    /// `header` 必须是经过 `FdtHeader::validate()` 验证的有效头部，
    /// 且 DTB 内存在迭代期间保持有效。
    pub(crate) unsafe fn new(header: &FdtHeader) -> Self {
        let base = header as *const _ as usize;
        let struct_off = header.off_dt_struct() as usize;
        Self {
            struct_base: base as *const u8,
            cursor: (base + struct_off) as *const u32,
            end: (base + struct_off + header.size_dt_struct() as usize) as *const u32,
            strings: StringTable::new(header),
            skip_depth: 0,
            finished: false,
        }
    }

    /// 当前游标在结构块中的偏移（相对 struct_base 的字节数）
    pub(crate) fn cursor_offset(&self) -> u32 {
        (self.cursor as usize - self.struct_base as usize) as u32
    }

    /// 跳过当前节点的整个子树。
    ///
    /// 调用方必须在刚收到 `Token::BeginNode(_)` 后立即调用此方法。
    /// 此后所有 token 将被内部消费，直到匹配的 `END_NODE` 被消耗后恢复正常迭代。
    #[inline]
    pub(crate) fn skip_subtree(&mut self) {
        self.skip_depth = 1;
    }

    // ── 内部方法 ──────────────────────────────────────

    /// 读取一个大端序 u32，前进游标。
    unsafe fn read_u32(&mut self) -> Result<u32, WalkError> {
        if self.cursor >= self.end {
            return Err(WalkError::Truncated);
        }
        let val = self.cursor.read_volatile();
        self.cursor = self.cursor.add(1);
        Ok(val)
    }

    /// 读取 null-terminated 节点名称，前进游标到 4 字节对齐。
    ///
    /// 使用 `from_utf8` 验证名称有效性。
    unsafe fn read_node_name(&mut self) -> Result<&'static str, WalkError> {
        let start = self.cursor as *const u8;
        let max = self.end as usize - start as usize;
        let mut len = 0usize;
        while len < max {
            if start.add(len).read_volatile() == 0 {
                break;
            }
            len += 1;
        }
        // 未找到 null 终止符 → 数据截断
        if len >= max {
            return Err(WalkError::Truncated);
        }
        let total = (len + 1 + 3) & !3; // null + 4-byte 对齐
        self.cursor = (start.add(total)) as *const u32;

        let slice = core::slice::from_raw_parts(start, len);
        core::str::from_utf8(slice).map_err(|_| WalkError::Truncated)
    }

    /// 读取 `len` 字节的属性值，前进到 4 字节对齐。
    ///
    /// 使用 checked_add 防止整数溢出。
    unsafe fn read_prop_value(&mut self, len: usize) -> Result<&'static [u8], WalkError> {
        let ptr = self.cursor as *const u8;
        // SAFETY: len 来自 DTB PROP token，padded_len 带溢出检查
        let padded = len.checked_add(3)
            .map(|v| v & !3)
            .ok_or(WalkError::Truncated)?;
        let end_addr = (ptr as usize).checked_add(padded)
            .ok_or(WalkError::Truncated)?;
        if end_addr > self.end as usize {
            return Err(WalkError::Truncated);
        }
        self.cursor = end_addr as *const u32;
        Ok(core::slice::from_raw_parts(ptr, len))
    }

    /// 消费 token（不 yield）——skip_subtree 模式专用。
    ///
    /// 返回 `true` 若 skip 模式仍在继续。
    unsafe fn consume_skip(&mut self, token_val: u32) -> Result<bool, WalkError> {
        match token_val {
            token::BEGIN_NODE => {
                // CRITICAL: 消费节点名称后增加 skip_depth
                self.read_node_name()?;
                self.skip_depth += 1;
                Ok(true)
            }
            token::END_NODE => {
                self.skip_depth -= 1;
                Ok(self.skip_depth > 0)
            }
            token::PROP => {
                let len = u32::from_be(self.read_u32()?) as usize;
                let _nameoff = self.read_u32()?; // 跳过 nameoff
                self.read_prop_value(len)?;       // 跳过 value
                Ok(self.skip_depth > 0)
            }
            token::END => {
                self.finished = true;
                Err(WalkError::Unbalanced)
            }
            _ => Ok(true), // NOP 或其他——继续跳过
        }
    }

    /// 产生下一个正常 token。
    unsafe fn next_normal(&mut self, token_val: u32) -> Option<Result<Token<'static>, WalkError>> {
        Some(match token_val {
            token::BEGIN_NODE => match self.read_node_name() {
                Ok(name) => Ok(Token::BeginNode(name)),
                Err(e) => Err(e),
            },
            token::END_NODE => Ok(Token::EndNode),
            token::PROP => {
                let len_raw = match self.read_u32() {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
                let len = u32::from_be(len_raw) as usize;
                let nameoff = match self.read_u32() {
                    Ok(v) => u32::from_be(v),
                    Err(e) => return Some(Err(e)),
                };
                let name = match self.strings.get(nameoff) {
                    Some(n) => n,
                    None => return Some(Err(WalkError::BadStringOffset(nameoff))),
                };
                let value = match self.read_prop_value(len) {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
                Ok(Token::Property { name, value })
            }
            token::NOP => Ok(Token::Nop),
            token::END => {
                self.finished = true;
                Ok(Token::End)
            }
            _ => Err(WalkError::Truncated), // 未知 token
        })
    }
}

impl Iterator for FdtIter {
    type Item = Result<Token<'static>, WalkError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }

        loop {
            // 读取下一个 u32 token（裸指针比较是安全的）
            if self.cursor >= self.end {
                self.finished = true;
                return Some(Err(WalkError::Truncated));
            }
            let token_val = {
                let v = unsafe { self.cursor.read_volatile() };
                unsafe { self.cursor = self.cursor.add(1) };
                u32::from_be(v)
            };

            if self.skip_depth > 0 {
                // SAFETY: cursor/end 来自已验证头部，skip_depth 保证 token 被正确消费
                match unsafe { self.consume_skip(token_val) } {
                    Ok(true) => continue, // 仍在 skip 模式
                    Ok(false) => {
                        // skip 模式结束，继续正常迭代
                        continue;
                    }
                    Err(e) => return Some(Err(e)),
                }
            }

            // SAFETY: 非 skip 模式，正常 yield token
            return unsafe { self.next_normal(token_val) };
        }
    }
}
