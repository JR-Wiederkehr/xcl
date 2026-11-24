//! On unless `XCL_SYNC_TRACE=0`: who resets and submits which PC command list, and a report when a
//! submit breaks D3D12's rules – the two the debug layer names before it removes the device:
//! a list submitted after its allocator was reset (id 541), and a list submitted again while
//! an earlier submit of the same recording is still on the queue (id 553). Each report carries
//! the list's last events (reset / submit / its allocator's resets, with thread and time).
use std::collections::{HashMap, VecDeque};
use std::ffi::c_void;
use std::sync::Mutex;
use std::time::Instant;

use windows::core::Interface;
use windows::Win32::Graphics::Direct3D12::*;

const EVENTS: usize = 14;
const REPORTS: u32 = 60;

#[derive(Default)]
struct List {
    alloc: usize,
    /// the allocator's reset count when this recording began
    gen: u64,
    /// submits of this recording
    execs: u32,
    /// (queue, fence value after the last submit)
    last: Option<(usize, u64)>,
    events: VecDeque<String>,
}

#[derive(Default)]
struct State {
    lists: HashMap<usize, List>,
    alloc_gen: HashMap<usize, u64>,
    fences: HashMap<usize, (ID3D12Fence, u64)>,
    reports: u32,
    start: Option<Instant>,
    /// queue → what XCL put on it since its last traced submit (waits for other queues)
    ops: HashMap<usize, Vec<String>>,
    /// queue → its traced submits the GPU is not past: (number, when, the ops before it)
    pending: HashMap<usize, VecDeque<(u64, Instant, String)>>,
}

unsafe impl Send for State {}

static STATE: Mutex<Option<State>> = Mutex::new(None);

pub fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| !std::env::var("XCL_SYNC_TRACE").is_ok_and(|v| v.trim() == "0"))
}

fn stamp(s: &mut State) -> String {
    let t = s.start.get_or_insert_with(Instant::now).elapsed().as_secs_f64();
    let tid = unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() };
    format!("+{t:.3}s t{tid}")
}

fn note(l: &mut List, e: String) {
    if l.events.len() == EVENTS {
        l.events.pop_front();
    }
    l.events.push_back(e);
}

fn report(s: &mut State, list: usize, what: String) {
    if s.reports >= REPORTS {
        return;
    }
    s.reports += 1;
    let history = s.lists.get(&list).map(|l| l.events.iter().cloned().collect::<Vec<_>>().join(" | ")).unwrap_or_default();
    crate::xlog_warn!("sync trace: {what}; list {list:#x} history: {history}");
}

/// `list` begins a recording on `alloc` (`why`: which path reset or made it).
pub fn list_reset(list: usize, alloc: usize, why: &str) {
    if !enabled() {
        return;
    }
    let mut g = STATE.lock().unwrap();
    let s = g.get_or_insert_with(State::default);
    let at = stamp(s);
    let gen = s.alloc_gen.get(&alloc).copied().unwrap_or(0);
    let l = s.lists.entry(list).or_default();
    let (old_execs, old_last) = (l.execs, l.last);
    l.alloc = alloc;
    l.gen = gen;
    l.execs = 0;
    note(l, format!("{at} reset[{why}] alloc {alloc:#x} gen {gen} (was: {old_execs} submits, last {old_last:x?})"));
}

/// `alloc` was reset by the title.
pub fn alloc_reset(alloc: usize) {
    if !enabled() {
        return;
    }
    let mut g = STATE.lock().unwrap();
    let s = g.get_or_insert_with(State::default);
    let at = stamp(s);
    let gen = s.alloc_gen.entry(alloc).or_insert(0);
    let old = *gen;
    *gen += 1;
    let mut flying = Vec::new();
    let mut unsubmitted = 0usize;
    let mine: Vec<usize> = s.lists.iter().filter(|(_, l)| l.alloc == alloc && l.gen == old).map(|(&k, _)| k).collect();
    for k in mine {
        let (execs, last) = {
            let l = s.lists.get_mut(&k).unwrap();
            note(l, format!("{at} ALLOC-RESET {alloc:#x} gen {old}->{}", old + 1));
            (l.execs, l.last)
        };
        if execs == 0 {
            unsubmitted += 1;
        } else if let Some((q, v)) = last {
            let done = s.fences.get(&q).map(|(f, _)| unsafe { f.GetCompletedValue() }).unwrap_or(u64::MAX);
            if done < v {
                flying.push((k, q, v, done));
            }
        }
    }
    for (k, q, v, done) in flying {
        report(s, k, format!("allocator {alloc:#x} reset while its list is still on queue {q:#x} (submit {v}, queue at {done})"));
    }
    let _ = unsubmitted;
}

/// `lists` were just submitted to `queue` (`why`: which path).
pub unsafe fn executed(queue: *mut c_void, lists: &[*mut c_void], why: &str) {
    if !enabled() || queue.is_null() {
        return;
    }
    let Some(q) = ID3D12CommandQueue::from_raw_borrowed(&queue) else { return };
    let mut g = STATE.lock().unwrap();
    let s = g.get_or_insert_with(State::default);
    let at = stamp(s);
    let key = queue as usize;
    if !s.fences.contains_key(&key) {
        let mut dev: Option<ID3D12Device> = None;
        let Some(f) = q.GetDevice(&mut dev).ok().and(dev).and_then(|d| d.CreateFence::<ID3D12Fence>(0, D3D12_FENCE_FLAG_NONE).ok()) else { return };
        s.fences.insert(key, (f, 0));
    }
    let (done, next) = {
        let (f, v) = s.fences.get_mut(&key).unwrap();
        *v += 1;
        let _ = q.Signal(&*f, *v);
        (f.GetCompletedValue(), *v)
    };
    {
        let before = s.ops.remove(&key).unwrap_or_default().join("; ");
        let p = s.pending.entry(key).or_default();
        while p.front().is_some_and(|x| x.0 <= done) {
            p.pop_front();
        }
        p.push_back((next, Instant::now(), format!("[{why}, {} lists] after: {}", lists.len(), if before.is_empty() { "nothing" } else { &before })));
    }
    for &list in lists {
        let list = list as usize;
        let known = s.lists.contains_key(&list);
        let l = s.lists.entry(list).or_default();
        note(l, format!("{at} submit[{why}] queue {key:#x} #{next} (batch of {})", lists.len()));
        let (alloc, gen, execs, last) = (l.alloc, l.gen, l.execs, l.last);
        l.execs += 1;
        l.last = Some((key, next));
        if !known {
            continue;
        }
        let now_gen = s.alloc_gen.get(&alloc).copied().unwrap_or(0);
        if now_gen != gen {
            report(s, list, format!("STALE SUBMIT [{why}]: recorded on allocator {alloc:#x} gen {gen}, the allocator is at gen {now_gen} (reset since)"));
        } else if execs > 0 {
            if let Some((pq, pv)) = last {
                let pdone = if pq == key { done } else { s.fences.get(&pq).map(|(f, _)| f.GetCompletedValue()).unwrap_or(u64::MAX) };
                if pdone < pv {
                    report(s, list, format!("SUBMITTED AGAIN [{why}] without a reset: submit {pv} on queue {pq:#x} is not done (queue at {pdone})"));
                }
            }
        }
    }
}

/// (queue, submits traced, submits the GPU is past) for every queue seen.
pub fn queue_progress() -> Vec<(usize, u64, u64)> {
    let g = STATE.lock().unwrap();
    let Some(s) = g.as_ref() else { return Vec::new() };
    s.fences.iter().map(|(&q, (f, v))| (q, *v, unsafe { f.GetCompletedValue() })).collect()
}

/// XCL put `what` on `queue` (a wait for another queue, mostly): the next traced submit on it
/// stands behind that.
pub fn queue_op(queue: usize, what: String) {
    if !enabled() {
        return;
    }
    let mut g = STATE.lock().unwrap();
    let s = g.get_or_insert_with(State::default);
    let v = s.ops.entry(queue).or_default();
    if v.len() < 12 {
        v.push(what);
    }
}

/// For each queue behind: (queue, its oldest unfinished traced submit, for how many ms, what
/// XCL put on the queue before it).
pub fn oldest_pending() -> Vec<(usize, u64, u128, String)> {
    let mut g = STATE.lock().unwrap();
    let Some(s) = g.as_mut() else { return Vec::new() };
    let mut out = Vec::new();
    for (&q, p) in s.pending.iter_mut() {
        let done = s.fences.get(&q).map(|(f, _)| unsafe { f.GetCompletedValue() }).unwrap_or(u64::MAX);
        while p.front().is_some_and(|x| x.0 <= done) {
            p.pop_front();
        }
        if let Some((n, at, what)) = p.front() {
            out.push((q, *n, at.elapsed().as_millis(), what.clone()));
        }
    }
    out
}
