// Inode — 文件系统节点抽象
//
// Inode 是 VFS 的核心抽象，代表文件系统树中的一个节点。
// 引导期通过 InodeBuilder 创建并以 Box::leak 获得 'static 生命周期，
// 此后不可变——读取路径无锁。
//
// 文件能力收敛为单个 `&dyn File`（Linux 驱动注册 fops 给 VFS 的对应物）；
// 目录节点 file 为 None，children 非空。

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::fmt;

use crate::file::ops::{File, Result};

// ── InodeType ─────────────────────────────────────────────

/// 文件系统节点类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // File 变体为真实文件系统预留（当前仅 devfs 使用字节设备）
pub enum InodeType {
    /// 目录 — 包含子节点
    Directory,
    /// 常规文件 — 真实文件系统节点（预留；当前仅 devfs 使用字节设备）
    File,
    /// 字节设备 — 流式传输，无 seek（UART、console）
    ByteDevice,
    /// 符号链接 — 指向同目录另一节点的别名（`/dev/console → consoleN`）
    Symlink,
}

// ── 动态目录能力 ───────────────────────────────────────────

/// 动态目录能力 — 运行时在磁盘上物化子 Inode（Linux `inode_operations` 的目录部分）。
///
/// 与 devfs 的静态 `children` 不同：真实文件系统（如 file::fs）的目录子节点
/// 在运行时创建/查询，经本 trait 在磁盘上解析并**懒物化** `&'static Inode`
/// （Box::leak）。定义在本文件（返回 `&'static Inode`）避免 `ops.rs` 零依赖
/// 叶子反向引用 `Inode`。
pub trait Directory: Send + Sync {
    /// 按名解析子节点（磁盘上不存在返回 `None`）。
    fn lookup_child(&self, name: &str) -> Option<&'static Inode>;

    /// 创建子节点（磁盘上建 inode + 目录项），返回新 Inode。
    fn create_child(&self, name: &str, ty: InodeType) -> Result<&'static Inode>;

    /// 一次性列举子节点名，以 `"name\n"` 写入 buf，返回字节数（buf 满整项截断）。
    fn readdir(&self, buf: &mut [u8]) -> Result<usize>;
}

// ── Inode ─────────────────────────────────────────────────

/// 文件系统节点。
///
/// 每个 Inode 代表文件系统树中的一个命名对象。设备驱动通过 trait 对象引用
/// 嵌入节点中——文件能力由单个 `&dyn File` 承载。
///
/// 目录的两种形态：
///   - 静态 `children`（devfs）：引导期枚举构建后不可变，零锁；
///   - 动态 `dir` 能力（file::fs）：运行时磁盘解析 + 懒物化子 Inode。
///
/// Inode 在引导期创建后不可变，所有字段均为共享引用。
pub struct Inode {
    /// 节点名称（如 "console"、"dev"、"data"）
    pub name: &'static str,
    /// 节点类型
    pub inode_type: InodeType,
    /// 文件能力实现（目录节点为 None）
    pub file: Option<&'static dyn File>,
    /// 动态目录能力（静态 children 目录为 None；FS 目录用磁盘懒物化）
    pub dir: Option<&'static dyn Directory>,
    /// 子节点（仅目录类型非空；静态形态）
    pub children: &'static [&'static Inode],
    /// 符号链接目标（仅 Symlink 类型非 None）— 同目录相对名（如 "console0"）
    pub target: Option<&'static str>,
}

impl fmt::Debug for Inode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Inode")
            .field("name", &self.name)
            .field("inode_type", &self.inode_type)
            .field("has_dir", &self.dir.is_some())
            .field(
                "children",
                &self.children.iter().map(|c| c.name).collect::<Vec<_>>(),
            )
            .finish()
    }
}

// ── InodeBuilder ──────────────────────────────────────────

/// Inode 构造器 — 引导期链式构造。
///
/// # 示例
///
/// ```ignore
/// let console = InodeBuilder::new("console", InodeType::ByteDevice)
///     .with_file(uart)
///     .build();
/// ```
pub struct InodeBuilder {
    name: &'static str,
    inode_type: InodeType,
    file: Option<&'static dyn File>,
    dir: Option<&'static dyn Directory>,
    children: Vec<&'static Inode>,
    target: Option<&'static str>,
}

impl InodeBuilder {
    /// 创建新的构造器。
    pub fn new(name: &'static str, inode_type: InodeType) -> Self {
        Self {
            name,
            inode_type,
            file: None,
            dir: None,
            children: Vec::new(),
            target: None,
        }
    }

    /// 设置文件能力实现（设备驱动实例、devfs 节点等）。
    pub fn with_file(mut self, f: &'static dyn File) -> Self {
        self.file = Some(f);
        self
    }

    /// 设置动态目录能力（file::fs 目录；静态 children 目录不需要）。
    pub fn with_dir(mut self, dir: &'static dyn Directory) -> Self {
        self.dir = Some(dir);
        self
    }

    /// 添加子节点（仅目录类型使用）。
    pub fn with_child(mut self, child: &'static Inode) -> Self {
        self.children.push(child);
        self
    }

    /// 设置符号链接目标（Symlink 节点）— 同目录相对名。
    pub fn with_target(mut self, target: &'static str) -> Self {
        self.target = Some(target);
        self
    }

    /// 消费构造器，在堆上分配 Inode 并泄漏获得 'static 引用。
    pub fn build(self) -> &'static Inode {
        let children: &'static [&'static Inode] = if self.children.is_empty() {
            &[]
        } else {
            Vec::leak(self.children)
        };

        Box::leak(Box::new(Inode {
            name: self.name,
            inode_type: self.inode_type,
            file: self.file,
            dir: self.dir,
            children,
            target: self.target,
        }))
    }
}

// ── 路径解析 ──────────────────────────────────────────────

/// 符号链接最大跟随深度 — 防御链接环（同目录互相引用）造成的无限循环。
const MAX_SYMLINK_DEPTH: usize = 8;

/// 跟随符号链接节点 — 在 `dir`（符号链接所在目录）的 `children` 中按
/// `target` 相对名解析目标节点，若目标仍是符号链接则继续，深度超过
/// [`MAX_SYMLINK_DEPTH`]（环或过深）或 target 缺失/目标不存在返回 `None`。
fn follow<'a>(dir: &'a Inode, node: &'a Inode) -> Option<&'a Inode> {
    let mut cur = node;
    let mut depth = 0;
    while cur.inode_type == InodeType::Symlink {
        if depth >= MAX_SYMLINK_DEPTH {
            return None;
        }
        let target = cur.target?;
        cur = dir.children.iter().find(|c| c.name == target)?;
        depth += 1;
    }
    Some(cur)
}

/// 从根节点按路径查找目标 Inode（Linux `inode_operations::lookup` 对应物）。
///
/// 无分配、纯函数——按 '/' 分割路径组件，从根开始逐级在
/// 目录节点的 `children` 列表中线性查找；命中 Symlink 节点时在
/// 同目录按 target 相对名跟随（支持末尾与中间组件）。
///
/// 暂不支持 `..`（无 parent 指针）。
pub fn lookup<'a>(root: &'a Inode, path: &str) -> Option<&'a Inode> {
    let path = path.trim_start_matches('/');
    if path.is_empty() {
        return Some(root);
    }

    let mut current = root;
    for part in path.split('/') {
        // 跳过空组件（连续斜杠）和当前目录
        if part.is_empty() || part == "." {
            continue;
        }

        // 查找匹配名称的子节点：静态 children 未命中 → 动态目录能力
        // （file::fs 磁盘懒物化子 Inode）。
        let node = current
            .children
            .iter()
            .find(|c| c.name == part)
            .copied()
            .or_else(|| current.dir.as_ref().and_then(|d| d.lookup_child(part)))?;
        current = if node.inode_type == InodeType::Symlink {
            follow(current, node)?
        } else {
            node
        };
    }

    Some(current)
}
