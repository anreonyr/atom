# fs 模块 API 契约（自制极简文件系统）

> 本文件是 `src/file/fs/` 模块的权威 API 契约：on-disk 布局、公共签名、引导流程
> 与约定。代码变更须与本文档保持同步（对照 CLAUDE.md「API naming conventions」）。
> 本文档是 `docs/src/template.md` 模板的一个实例。

## 1. 职责

在 `hal::BlockDevice` 之上实现可持久化的极简 FS：superblock + inode 表 + 数据位图
+ 数据区 + 目录。`mount` 读 superblock（magic 非法则 format），返回 `/data` 挂载点
Inode（`dir = FsDir` 动态目录能力）；文件/子目录经 `Directory::lookup_child`/
`create_child` 运行时**懒物化** `&'static Inode`。

边界：

- **不做通用 VFS**：目录/文件组织契约（`Directory`/`File`）在 `file/vfs` + `file/ops`；
  本模块是块设备上的具体 FS（Linux ext2 的极简对应物）。
- **不做块设备驱动**：只消费 `hal::BlockDevice` 能力契约（依赖方向 `file::fs → hal::block`）。
- **不做缓存/日志 FS**：所有写 write-through 到块设备——无需 fsync；重启 mount 读回。

## 2. 引导流程

```
init Phase 2：hal::block::get() → Some(dev) → file::fs::mount(dev)
  mount：read_super(block 0)
    ├─ magic == "ATOM" → 复用（不格式化）
    └─ 否则 → format(dev)（写 superblock + 清零 inode 表/位图 + 建 root ino 1）
  → 物化 FsDir(root) → InodeBuilder::new("data", Directory).with_dir(root).build()
init 把 /data 作为根 Inode 的子节点（/dev 与 /data 两棵子树）→ set_root

运行期：lookup/open 静态 children 未命中 → Inode.dir.lookup_child（磁盘懒物化）；
create(1009) → 父目录 dir.create_child（磁盘建 inode + 目录项）→ open 返回 fd。
```

## 3. on-disk 格式（FS 块 = 设备块 = 512B）

```
block 0               superblock SuperBlock（24B：magic="ATOM"、inode_count、inode_table、
                        bitmap、data_start、block_count）
block 1..1+8          inode 表（DiskInode 64B → 8 个/块；ino 0 无效，root=ino 1）
block 9..9+bmb-1      数据位图（1 bit/数据块；32M 盘 bmb=16）
block data_start..    数据区
```

```rust
#[repr(C)] struct SuperBlock { magic: [u8;4], inode_count: u32, inode_table: u32,
                               bitmap: u32, data_start: u32, block_count: u32 }
#[repr(C)] struct DiskInode { ty: u32, size: u32, direct: [u32; 12], reserved: [u32; 2] }
#[repr(C)] struct DirEntry  { ino: u32, name: [u8; 28] }   // ino==0 空槽，name NUL 结尾
```

- 文件最大 = 12 × 512 = 6 KB（`DIRECT` 直接块指针，探针足够；超限 → `InvalidArg`）。
- 目录项 16 个/块；目录 size = 已用目录项字节数（追加递增）。
- inode 类型：`TY_FREE=0` / `TY_FILE=1` / `TY_DIR=2`。

## 4. 公共 API 面

### 4.1 函数（`file/fs/mod.rs`）

| API | 签名 | 语义与契约 |
|-----|------|-----------|
| `mount` | `pub fn mount(device: &'static dyn BlockDevice) -> Option<&'static Inode>` | 读 superblock（magic 非法 → format），返回 `/data` 挂载点 Inode。无有效设备/格式化失败返回 None（init 不加 /data，系统无 FS 仍可运行）。**不 panic** |

### 4.2 内部组件

| 组件 | 实现 | 语义 |
|------|------|------|
| `FsDir`（`dir.rs`） | `Directory` | 磁盘目录数据 + 已物化子 Inode 缓存（`SpinLock<Vec<(&'static str, &'static Inode)>>`）；`lookup_child`（缓存→磁盘→物化）、`create_child`（alloc_inode + 追加目录项写盘 + 物化）、`readdir`（"name\n" 列举） |
| `FsFile`（`file.rs`） | `File` | `read/write(offset, …)` 随机访问 on-disk 直接块；write 超 size 追加分配数据块（扫位图）+ 部分块 RMW + 写回 inode（size/direct 持久）；`size()` 报文件大小；seek 支持 `End` |
| 低层辅助（mod.rs） | — | `read_inode`/`write_inode`/`alloc_inode`/`alloc_block`/`find_dirent`/`append_dir_entry`/`materialize`；`SUPER`（OnceLock）缓存布局 |

### 4.3 VFS 配合改动

| 位置 | 改动 |
|------|------|
| `file/vfs/inode.rs` | 新增 `Directory` trait（`lookup_child`/`create_child`/`readdir`，返回 `&'static Inode`）+ `Inode.dir` + `with_dir`；`lookup` 静态 children 未命中 → `dir.lookup_child` |
| `file/vfs/filetable.rs` | `readdir` 委托 `dir.readdir`；`create(path, flags)`（拆父+名 → `dir.create_child` → `open_inode`）；`open_inode` 抽取 |
| `runtime/envcall.rs` | `CREATE = 1009`：`sys_create` → `filetable::create` → 返回 fd |

## 5. 命名框架（对照 CLAUDE.md「API naming conventions」）

| 约定 | 本模块落点 |
|------|-----------|
| 读写对称 | `read_inode`/`write_inode`；读方法 = 字段名（`size()` 无 `get_`） |
| 查询 API 用查找动词 | `find_dirent -> Option<u32>`、`lookup_child -> Option<&'static Inode>` |
| 错误码语义即行为 | `FileError::NotFound`（路径不存在）/`NotDirectory`（父非动态目录）/`InvalidArg`（名字超长/文件超上限/磁盘满） |
| 不用缩写 | `inode_count`/`data_start`/`block_count` 全称；`ino` 用于 on-disk 字段（inode 号的固有缩写，模块内一致） |

## 6. 生命周期与并发

- **持锁 = 轮询**：FsDir/FsFile 操作全程持 `SpinLock<DiskInode>`（关中断，SIE=0）→
  块 I/O 走 `wait_event` 轮询分支——**从设计上杜绝"持锁 wfi"死锁**。嵌套锁序固定
  （FS 锁 → virtio 队列锁），单 hart 无环。
- **懒物化泄漏**：运行时物化的子 Inode 经 `Box::leak` 永不回收（教学内核接受；
  目录项本身在磁盘上是唯一事实源）。
- **write-through 持久**：数据块/inode 表/位图每次写都落盘；重启 mount 从磁盘
  重建（目录项 → 懒物化）。文件追加分配的数据块不主动清零（RMW 覆盖 + size 裁剪，
  未覆盖区域不暴露）。
- **`unsafe` 边界**：repr(C) 结构直接读写块缓冲（`read_unaligned`/`copy_nonoverlapping`），
  偏移由固定布局保证（inode 槽 ≤ 8、目录项 ≤ 16）。

## 7. 变更记录

- 2026-08-08：M4——`file/fs` 落地（on-disk 布局 + mount/format + FsDir/FsFile +
  Directory 动态目录 + create(1009)）；VFS `Inode.dir` + lookup/readdir 动态回退；
  init 根组装 /dev + /data。验收：U 程序 create/write `/data/msg.txt` → 重启 → 读回一致。
