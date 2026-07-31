// 设备注册中心 — 按 (TypeId, name) 注册/查找具体设备实例
//
// 只存具体类型（Sized），不存 trait 对象。
// 调用方从注册中心取到具体实例后自己做 &T → &dyn Trait 的 coercion。

use alloc::vec::Vec;
use core::any::TypeId;

use crate::lock::RwLock;

struct Entry {
    type_id: TypeId,
    name: &'static str,
    ptr: usize,
}

static TABLE: RwLock<Vec<Entry>> = RwLock::new(Vec::new());

/// 注册一个设备实例（追加到末尾）。
///
/// 同一 (TypeId, name) 可多次注册，`get()` 返回最新注册的那个。
pub fn register<T: 'static>(dev: &'static T, name: &'static str) {
    TABLE.write().push(Entry {
        type_id: TypeId::of::<T>(),
        name,
        ptr: dev as *const T as usize,
    });
}

/// 按类型和名称查询最新注册的设备实例。
pub fn get<T: 'static>(name: &str) -> Option<&'static T> {
    let id = TypeId::of::<T>();
    let list = TABLE.read();
    let entry = list
        .iter()
        .rev()
        .find(|e| e.type_id == id && e.name == name)?;
    // SAFETY: register() stores &'static T as ptr; TypeId matched above; reference never invalidated.
    Some(unsafe { &*(entry.ptr as *const T) })
}

/// 替换同名实例（未找到则追加）。
pub fn replace<T: 'static>(dev: &'static T, name: &'static str) {
    let id = TypeId::of::<T>();
    let ptr = dev as *const T as usize;
    let mut list = TABLE.write();
    if let Some(idx) = list.iter().rposition(|e| e.type_id == id && e.name == name) {
        list[idx] = Entry {
            type_id: id,
            name,
            ptr,
        };
    } else {
        list.push(Entry {
            type_id: id,
            name,
            ptr,
        });
    }
}

/// 注销指定名称的设备实例。
pub fn unregister<T: 'static>(name: &str) {
    let id = TypeId::of::<T>();
    let mut list = TABLE.write();
    if let Some(idx) = list.iter().rposition(|e| e.type_id == id && e.name == name) {
        list.swap_remove(idx);
    }
}

/// 遍历某类型的所有已注册实例。
/// 先收集指针再遍历，避免在持有锁时调用回调。
pub fn for_each<T: 'static>(mut f: impl FnMut(&'static T)) {
    let id = TypeId::of::<T>();
    let collected: Vec<usize> = {
        let list = TABLE.read();
        list.iter()
            .filter(|e| e.type_id == id)
            .map(|e| e.ptr)
            .collect()
    };
    for ptr in collected {
        // SAFETY: register() stores &'static T; TypeId matched during collection; reference never invalidated.
        f(unsafe { &*(ptr as *const T) });
    }
}

/// 统计某类型已注册的实例数量。
pub fn count<T: 'static>() -> usize {
    let id = TypeId::of::<T>();
    TABLE.read().iter().filter(|e| e.type_id == id).count()
}
