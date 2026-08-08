// envcall 分发 — 处理来自 U-mode 的 ecall（scause=8，Environment Call from U-mode）
//
// U-mode 任务执行 ecall 指令 → 陷入 S-mode → trap_handler 按 scause=8 路由到
// 本模块（S-mode 自身的 ecall 是 scause=9，直接陷入 M-mode OpenSBI，不经这里）。
// 调用约定（RISC-V ABI + Linux 惯例）：
//   a7 = 调用号（syscall number）
//   a0..a5 = 参数
//   返回: a0 = 返回值；错误用负数 -errno 编码（usize 两补），未知号 → -ENOSYS
//
// 分发形态：enum [`Ecall`] 的变体即调用号（`from_number` 解码 a7），dispatch
// 按变体 match 转发到独立的 sys_* 函数——新增一个 ecall = 加一个变体 +
// from_number 一行 + dispatch 一行 + 一个 sys_* 函数。标状态（exit 标 Reap /
// read 置 WaitRead）在 dispatch 完成，调度（scheduler 选下一任务）统一由
// trap_handler 在 [`DispatchResult::Reschedule`] 时执行。

/// Linux errno ENOSYS = 38；返回值语义为 -errno（负数），usize 下取两补。
pub const ENOSYS: usize = (38usize).wrapping_neg();
/// Linux riscv64 系统调用号：read = 63。
pub const READ: usize = 63;
/// Linux riscv64 系统调用号：write = 64。
pub const WRITE: usize = 64;
/// Linux riscv64 系统调用号：exit = 93。
pub const EXIT: usize = 93;
/// 教学自定义号段（≥1000，避让 Linux 保留区）：map —— 用户堆匿名页分配。
/// 语义为"堆区单调分配匿名页"，非 Linux mmap 文件映射，故不占用 mmap 号。
pub const MAP: usize = 1000;
/// 教学自定义号段：unmap —— 用户堆匿名页释放（块表精确匹配）。
pub const UNMAP: usize = 1001;
/// 教学自定义号段（≥1000，避让 Linux 保留区）：open —— 打开已存在路径。
/// 语义对应 Linux open（仅查找已存在节点，无 O_CREAT 创建语义），号自定。
pub const OPEN: usize = 1002;
/// 教学自定义号段：close —— 关闭 fd（释放 fd 槽位）。
pub const CLOSE: usize = 1003;
/// 教学自定义号段：seek —— 设置文件偏移（whence 语义对应 Linux lseek）。
pub const SEEK: usize = 1004;
/// 教学自定义号段：control —— 设备控制命令（语义对应 Linux ioctl）。
pub const CONTROL: usize = 1005;
/// errno EBADF = 9（fd 非法）
const EBADF: usize = (9usize).wrapping_neg();
/// errno ENOENT = 2（文件/目录不存在，含无输入设备）
const ENOENT: usize = (2usize).wrapping_neg();
/// errno EAGAIN = 11（非阻塞操作暂不可完成）——errno_of 映射用；
/// read 阻塞等待走 Reschedule park，不经此值返回用户。
const EAGAIN: usize = (11usize).wrapping_neg();
/// errno EACCES = 13（权限不足）
const EACCES: usize = (13usize).wrapping_neg();
/// errno EIO = 5（I/O 错误）
const EIO: usize = (5usize).wrapping_neg();
/// errno ENOTDIR = 20（路径组件非目录）
const ENOTDIR: usize = (20usize).wrapping_neg();
/// errno EOPNOTSUPP = 95（操作不被支持）
const EOPNOTSUPP: usize = (95usize).wrapping_neg();
/// errno EFAULT = 14（用户指针非法：非用户区或未映射）
const EFAULT: usize = (14usize).wrapping_neg();
/// errno EINVAL = 22（参数非法）
const EINVAL: usize = (22usize).wrapping_neg();
/// errno ENOMEM = 12（内存不足）
const ENOMEM: usize = (12usize).wrapping_neg();

use crate::filesystem::ops::{FileError, OpenFlags, SeekFrom};
use crate::{debug, info};

/// 一个 U-mode ecall 调用：变体即调用号（a7 解码结果）。
///
/// `from_number` 是号 → 变体的唯一映射（表驱动语义，编译期固定集合），
/// dispatch 按变体 match 转发。derive(Debug) 让日志打 `Ecall::Read` 而非
/// 裸数字。
#[derive(Debug, Clone, Copy)]
pub enum Ecall {
    /// `read(fd, buf, count)` — a7=63（Linux riscv64 号）。
    Read,
    /// `write(fd, buf, count)` — a7=64。
    Write,
    /// `exit(code)` — a7=93。
    Exit,
    /// `map(size)` — 自定义号，用户堆匿名页分配。
    Map,
    /// `unmap(addr, size)` — 自定义号，用户堆匿名页释放。
    Unmap,
    /// `open(path, flags, mode)` — 自定义号，打开已存在路径返回 fd。
    Open,
    /// `close(fd)` — 自定义号，关闭 fd。
    Close,
    /// `seek(fd, offset, whence)` — 自定义号，设置文件偏移。
    Seek,
    /// `control(fd, cmd, arg)` — 自定义号，设备控制命令。
    Control,
}

impl Ecall {
    /// a7 调用号 → 变体；未注册号返回 `None`（dispatch 回落 -ENOSYS）。
    fn from_number(number: usize) -> Option<Self> {
        match number {
            READ => Some(Self::Read),
            WRITE => Some(Self::Write),
            EXIT => Some(Self::Exit),
            MAP => Some(Self::Map),
            UNMAP => Some(Self::Unmap),
            OPEN => Some(Self::Open),
            CLOSE => Some(Self::Close),
            SEEK => Some(Self::Seek),
            CONTROL => Some(Self::Control),
            _ => None,
        }
    }
}

/// envcall 分发结果 — trap_handler 据此决定"写回 a0 恢复用户态"还是
/// "当前任务已离开运行态，调度下一任务"。
pub enum DispatchResult {
    /// 正常返回：值写回 `frame.a0`，sepc += 4，恢复当前任务。
    Ret(usize),
    /// 当前任务已离开运行态（exit 已标 Reap / read 已置 WaitRead park）——
    /// trap_handler 调 scheduler(frame) 切到下一任务：不再写 a0、不再加 sepc。
    Reschedule,
}

/// 分发 U-mode ecall。
///
/// `number` = a7（调用号），`args` = a0..a5（参数寄存器快照）。按 [`Ecall`]
/// 变体转发到对应 sys_* 函数；未知号返回 [`ENOSYS`]。
///
/// # 调用面
///
/// - `READ`（63）：`read(fd, buf, count)`。fd 经 VFS 全局表解析（预置
///   fd 0 = /dev/stdin）；buf 须为映射的**用户区地址**（校验失败 -EFAULT）。
///   缓冲空 → 置输入等待（WaitRead）返回 [`DispatchResult::Reschedule`]——
///   trap_handler 调 scheduler 直接 park 任务切走（不再用户态忙转），字符
///   到达唤醒后 sret 到 ecall 重放分发，缓冲已非空读到返回。fd 非法 →
///   -EBADF；count = 0 → 0。
/// - `WRITE`（64）：`write(fd, buf, count)`。fd 经 VFS 全局表解析（预置
///   fd 1 = /dev/console0）；buf 须为映射的**用户区地址**（校验失败
///   -EFAULT），内容逐字节输出（UART 层 \n → \r\n）。返回写入字节数；
///   fd 非法 → -EBADF；count = 0 → 0。
/// - `EXIT`（93）：`exit(code)`。标 Zombie（带退出码）后返回
///   [`DispatchResult::Reschedule`]——trap_handler 调度下一任务，不再恢复
///   用户态。
/// - `MAP`（1000，自定义号）：`map(size)`。从当前任务堆区
///   [`crate::memory::USER_HEAP_BASE`] 单调分配匿名页，返回用户区 VA；
///   堆区耗尽 → -ENOMEM。线程共享空间天然共享堆。
/// - `UNMAP`（1001，自定义号）：`unmap(addr, size)`。块表精确匹配后释放
///   物理页 + 解除映射，返回 0；不匹配 → -EINVAL。
/// - `OPEN`（1002，自定义号）：`open(path, flags, mode)`。path 为用户区
///   NUL 结尾路径（非用户区/未映射/超长 → -EFAULT），经 [`user_str`] 拷入
///   内核后从根解析打开已存在节点返回 fd；flags 低 2 位 accmode →
///   READ/WRITE/RDWR（3 → -EINVAL）；路径不存在 → -ENOENT。mode 忽略
///   （无创建语义）。
/// - `CLOSE`（1003，自定义号）：`close(fd)`。释放 fd 槽位，成功 → 0；
///   fd 非法或未打开 → -EBADF。
/// - `SEEK`（1004，自定义号）：`seek(fd, offset, whence)`。whence
///   0=Start/1=Current/2=End；Start 负偏移或 whence 非法 → -EINVAL；
///   流式设备不支持 → -EOPNOTSUPP。
/// - `CONTROL`（1005，自定义号）：`control(fd, cmd, arg)`。设备控制命令
///   （ioctl 语义），cmd 截断 u32、arg 透传；设备不支持 → -EOPNOTSUPP。
///
/// # 日志
///
/// READ 走 debug!——等待期间每轮重放都进本分发，info! 会刷屏；未知号保留
/// info!（骨架阶段可见性优先）。
pub fn dispatch(number: usize, args: [usize; 6]) -> DispatchResult {
    let Some(call) = Ecall::from_number(number) else {
        info!("envcall: number={number:#x} args={args:?} → -ENOSYS");
        return DispatchResult::Ret(ENOSYS);
    };
    match call {
        Ecall::Read => sys_read(args),
        Ecall::Write => sys_write(args),
        Ecall::Exit => sys_exit(args),
        Ecall::Map => sys_map(args),
        Ecall::Unmap => sys_unmap(args),
        Ecall::Open => sys_open(args),
        Ecall::Close => sys_close(args),
        Ecall::Seek => sys_seek(args),
        Ecall::Control => sys_control(args),
    }
}

/// `FileError` → errno（负值编码）映射 — syscall 层统一错误出口。
///
/// 错误码语义即行为：调用方按返回值决定降级。`Eof` 映射 0——读到文件尾
/// 不是错误（Linux read 返回 0）。`WouldBlock` 映射 -EAGAIN：read 阻塞等待
/// 走 Reschedule park（不返回用户），此处仅保证 match 穷尽。
fn errno_of(e: FileError) -> usize {
    match e {
        FileError::NotFound => ENOENT,
        FileError::NotSupported => EOPNOTSUPP,
        FileError::InvalidFd => EBADF,
        FileError::PermissionDenied => EACCES,
        FileError::IoError => EIO,
        FileError::Eof => 0,
        FileError::WouldBlock => EAGAIN,
        FileError::InvalidArg => EINVAL,
        FileError::NotDirectory => ENOTDIR,
    }
}

/// `read(fd, buf, count)` — 从 fd 读取到用户缓冲区。
///
/// fd 经 VFS 全局表解析（fd 0 = /dev/stdin 预置）；buf 须为映射的用户区
/// 地址（-EFAULT）；流式设备缓冲空 → 置输入等待后返回 [`DispatchResult::Reschedule`]
/// （trap 态直接 park，唤醒后重放分发）；fd 非法 → -EBADF。
fn sys_read(args: [usize; 6]) -> DispatchResult {
    let (fd, buf, count) = (args[0], args[1], args[2]);
    if count == 0 {
        return DispatchResult::Ret(0);
    }
    // 用户 buf 校验：非用户区或未映射 → -EFAULT（防止写坏内核）
    if !user_ptr_valid(buf, count) {
        debug!("envcall: read(buf={buf:#x} count={count}) → -EFAULT");
        return DispatchResult::Ret(EFAULT);
    }
    // SAFETY: 校验已保证 [buf, buf+count) 落在用户区且每页映射（user_ptr_valid）。
    let slice = unsafe { core::slice::from_raw_parts_mut(buf as *mut u8, count) };
    match crate::filesystem::filetable::read(fd, slice) {
        Ok(n) => {
            // 读到数据：复位输入等待标记（若 park 唤醒重放期间置过位）——
            // 幂等（非 WaitRead 不动作）。
            crate::schedule::clear_input_wait();
            DispatchResult::Ret(n)
        }
        Err(FileError::WouldBlock) => {
            // 缓冲空：置输入等待（WaitRead + resume_sepc=0 保持 ecall 地址），
            // 返回 Reschedule——trap_handler 调 scheduler 直接 park 当前任务
            // 切到下一任务（不再 sret 重放忙转）。字符到达时 wake_input_waiters
            // 唤醒，任务被调度回后 sret 到 ecall 重放分发，缓冲已非空读到返回。
            crate::schedule::mark_input_wait();
            DispatchResult::Reschedule
        }
        Err(e) => DispatchResult::Ret(errno_of(e)),
    }
}

/// `write(fd, buf, count)` — 从用户缓冲区写入到 fd。
///
/// fd 经 VFS 全局表解析（fd 1 = /dev/console0 预置）；buf 须为映射的用户区
/// 地址（-EFAULT）；内容逐字节输出（UART 层 \n → \r\n）。返回写入字节数；
/// fd 非法 → -EBADF。
fn sys_write(args: [usize; 6]) -> DispatchResult {
    let (fd, buf, count) = (args[0], args[1], args[2]);
    if count == 0 {
        return DispatchResult::Ret(0);
    }
    // 用户 buf 校验：非用户区或未映射 → -EFAULT（防止读坏内核/读野指针）
    if !user_ptr_valid(buf, count) {
        debug!("envcall: write(buf={buf:#x} count={count}) → -EFAULT");
        return DispatchResult::Ret(EFAULT);
    }
    // SAFETY: 校验已保证 [buf, buf+count) 落在用户区且每页映射（user_ptr_valid）。
    let slice = unsafe { core::slice::from_raw_parts(buf as *const u8, count) };
    match crate::filesystem::filetable::write(fd, slice) {
        Ok(n) => DispatchResult::Ret(n),
        Err(e) => DispatchResult::Ret(errno_of(e)),
    }
}

/// `exit(code)` — 终止当前任务（标 Zombie 带退出码）。
///
/// 标 Reap 后返回 [`DispatchResult::Reschedule`]，trap_handler 调 scheduler
/// 切下一任务——不再恢复用户态（与缺页 terminate 共用终止语义，区别仅退出码）。
fn sys_exit(args: [usize; 6]) -> DispatchResult {
    let code = args[0] as i32;
    info!("envcall: exit({code})");
    crate::schedule::mark_reap(code);
    DispatchResult::Reschedule
}

/// `unmap(addr, size)` — 释放用户堆匿名页（块表精确匹配）。
///
/// 匹配成功 → 释放物理页 + 解除映射，返回 0；块表不匹配 → -EINVAL；
/// 无当前任务空间 → -EINVAL。
fn sys_unmap(args: [usize; 6]) -> DispatchResult {
    let (addr, size) = (args[0], args[1]);
    let Some(sp) = crate::schedule::current_space() else {
        return DispatchResult::Ret(EINVAL); // 无当前任务空间
    };
    // SAFETY: current_space 指向 CURRENT 任务空间，trap 期间不回收
    let space = unsafe { sp.as_ref() };
    if space.heap_deallocate(addr, size) {
        info!("envcall: unmap({addr:#x}, {size:#x}) → 0");
        DispatchResult::Ret(0)
    } else {
        info!("envcall: unmap({addr:#x}, {size}) → -EINVAL (no matching block)");
        DispatchResult::Ret(EINVAL)
    }
}

/// `map(size)` — 从当前任务堆区分配匿名页，返回用户区 VA。
///
/// 堆区耗尽 → -ENOMEM；无当前任务空间 → -EINVAL。
fn sys_map(args: [usize; 6]) -> DispatchResult {
    let size = args[0];
    let Some(sp) = crate::schedule::current_space() else {
        return DispatchResult::Ret(EINVAL); // 无当前任务空间
    };
    // SAFETY: current_space 指向 CURRENT 任务空间，trap 期间不回收
    let space = unsafe { sp.as_ref() };
    match space.heap_allocate(size, crate::memory::allocator::page::allocator()) {
        Ok(va) => {
            info!("envcall: map({size:#x}) → {va:#x}");
            DispatchResult::Ret(va)
        }
        Err(_) => DispatchResult::Ret(ENOMEM), // 堆区耗尽
    }
}

/// 路径长度上限（`user_str` 扫描边界；devfs 路径远短于此）。
const MAX_PATH: usize = 256;

/// Linux O_ACCMODE（低 2 位）→ [`OpenFlags`] 映射；3（无定义 accmode）→ 非法。
///
/// 教学内核无创建语义，`O_CREAT` 等其余位忽略不解析。
fn open_flags_from(flags: usize) -> core::result::Result<OpenFlags, ()> {
    match flags & 0b11 {
        0 => Ok(OpenFlags::READ),
        1 => Ok(OpenFlags::WRITE),
        2 => Ok(OpenFlags::RDWR),
        _ => Err(()),
    }
}

/// `open(path, flags, mode)` — 打开已存在路径返回 fd。
///
/// `path` 须为映射的用户区 NUL 结尾字符串（非法 → -EFAULT），经 [`user_str`]
/// 拷入内核后走 VFS 全局表从根解析（devfs 节点等）；`flags` 低 2 位
/// accmode（0=READ/1=WRITE/2=RDWR，3 → -EINVAL），`mode` 忽略（无创建语义）。
/// 路径不存在 → -ENOENT。
fn sys_open(args: [usize; 6]) -> DispatchResult {
    let (path, flags, _mode) = (args[0], args[1], args[2]);
    let Some(path) = user_str(path, MAX_PATH) else {
        debug!("envcall: open(path={path:#x}) → -EFAULT");
        return DispatchResult::Ret(EFAULT);
    };
    let Ok(flags) = open_flags_from(flags) else {
        info!("envcall: open(path={path:?}) → -EINVAL (accmode={flags:#x})");
        return DispatchResult::Ret(EINVAL);
    };
    match crate::filesystem::filetable::open(&path, flags) {
        Ok(fd) => {
            info!("envcall: open({path:?}) → fd={fd}");
            DispatchResult::Ret(fd)
        }
        Err(e) => {
            info!("envcall: open({path:?}) → {e:?}");
            DispatchResult::Ret(errno_of(e))
        }
    }
}

/// `close(fd)` — 关闭 fd（释放 fd 槽位）。
///
/// fd 非法或未打开 → -EBADF；成功 → 0。
fn sys_close(args: [usize; 6]) -> DispatchResult {
    let fd = args[0];
    match crate::filesystem::filetable::close(fd) {
        Ok(()) => {
            info!("envcall: close({fd}) → 0");
            DispatchResult::Ret(0)
        }
        Err(e) => DispatchResult::Ret(errno_of(e)),
    }
}

/// `seek(fd, offset, whence)` — 设置文件偏移（whence：0=Start/1=Current/2=End）。
///
/// `offset` 为两补有符号量：`Start` 负偏移 → -EINVAL；`Current`/`End` 带符号
/// delta。委托 VFS `filetable::seek` 计算新绝对偏移写回。fd 非法 → -EBADF；
/// 流式设备不支持 seek → -EOPNOTSUPP；whence 非法 → -EINVAL。
fn sys_seek(args: [usize; 6]) -> DispatchResult {
    let (fd, offset, whence) = (args[0], args[1], args[2]);
    let pos = match whence {
        0 => {
            if offset as isize >= 0 {
                SeekFrom::Start(offset)
            } else {
                return DispatchResult::Ret(EINVAL); // Start 负偏移非法
            }
        }
        1 => SeekFrom::Current(offset as isize),
        2 => SeekFrom::End(offset as isize),
        _ => return DispatchResult::Ret(EINVAL), // whence 仅 0/1/2
    };
    match crate::filesystem::filetable::seek(fd, pos) {
        Ok(new) => {
            info!("envcall: seek(fd={fd}, whence={whence}) → {new:#x}");
            DispatchResult::Ret(new)
        }
        Err(e) => DispatchResult::Ret(errno_of(e)),
    }
}

/// `control(fd, cmd, arg)` — 设备控制命令（Linux ioctl 语义）。
///
/// `cmd` 截断为 u32（命令码），`arg` 语义由设备定义原样透传。委托
/// `filetable::control`，返回设备定义值（isize 两补转 usize）；fd 非法 →
/// -EBADF；设备不支持 → -EOPNOTSUPP。
fn sys_control(args: [usize; 6]) -> DispatchResult {
    let (fd, cmd, arg) = (args[0], args[1], args[2]);
    match crate::filesystem::filetable::control(fd, cmd as u32, arg) {
        Ok(n) => {
            info!("envcall: control(fd={fd}, cmd={cmd:#x}) → {n:#x}");
            DispatchResult::Ret(n as usize)
        }
        Err(e) => DispatchResult::Ret(errno_of(e)),
    }
}

/// 校验用户指针 `[addr, addr+len)` 可访问：落在用户半区且每页均已在
/// 当前任务空间映射。
///
/// 单 hart 关中断（trap 上下文）下校验与后续访问之间页表不会变化
/// （缺页处理也发生在同一 hart），无 TOCTOU 竞态。
fn user_ptr_valid(addr: usize, len: usize) -> bool {
    let Some(sp) = crate::schedule::current_space() else {
        return false; // 无当前任务空间（boot/空闲）——用户指针必然非法
    };
    // SAFETY: current_space 返回的 NonNull 指向 CURRENT 任务的空间，
    // trap 期间不回收（见 trap.rs ecall 分支注释）。
    let space = unsafe { sp.as_ref() };
    let start = crate::memory::addr::VirtAddr::from_raw(addr);
    if !start.is_user() {
        return false;
    }
    // 覆盖 len 的每一页（含部分页）：任一页未映射 → 非法
    let end = addr.saturating_add(len);
    let mut va = start;
    while va.as_usize() < end {
        if space.translate(va).is_none() {
            return false;
        }
        va = va + crate::memory::PAGE_SIZE;
    }
    true
}

/// 从用户空间拷贝 NUL 结尾字符串到内核堆（`sys_open` 路径解析用）。
///
/// 逐页**先校验映射再读**：任一页未映射（防缺页 terminate 而非 -EFAULT）或
/// 超过 `max` 字节未见 NUL 终止符 → 返回 `None`（调用方映射 -EFAULT）。
/// 返回的内核堆拷贝脱离用户空间，`&str` 借出后可供 `filetable::open` 消费。
///
/// 与 [`user_ptr_valid`] 同理：单 hart 关中断（trap 上下文）下页表不变，
/// 校验与拷贝之间无竞态。
fn user_str(addr: usize, max: usize) -> Option<alloc::string::String> {
    let Some(sp) = crate::schedule::current_space() else {
        return None; // 无当前任务空间（boot/空闲）——用户指针必然非法
    };
    // SAFETY: current_space 返回的 NonNull 指向 CURRENT 任务的空间，
    // trap 期间不回收（见 trap.rs ecall 分支注释）。
    let space = unsafe { sp.as_ref() };
    let start = crate::memory::addr::VirtAddr::from_raw(addr);
    if !start.is_user() {
        return None;
    }
    let mut out = alloc::vec::Vec::with_capacity(max.min(64));
    let mut va = start;
    while out.len() < max {
        // 本页须已映射：逐字节读前先校验，未映射页直接读会触发缺页 terminate
        space.translate(va)?;
        let page_off = va.as_usize() & (crate::memory::PAGE_SIZE - 1);
        let avail = crate::memory::PAGE_SIZE - page_off;
        // SAFETY: 本页已映射且 trap 上下文页表不变；[va, va+avail) 落在
        // 用户区单页内，逐字节读找 NUL。
        let base = va.as_usize() as *const u8;
        for i in 0..avail {
            // SAFETY: i < avail 保证不越出本页
            let b = unsafe { core::ptr::read(base.add(i)) };
            if b == 0 {
                return Some(alloc::string::String::from_utf8_lossy(&out).into_owned());
            }
            out.push(b);
            if out.len() >= max {
                return None; // 路径长度已达上限（≥ max）→ 非法（-EFAULT）
            }
        }
        va = va + crate::memory::PAGE_SIZE;
    }
    None // 超过 max 未见 NUL → 非法（-EFAULT）
}
