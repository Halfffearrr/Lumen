//! A concurrent mark-sweep garbage collector (buff4).
//!
//! # What this is, and how it relates to the interpreter
//!
//! Lumen's live interpreter manages heap values (`Str`/`List`/`Dict`/`Closure`)
//! with reference counting (`Rc`), which is simple, deterministic, and the
//! fallback the project plan explicitly sanctions. Reference counting has one
//! famous blind spot: it cannot reclaim **reference cycles**. This module is a
//! self-contained **tracing** collector that does reclaim cycles, and runs its
//! collection on a **background thread** — which is where the project's
//! concurrency requirement is demonstrated.
//!
//! # The algorithm
//!
//! The heap is an arena of objects, each carrying outgoing references to other
//! objects (so cycles are expressible) and a `marked` bit. Some objects are
//! *roots* (reachable directly, as the VM stack and globals would be).
//! Collection is two phases:
//!
//! 1. **Mark.** Starting from the roots, do a graph traversal, setting `marked`
//!    on every reachable object. A work-list makes this iterative (no recursion,
//!    so deep/cyclic graphs are fine — a visited object is skipped).
//! 2. **Sweep.** Walk every slot; whatever is still unmarked is unreachable, so
//!    free it and recycle its slot.
//!
//! # Concurrency
//!
//! The heap lives behind `Arc<Mutex<Heap>>`. A [`Collector`] owns a background
//! thread that waits on a channel; the mutator (main thread) keeps allocating
//! and, when it wants, sends `Collect`. The collector locks the heap, runs a
//! mark-sweep, and records stats. This mirrors a real runtime where GC happens
//! off the mutator's hot path; the `Mutex` is the synchronization point.

use std::collections::HashSet;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

/// One object in the GC heap. The `value` is a stand-in payload (a real VM would
/// store a string, list, etc.); `refs` are the objects it points at, which is
/// what makes the object graph — and cycles within it — possible.
#[derive(Debug)]
struct GcObject {
    value: i64,
    refs: Vec<usize>,
    marked: bool,
}

/// Snapshot of heap occupancy, returned by collection for logging/tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GcStats {
    /// Objects still live after the last collection.
    pub live: usize,
    /// Objects reclaimed by the last collection.
    pub freed: usize,
}

/// A simple arena heap with mark-sweep collection.
#[derive(Debug, Default)]
pub struct Heap {
    objects: Vec<Option<GcObject>>,
    roots: HashSet<usize>,
    /// Slots emptied by sweeping, available for reuse.
    free: Vec<usize>,
}

impl Heap {
    pub fn new() -> Self {
        Heap::default()
    }

    /// Allocate an object holding `value`, reusing a swept slot when possible, and
    /// return its handle (slot index).
    pub fn alloc(&mut self, value: i64) -> usize {
        let obj = GcObject {
            value,
            refs: Vec::new(),
            marked: false,
        };
        if let Some(i) = self.free.pop() {
            self.objects[i] = Some(obj);
            i
        } else {
            self.objects.push(Some(obj));
            self.objects.len() - 1
        }
    }

    /// Record that object `from` references object `to` (an edge in the graph).
    pub fn add_ref(&mut self, from: usize, to: usize) {
        if let Some(Some(obj)) = self.objects.get_mut(from) {
            obj.refs.push(to);
        }
    }

    /// Mark `id` as a root (a directly-reachable object).
    pub fn add_root(&mut self, id: usize) {
        self.roots.insert(id);
    }

    /// Drop `id` from the root set; it now lives only as long as something
    /// reachable still references it.
    pub fn remove_root(&mut self, id: usize) {
        self.roots.remove(&id);
    }

    /// The number of objects currently allocated (live in the arena).
    pub fn live(&self) -> usize {
        self.objects.iter().flatten().count()
    }

    /// The payload of an object, if it is still allocated.
    pub fn value(&self, id: usize) -> Option<i64> {
        self.objects
            .get(id)
            .and_then(|o| o.as_ref())
            .map(|o| o.value)
    }

    /// Phase 1: mark every object reachable from a root.
    fn mark(&mut self) {
        for obj in self.objects.iter_mut().flatten() {
            obj.marked = false;
        }
        let mut work: Vec<usize> = self.roots.iter().copied().collect();
        while let Some(id) = work.pop() {
            // Mark this object and queue its referents, skipping anything already
            // visited (which also makes cycles terminate).
            let refs = match self.objects.get_mut(id) {
                Some(Some(obj)) if !obj.marked => {
                    obj.marked = true;
                    obj.refs.clone()
                }
                _ => continue,
            };
            work.extend(refs);
        }
    }

    /// Phase 2: free every unmarked object and recycle its slot. Returns how many
    /// were reclaimed.
    fn sweep(&mut self) -> usize {
        let dead: Vec<usize> = self
            .objects
            .iter()
            .enumerate()
            .filter_map(|(i, slot)| match slot {
                Some(obj) if !obj.marked => Some(i),
                _ => None,
            })
            .collect();
        for &i in &dead {
            self.objects[i] = None;
            self.free.push(i);
        }
        dead.len()
    }

    /// Run a full mark-sweep collection and report the resulting occupancy.
    pub fn collect(&mut self) -> GcStats {
        self.mark();
        let freed = self.sweep();
        GcStats {
            live: self.live(),
            freed,
        }
    }
}

/// Messages the mutator sends to the background collector.
enum Command {
    /// Run one mark-sweep collection; reply with the resulting stats.
    Collect(Sender<GcStats>),
    /// Shut the collector thread down.
    Stop,
}

/// A garbage collector whose marking and sweeping run on a dedicated background
/// thread, sharing the heap with the mutator through `Arc<Mutex<Heap>>`.
pub struct Collector {
    heap: Arc<Mutex<Heap>>,
    tx: Sender<Command>,
    handle: Option<JoinHandle<()>>,
}

impl Collector {
    /// Start a collector with a fresh heap and its background thread.
    pub fn new() -> Self {
        Self::with_heap(Arc::new(Mutex::new(Heap::new())))
    }

    /// Start a collector over an existing shared heap (useful for tests that want
    /// to inspect the heap directly).
    pub fn with_heap(heap: Arc<Mutex<Heap>>) -> Self {
        let (tx, rx): (Sender<Command>, Receiver<Command>) = mpsc::channel();
        let worker_heap = Arc::clone(&heap);
        let handle = thread::spawn(move || collector_loop(worker_heap, rx));
        Collector {
            heap,
            tx,
            handle: Some(handle),
        }
    }

    /// A handle to the shared heap for allocation and root management.
    pub fn heap(&self) -> Arc<Mutex<Heap>> {
        Arc::clone(&self.heap)
    }

    /// Ask the background thread to collect now and wait for it to finish,
    /// returning the post-collection stats.
    pub fn collect_now(&self) -> GcStats {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.tx
            .send(Command::Collect(reply_tx))
            .expect("collector thread alive");
        reply_rx.recv().expect("collector reply")
    }
}

impl Default for Collector {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Collector {
    /// Stop and join the background thread so it never outlives the collector.
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Stop);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// The background thread body: service collection requests until told to stop.
fn collector_loop(heap: Arc<Mutex<Heap>>, rx: Receiver<Command>) {
    while let Ok(cmd) = rx.recv() {
        match cmd {
            Command::Collect(reply) => {
                let stats = heap.lock().expect("heap mutex not poisoned").collect();
                let _ = reply.send(stats);
            }
            Command::Stop => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unreachable_objects_are_swept() {
        let mut h = Heap::new();
        let keep = h.alloc(1);
        let _drop_me = h.alloc(2);
        h.add_root(keep);
        let stats = h.collect();
        assert_eq!(stats.freed, 1);
        assert_eq!(stats.live, 1);
        assert_eq!(h.value(keep), Some(1));
    }

    #[test]
    fn reachable_chains_survive() {
        let mut h = Heap::new();
        let a = h.alloc(1);
        let b = h.alloc(2);
        let c = h.alloc(3);
        h.add_root(a);
        h.add_ref(a, b);
        h.add_ref(b, c);
        let stats = h.collect();
        assert_eq!(stats.freed, 0);
        assert_eq!(stats.live, 3);
    }

    #[test]
    fn cycles_with_no_root_are_collected() {
        // a <-> b form a cycle that nothing roots: reference counting would leak
        // this, but mark-sweep reclaims it.
        let mut h = Heap::new();
        let a = h.alloc(1);
        let b = h.alloc(2);
        h.add_ref(a, b);
        h.add_ref(b, a);
        let stats = h.collect();
        assert_eq!(stats.freed, 2);
        assert_eq!(stats.live, 0);
    }

    #[test]
    fn dropping_a_root_frees_its_now_unreachable_graph() {
        let mut h = Heap::new();
        let root = h.alloc(1);
        let child = h.alloc(2);
        h.add_root(root);
        h.add_ref(root, child);
        assert_eq!(h.collect().freed, 0);

        h.remove_root(root);
        let stats = h.collect();
        assert_eq!(stats.freed, 2); // both root and child are now garbage
        assert_eq!(stats.live, 0);
    }

    #[test]
    fn swept_slots_are_reused() {
        let mut h = Heap::new();
        let a = h.alloc(1); // slot 0, will become garbage
        h.collect();
        let b = h.alloc(2); // should reuse slot 0
        assert_eq!(a, b);
    }

    #[test]
    fn background_collector_reclaims_over_a_shared_heap() {
        let gc = Collector::new();
        let heap = gc.heap();

        // The mutator allocates a rooted object and a piece of garbage.
        let keep = {
            let mut h = heap.lock().unwrap();
            let keep = h.alloc(100);
            let _garbage = h.alloc(200);
            h.add_root(keep);
            keep
        };

        // The background thread performs the collection.
        let stats = gc.collect_now();
        assert_eq!(stats.freed, 1);
        assert_eq!(stats.live, 1);
        assert_eq!(heap.lock().unwrap().value(keep), Some(100));
    }
}
