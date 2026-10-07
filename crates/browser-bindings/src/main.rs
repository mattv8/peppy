fn main() {
    // Link the library into the executable so Emscripten can retain the exported RPC ABI.
    std::hint::black_box(
        peppy_browser_bindings::peppy_browser_alloc as extern "C" fn(usize) -> *mut u8,
    );
    std::hint::black_box(
        peppy_browser_bindings::peppy_browser_dispatch
            as unsafe extern "C" fn(*const u8, usize) -> *mut u8,
    );
    std::hint::black_box(
        peppy_browser_bindings::peppy_browser_free_request as unsafe extern "C" fn(*mut u8, usize),
    );
    std::hint::black_box(
        peppy_browser_bindings::peppy_browser_free_response as unsafe extern "C" fn(*mut u8),
    );
}
