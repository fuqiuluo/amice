#[no_mangle]
#[inline(never)]
pub extern "C" fn vm_rust_mix(a: i32, b: i32) -> i32 {
    let mut acc = (a + b) * 3;
    let mut i = 0;
    while i < 4 {
        acc = (acc ^ (b + i)) + a;
        i += 1;
    }
    acc - b
}
