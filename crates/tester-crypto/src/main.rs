fn main() {
    std::hint::black_box(
        peppy_tester_crypto::peppy_tester_crypto_alloc as extern "C" fn(usize) -> *mut u8,
    );
    std::hint::black_box(
        peppy_tester_crypto::peppy_tester_crypto_dispatch
            as unsafe extern "C" fn(*const u8, usize) -> *mut u8,
    );
    std::hint::black_box(
        peppy_tester_crypto::peppy_tester_crypto_free_request
            as unsafe extern "C" fn(*mut u8, usize),
    );
    std::hint::black_box(
        peppy_tester_crypto::peppy_tester_crypto_free_response as unsafe extern "C" fn(*mut u8),
    );
}
