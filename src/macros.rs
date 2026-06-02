// CSR 寄存器操作宏：读取、写入、置位

#[macro_export]
macro_rules! csr_read {
    ($csr:expr) => {{
        let r: usize;
        unsafe { asm!(concat!("csrr {o}, ", stringify!($csr)), o = out(reg) r) }
        r
    }};
}

#[macro_export]
macro_rules! csr_write {
    ($csr:expr, $val:expr) => {
        unsafe { asm!(concat!("csrw ", stringify!($csr), ", {v}"), v = in(reg) $val) }
    };
}

#[macro_export]
macro_rules! csr_set {
    ($csr:expr, $bits:expr) => {
        unsafe { asm!(concat!("csrs ", stringify!($csr), ", {v}"), v = in(reg) $bits) }
    };
}
