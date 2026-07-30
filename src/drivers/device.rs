// 设备注册中心 — 按 TypeId 注册/查找设备实例
//
// 使用 `register::<dyn Trait>(&INSTANCE)` 注册，谁实例化设备时决定。
// 使用 `get::<dyn Trait>()` 获取，谁使用时查询当前活跃的设备。
// `replace::<dyn Trait>(&NEW)` 可在运行时动态切换设备实现。
//
// 胖指针拆装：对于 `&'static dyn Trait`，Rust 在栈上存为 [data_ptr, vtable_ptr]。
// 通过 `&dev as *const usize` 读取两个 word，再通过 [usize; 2] 恢复。

use core::any::TypeId;
use core::mem::MaybeUninit;

use crate::lock::RwLock;

const MAX: usize = 16;

struct Entry {
    type_id: TypeId,
    data: usize,
    vtable: usize,
}

struct EntryList {
    entries: [MaybeUninit<Entry>; MAX],
    len: usize,
}

const EMPTY: MaybeUninit<Entry> = MaybeUninit::uninit();

const fn empty_entries() -> [MaybeUninit<Entry>; MAX] {
    [EMPTY; MAX]
}

static TABLE: RwLock<EntryList> = RwLock::new(EntryList {
    entries: empty_entries(),
    len: 0,
});

/// 注册一个设备实例（追加到末尾，允许同类型多次注册）
pub fn register<T: ?Sized + 'static>(dev: &'static T) {
    let (data, vtable) = unsafe { fat_ptr_parts(dev) };

    let mut list = TABLE.write();
    if list.len >= MAX {
        panic!("device: table full");
    }
    let idx = list.len;
    list.entries[idx].write(Entry {
        type_id: TypeId::of::<T>(),
        data,
        vtable,
    });
    list.len += 1;
}

/// 替换已有同类型设备（未找到则追加）
pub fn replace<T: ?Sized + 'static>(dev: &'static T) {
    let (data, vtable) = unsafe { fat_ptr_parts(dev) };
    let id = TypeId::of::<T>();

    let mut list = TABLE.write();
    for i in 0..list.len {
        let entry = unsafe { list.entries[i].assume_init_ref() };
        if entry.type_id == id {
            list.entries[i].write(Entry {
                type_id: id,
                data,
                vtable,
            });
            return;
        }
    }
    if list.len >= MAX {
        panic!("device: table full");
    }
    let idx = list.len;
    list.entries[idx].write(Entry {
        type_id: id,
        data,
        vtable,
    });
    list.len += 1;
}

/// 获取当前活跃的某类型设备（返回最近注册/替换的实例）
pub fn get<T: ?Sized + 'static>() -> &'static T {
    let id = TypeId::of::<T>();

    let list = TABLE.read();
    for i in (0..list.len).rev() {
        let entry = unsafe { list.entries[i].assume_init_ref() };
        if entry.type_id == id {
            // 从 (data_ptr, vtable_ptr) 恢复 &'static T
            return unsafe { fat_ptr_from_parts(entry.data, entry.vtable) };
        }
    }
    panic!("device: `{}` not registered", core::any::type_name::<T>());
}

/// 清除某类型的所有注册
pub fn unregister<T: ?Sized + 'static>() {
    let id = TypeId::of::<T>();
    let mut list = TABLE.write();
    let mut i = 0;
    while i < list.len {
        let entry = unsafe { list.entries[i].assume_init_ref() };
        if entry.type_id == id {
            let last = list.len - 1;
            if i != last {
                list.entries[i] =
                    MaybeUninit::new(unsafe { list.entries[last].assume_init_read() });
            }
            list.len -= 1;
        } else {
            i += 1;
        }
    }
}

/// 从 `&'static T` 中取出 data_ptr 和 vtable_ptr（T 必须是 unsized 如 dyn Trait）
///
/// `&T` 的引用在栈上是一个 fat pointer（对 unsized T），
/// 布局为 [data_ptr: usize, vtable_ptr: usize]，共 16 字节。
unsafe fn fat_ptr_parts<T: ?Sized>(r: &'static T) -> (usize, usize) {
    // &r 是指向 r 这个 fat pointer 起始处的普通指针（8 字节）
    // reinterpret 成 *const usize 读取第一个 word (data_ptr)
    let pp = &r as *const &'static T as *const usize;
    let data = pp.read();
    let vtable = pp.add(1).read();
    (data, vtable)
}

/// 从 data_ptr 和 vtable_ptr 恢复 `&'static T`
unsafe fn fat_ptr_from_parts<T: ?Sized>(data: usize, vtable: usize) -> &'static T {
    // 在栈上摆出 [data, vtable] 布局的 16 字节，按 &T 读出来
    let storage: [usize; 2] = [data, vtable];
    let ptr = &storage as *const [usize; 2] as *const &'static T;
    ptr.read()
}
