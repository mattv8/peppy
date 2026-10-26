#![cfg(target_os = "macos")]

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use block::ConcreteBlock;

struct DropWitness(Arc<AtomicUsize>);

impl Drop for DropWitness {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn concrete_block_uses_system_runtime_for_copy_clone_call_and_drop() {
    let drops = Arc::new(AtomicUsize::new(0));
    let witness = DropWitness(Arc::clone(&drops));
    let block = ConcreteBlock::new(move |value: i32| {
        let _ = &witness;
        value + 7
    });
    let copied = block.copy();

    assert_eq!(unsafe { copied.call((5,)) }, 12);

    let cloned = copied.clone();
    assert_eq!(unsafe { cloned.call((9,)) }, 16);

    drop(copied);
    assert_eq!(drops.load(Ordering::SeqCst), 0);

    drop(cloned);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}
