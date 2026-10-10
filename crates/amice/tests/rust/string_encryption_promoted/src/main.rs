// Issue #96 regression shape: promoted &'static [u8] literals inside a
// static struct initializer.
//
// rustc encodes such constants as relocation blobs: the fat-pointer lengths
// are [8 x i8] byte arrays and the scalar metadata is a [N x i8] blob inside
// the packed struct constant. StringEncryption currently treats those byte
// arrays as encryptable strings (LLVMIsConstantString accepts ANY [N x i8]
// constant array) and, combined with the per-field initializer rebuild, ends
// up XORing the length fields exactly once at runtime.
//
// Expected output (unobfuscated):
//   key1.len() = 16
//   key2.len() = 16
//   key len = 16
pub struct Sealed {
    pub key1: &'static [u8],
    pub key2: &'static [u8],
    pub rounds: u8,
    pub seed: u64,
    pub len_plain: u32,
    pub len_mask: u32,
    pub data: &'static [u8],
}

static SEALED: Sealed = Sealed {
    key1: &[
        0x37, 0x92, 0x4c, 0xa1, 0x0e, 0x5d, 0xc8, 0x33, 0x7f, 0x1a, 0xb6, 0x49, 0x90, 0x2d, 0xe5, 0x58,
    ],
    key2: &[
        0x5c, 0x21, 0xe7, 0x08, 0x93, 0x4a, 0x36, 0xbf, 0x60, 0x15, 0xda, 0x47, 0x01, 0xbe, 0x72, 0x39,
    ],
    rounds: 4,
    seed: 0x9e3779b97f4a7c15,
    len_plain: 32,
    len_mask: 0xdeadbeef,
    data: &[0x11u8; 48],
};

fn real_key(key1: &[u8], key2: &[u8]) -> Vec<u8> {
    key1.iter().zip(key2).map(|(a, b)| a ^ b).collect()
}

fn main() {
    println!("key1.len() = {}", SEALED.key1.len());
    println!("key2.len() = {}", SEALED.key2.len());
    let key = real_key(SEALED.key1, SEALED.key2);
    println!("key len = {}", key.len());
}
