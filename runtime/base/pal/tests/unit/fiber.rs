#![allow(clippy::undocumented_unsafe_blocks)]

use std::ffi::c_void;
use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering};

use marlin_pal::fiber::{self, Context, Stack};
use marlin_pal::thread;

/// Shared between a test and its fiber.
struct Pair {
    caller: Context,
    fiber: Context,
    value: u64,
}

unsafe extern "C" fn ping(arg: *mut c_void) -> ! {
    let pair = arg.cast::<Pair>();
    loop {
        unsafe {
            (*pair).value += 1;
            fiber::switch(&raw mut (*pair).fiber, (*pair).caller);
        }
    }
}

#[test]
fn ping_pong() {
    let stack = Stack::new(64 << 10).unwrap();
    let mut pair = Pair {
        caller: Context::NULL,
        fiber: Context::NULL,
        value: 0,
    };
    let p = &raw mut pair;
    unsafe {
        (*p).fiber = fiber::make_context(&stack, ping, p.cast());
        for i in 1..=100_000 {
            fiber::switch(&raw mut (*p).caller, (*p).fiber);
            assert_eq!((*p).value, i);
        }
    }
}

/// Clobbers every register it can, integer and vector, then switches back.
unsafe extern "C" fn clobber(arg: *mut c_void) -> ! {
    let pair = arg.cast::<Pair>();
    loop {
        let mut acc = [0.0f64; 16];
        for (i, a) in acc.iter_mut().enumerate() {
            *a = black_box(i as f64 * 1.37 + 0.5).sqrt();
        }
        let ints: [u64; 12] =
            core::array::from_fn(|i| black_box(i as u64).wrapping_mul(0x9E37_79B9));
        unsafe {
            (*pair).value = black_box(acc.iter().sum::<f64>() as u64 + ints.iter().sum::<u64>());
            fiber::switch(&raw mut (*pair).fiber, (*pair).caller);
        }
    }
}

#[test]
fn callee_saved_state_survives() {
    let stack = Stack::new(64 << 10).unwrap();
    let mut pair = Pair {
        caller: Context::NULL,
        fiber: Context::NULL,
        value: 0,
    };
    let p = &raw mut pair;
    unsafe {
        (*p).fiber = fiber::make_context(&stack, clobber, p.cast());
    }
    for round in 0..1000u64 {
        // Many values live across the switch, so some sit in callee-saved registers.
        let f: [f64; 12] = core::array::from_fn(|i| black_box((round + i as u64) as f64 * 0.25));
        let n: [u64; 8] = core::array::from_fn(|i| black_box(round * 31 + i as u64));
        unsafe { fiber::switch(&raw mut (*p).caller, (*p).fiber) };
        for (i, &x) in f.iter().enumerate() {
            assert_eq!(x, (round + i as u64) as f64 * 0.25);
        }
        for (i, &x) in n.iter().enumerate() {
            assert_eq!(x, round * 31 + i as u64);
        }
    }
}

#[inline(never)]
fn recurse(depth: usize) -> u64 {
    // 32 KiB frames: on Windows each one goes through a stack probe that checks the TEB bounds.
    let mut frame = [0u8; 32 << 10];
    black_box(&mut frame);
    frame[depth % frame.len()] = depth as u8;
    if depth == 0 {
        frame[0] as u64
    } else {
        recurse(depth - 1) + black_box(&frame)[depth % frame.len()] as u64
    }
}

unsafe extern "C" fn deep(arg: *mut c_void) -> ! {
    let pair = arg.cast::<Pair>();
    unsafe {
        // ~800 KiB of stack, plus std formatting and allocation on the fiber stack.
        (*pair).value = recurse(24);
        let text = format!("{:?}", (0..100).collect::<Vec<_>>());
        (*pair).value += text.len() as u64;
        fiber::switch(&raw mut (*pair).fiber, (*pair).caller);
    }
    unreachable!()
}

#[test]
fn deep_stack_and_std_on_a_fiber() {
    let stack = Stack::new(1 << 20).unwrap();
    let mut pair = Pair {
        caller: Context::NULL,
        fiber: Context::NULL,
        value: 0,
    };
    let p = &raw mut pair;
    unsafe {
        (*p).fiber = fiber::make_context(&stack, deep, p.cast());
        fiber::switch(&raw mut (*p).caller, (*p).fiber);
        let expected = recurse(24) + format!("{:?}", (0..100).collect::<Vec<_>>()).len() as u64;
        assert_eq!((*p).value, expected);
    }
}

static FIRST_THREAD: AtomicU64 = AtomicU64::new(0);
static SECOND_THREAD: AtomicU64 = AtomicU64::new(0);

unsafe extern "C" fn migrate(arg: *mut c_void) -> ! {
    let pair = arg.cast::<Pair>();
    unsafe {
        FIRST_THREAD.store(thread::current_id(), Ordering::Relaxed);
        fiber::switch(&raw mut (*pair).fiber, (*pair).caller);
        // Resumed by another thread, which saved its own context into `caller`.
        SECOND_THREAD.store(thread::current_id(), Ordering::Relaxed);
        let _ = recurse(4);
        fiber::switch(&raw mut (*pair).fiber, (*pair).caller);
    }
    unreachable!()
}

struct SendPtr(*mut Pair);
unsafe impl Send for SendPtr {}

#[test]
fn fibers_migrate_between_threads() {
    let stack = Stack::new(512 << 10).unwrap();
    let mut pair = Pair {
        caller: Context::NULL,
        fiber: Context::NULL,
        value: 0,
    };
    let p = SendPtr(&raw mut pair);
    unsafe {
        (*p.0).fiber = fiber::make_context(&stack, migrate, p.0.cast());
        fiber::switch(&raw mut (*p.0).caller, (*p.0).fiber);
    }
    std::thread::spawn(move || unsafe {
        let p = p;
        fiber::switch(&raw mut (*p.0).caller, (*p.0).fiber);
    })
    .join()
    .unwrap();
    let (a, b) = (
        FIRST_THREAD.load(Ordering::Relaxed),
        SECOND_THREAD.load(Ordering::Relaxed),
    );
    assert!(a != 0 && b != 0 && a != b);
}

/// Fiber A hands over to fiber B, which returns to the test.
struct Chain {
    caller: Context,
    a: Context,
    b: Context,
    order: Vec<char>,
}

unsafe extern "C" fn chain_a(arg: *mut c_void) -> ! {
    let chain = arg.cast::<Chain>();
    unsafe {
        (*chain).order.push('a');
        fiber::switch(&raw mut (*chain).a, (*chain).b);
    }
    unreachable!()
}

unsafe extern "C" fn chain_b(arg: *mut c_void) -> ! {
    let chain = arg.cast::<Chain>();
    unsafe {
        (*chain).order.push('b');
        fiber::switch(&raw mut (*chain).b, (*chain).caller);
    }
    unreachable!()
}

#[test]
fn fiber_to_fiber() {
    let (sa, sb) = (Stack::new(64 << 10).unwrap(), Stack::new(64 << 10).unwrap());
    let mut chain = Chain {
        caller: Context::NULL,
        a: Context::NULL,
        b: Context::NULL,
        order: vec![],
    };
    let c = &raw mut chain;
    unsafe {
        (*c).a = fiber::make_context(&sa, chain_a, c.cast());
        (*c).b = fiber::make_context(&sb, chain_b, c.cast());
        fiber::switch(&raw mut (*c).caller, (*c).a);
        assert_eq!((*c).order, ['a', 'b']);
    }
}

#[test]
fn stack_geometry() {
    let stack = Stack::new(100_000).unwrap();
    let page = marlin_pal::vmem::page_size();
    assert!(stack.size() >= 100_000);
    assert_eq!(stack.limit() as usize - stack.base() as usize, page);
    assert_eq!(stack.top() as usize - stack.limit() as usize, stack.size());
}
