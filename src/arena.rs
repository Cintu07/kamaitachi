//! Arenas with byte budgets, the counters :mem reads, the spsc rings between the two
//! threads, and the counting allocator that proves the frame loop never allocates.

use core::cell::{Cell, UnsafeCell};
use core::mem::MaybeUninit;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

// ---------------------------------------------------------------- arena counters

/// The five arenas from the prd. Each reports bytes in use, its high-water mark and budget.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(usize)]
pub enum ArenaId {
    Frame = 0,
    Net = 1,
    TabText = 2,
    Store = 3,
    Pixels = 4,
}

pub const ARENA_NAMES: [&str; 5] = ["frame", "net", "tab text", "store", "pixels"];
pub const KB: usize = 1024;
pub const MB: usize = 1024 * 1024;
pub const BUDGETS: [usize; 5] = [256 * KB, 4 * 64 * KB, 10 * MB, 4 * MB, 8 * MB];
pub const TAB_TEXT_BUDGET: usize = MB;
pub const NET_CONN_BUDGET: usize = 64 * KB;

pub struct ArenaStat {
    pub used: AtomicUsize,
    pub high: AtomicUsize,
}

pub static STATS: [ArenaStat; 5] = [const { ArenaStat { used: AtomicUsize::new(0), high: AtomicUsize::new(0) } }; 5];

pub fn add(id: ArenaId, bytes: isize) {
    let s = &STATS[id as usize];
    let now = if bytes >= 0 {
        s.used.fetch_add(bytes as usize, Ordering::Relaxed) + bytes as usize
    } else {
        s.used.fetch_sub((-bytes) as usize, Ordering::Relaxed) - (-bytes) as usize
    };
    s.high.fetch_max(now, Ordering::Relaxed);
}

pub fn set(id: ArenaId, bytes: usize) {
    let s = &STATS[id as usize];
    s.used.store(bytes, Ordering::Relaxed);
    s.high.fetch_max(bytes, Ordering::Relaxed);
}

pub fn used(id: ArenaId) -> usize {
    STATS[id as usize].used.load(Ordering::Relaxed)
}

pub fn high(id: ArenaId) -> usize {
    STATS[id as usize].high.load(Ordering::Relaxed)
}

// ---------------------------------------------------------------- bump arena

/// A fixed buffer handed out front to back and reset in one step. The frame arena is one
/// of these: layout of the visible rows lives here and dies at the end of the frame.
pub struct Bump {
    buf: Box<[u8]>,
    used: usize,
    id: ArenaId,
}

impl Bump {
    pub fn new(id: ArenaId) -> Bump {
        Bump { buf: vec![0u8; BUDGETS[id as usize]].into_boxed_slice(), used: 0, id }
    }

    /// A zeroed slice of `n` values of a plain-data type, or None when over budget.
    pub fn slice<T: Copy + Default>(&mut self, n: usize) -> Option<&mut [T]> {
        let align = core::mem::align_of::<T>();
        let start = (self.buf.as_ptr() as usize + self.used).next_multiple_of(align) - self.buf.as_ptr() as usize;
        let bytes = n * core::mem::size_of::<T>();
        if start + bytes > self.buf.len() {
            return None;
        }
        self.used = start + bytes;
        set(self.id, self.used);
        // SAFETY: in bounds, aligned, T is Copy plain data, and the borrow ties it to &mut self
        let s = unsafe { core::slice::from_raw_parts_mut(self.buf.as_mut_ptr().add(start) as *mut T, n) };
        for v in s.iter_mut() {
            *v = T::default();
        }
        Some(s)
    }

    pub fn reset(&mut self) {
        self.used = 0;
        set(self.id, 0);
    }
}

// ---------------------------------------------------------------- spsc ring

struct RingInner<T> {
    slots: Box<[UnsafeCell<MaybeUninit<T>>]>,
    head: AtomicUsize, // next to read
    tail: AtomicUsize, // next to write
}

unsafe impl<T: Send> Sync for RingInner<T> {}
unsafe impl<T: Send> Send for RingInner<T> {}

pub struct Producer<T>(Arc<RingInner<T>>);
pub struct Consumer<T>(Arc<RingInner<T>>);

/// A lock-free single-producer single-consumer ring with a fixed capacity.
pub fn ring<T>(cap: usize) -> (Producer<T>, Consumer<T>) {
    let cap = cap.next_power_of_two();
    let slots = (0..cap).map(|_| UnsafeCell::new(MaybeUninit::uninit())).collect();
    let inner = Arc::new(RingInner { slots, head: AtomicUsize::new(0), tail: AtomicUsize::new(0) });
    (Producer(inner.clone()), Consumer(inner))
}

impl<T> Producer<T> {
    /// Pushes a value, or hands it back when the ring is full.
    pub fn push(&self, v: T) -> Result<(), T> {
        let r = &*self.0;
        let tail = r.tail.load(Ordering::Relaxed);
        let head = r.head.load(Ordering::Acquire);
        if tail.wrapping_sub(head) == r.slots.len() {
            return Err(v);
        }
        unsafe { (*r.slots[tail & (r.slots.len() - 1)].get()).write(v) };
        r.tail.store(tail.wrapping_add(1), Ordering::Release);
        Ok(())
    }
}

impl<T> Consumer<T> {
    pub fn pop(&self) -> Option<T> {
        let r = &*self.0;
        let head = r.head.load(Ordering::Relaxed);
        let tail = r.tail.load(Ordering::Acquire);
        if head == tail {
            return None;
        }
        let v = unsafe { (*r.slots[head & (r.slots.len() - 1)].get()).assume_init_read() };
        r.head.store(head.wrapping_add(1), Ordering::Release);
        Some(v)
    }
}

impl<T> Drop for RingInner<T> {
    fn drop(&mut self) {
        let mut h = *self.head.get_mut();
        let t = *self.tail.get_mut();
        while h != t {
            unsafe { (*self.slots[h & (self.slots.len() - 1)].get()).assume_init_drop() };
            h = h.wrapping_add(1);
        }
    }
}

// ---------------------------------------------------------------- counting allocator

thread_local! {
    static THREAD_ALLOCS: Cell<u64> = const { Cell::new(0) };
}
static TOTAL_ALLOCS: AtomicU64 = AtomicU64::new(0);

/// Wraps the system allocator and counts allocations per thread. Installed in debug
/// builds; the allocation test asserts the frame loop leaves the count unchanged.
pub struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        count();
        unsafe { System.alloc(l) }
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        count();
        unsafe { System.alloc_zeroed(l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        count();
        unsafe { System.realloc(p, l, n) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
}

#[inline]
fn count() {
    TOTAL_ALLOCS.fetch_add(1, Ordering::Relaxed);
    let _ = THREAD_ALLOCS.try_with(|c| c.set(c.get() + 1));
}

/// Allocations made by the calling thread so far.
pub fn thread_allocs() -> u64 {
    THREAD_ALLOCS.with(|c| c.get())
}

pub fn total_allocs() -> u64 {
    TOTAL_ALLOCS.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_is_fifo_and_bounded() {
        let (p, c) = ring::<u32>(4);
        for i in 0..4 {
            p.push(i).unwrap();
        }
        assert!(p.push(9).is_err());
        assert_eq!(c.pop(), Some(0));
        p.push(4).unwrap();
        let got: Vec<u32> = std::iter::from_fn(|| c.pop()).collect();
        assert_eq!(got, vec![1, 2, 3, 4]);
    }

    #[test]
    fn ring_across_threads() {
        let (p, c) = ring::<u64>(64);
        let t = std::thread::spawn(move || {
            for i in 0..100_000u64 {
                let mut v = i;
                while let Err(back) = p.push(v) {
                    v = back;
                    std::thread::yield_now();
                }
            }
        });
        let mut want = 0u64;
        while want < 100_000 {
            if let Some(v) = c.pop() {
                assert_eq!(v, want);
                want += 1;
            }
        }
        t.join().unwrap();
    }

    #[test]
    fn bump_respects_budget() {
        let mut b = Bump::new(ArenaId::Frame);
        assert!(b.slice::<u64>(1000).is_some());
        assert!(b.slice::<u8>(BUDGETS[0]).is_none());
        b.reset();
        assert!(b.slice::<u8>(BUDGETS[0]).is_some());
    }
}
