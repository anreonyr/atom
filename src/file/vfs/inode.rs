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

use crate::file::ops::File;

// ── InodeType ─────────────────────────────────────────────

/// 文件系统节点类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // File 变体为真实文件系统预留（当前 devfs-only，无构造点）
pub enum InodeType {
    /// 目录 — 包含子节点
    Directory,
    /// 常规文件 — 真实文件系统节点（预留；当前仅 devfs 使用字节设备）
    File,
    /// 字节设备 — 流式传输，无 seek（UART、console）
    ByteDevice,
}

// ── Inode ─────────────────────────────────────────────────

/// 文件系统节点。
///
/// 每个 Inode 代表文件系统树中的一个命名对象。设备驱动通过 trait 对象引用
/// 嵌入节点中——文件能力由单个 `&dyn File` 承载。
///
/// Inode 在引导期创建后不可变，所有字段均为共享引用。
pub struct Inode {
    /// 节点名称（如 "console"、"dev"）
    pub name: &'static str,
    /// 节点类型
    pub inode_type: InodeType,
    /// 文件能力实现（目录节点为 None）
    pub file: Option<&'static dyn File>,
    /// 子节点（仅目录类型非空）
    pub children: &'static [&'static Inode],
}

impl fmt::Debug for Inode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Inode")
            .field("name", &self.name)
            .field("inode_type", &self.inode_type)
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
    children: Vec<&'static Inode>,
}

impl InodeBuilder {
    /// 创建新的构造器。
    pub fn new(name: &'static str, inode_type: InodeType) -> Self {
        Self {
            name,
            inode_type,
            file: None,
            children: Vec::new(),
        }
    }

    /// 设置文件能力实现（设备驱动实例、devfs 节点等）。
    pub fn with_file(mut self, f: &'static dyn File) -> Self {
        self.file = Some(f);
        self
    }

    /// 添加子节点（仅目录类型使用）。
    pub fn with_child(mut self, child: &'static Inode) -> Self {
        self.children.push(child);
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
            children,
        }))
    }
}

// ── 路径解析 ──────────────────────────────────────────────

/// 从根节点按路径查找目标 Inode（Linux `inode_operations::lookup` 对应物）。
///
/// 无分配、纯函数——按 '/' 分割路径组件，从根开始逐级在
/// 目录节点的 `children` 列表中线性查找。
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

        // 查找匹配名称的子节点
        current = current.children.iter().find(|c| c.name == part)?;
    }

    Some(current)
}
