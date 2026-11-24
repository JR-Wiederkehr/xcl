//! Non-blocking pipeline creation. Translating a console shader takes milliseconds; compiling
//! the HLSL with DXC takes 0.2 s to seconds, and a title creates thousands of pipelines while
//! it loads. When a pipeline's DXIL is not in the disk cache yet, the title gets a stand-in
//! pipeline (shaders that draw nothing) at once and the real one is built on a worker; binding
//! (`SetPipelineState`, console list replay, the two-pass lookups) uses the real one as soon
//! as it is there. A graphics pipeline draws nothing until then; a compute pipeline is waited
//! for at its first bind (a skipped dispatch would corrupt what it computes).
//! `XCL_ASYNC_PSO=0` builds every pipeline in the call, as before.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, OnceLock};

struct Pending {
    /// the build, until a worker – or a bind that cannot wait for the queue – takes it
    job: Mutex<Option<Job>>,
    /// the build went to the workers (a lazy one goes on its first `resolve`)
    sent: std::sync::atomic::AtomicBool,
    /// the real pipeline state (raw pointer), 0 while building or when building failed
    real: AtomicUsize,
    done: Mutex<bool>,
    cv: Condvar,
    compute: bool,
    /// of its shaders (`used`); 0 = none
    key: u64,
    /// it was used (`used::note` is told once)
    used: std::sync::atomic::AtomicBool,
    /// a draw waited for it once (`resolve_for_draw`)
    draw_waited: std::sync::atomic::AtomicBool,
    /// `resolve_wait` waited for it once
    waited: std::sync::atomic::AtomicBool,
    /// the stand-in was released before the real pipeline was built
    orphan: std::sync::atomic::AtomicBool,
}

impl Pending {
    fn note_use(&self) {
        if self.key != 0 && !self.used.swap(true, Ordering::Relaxed) {
            used::note(self.key);
        }
    }
}

/// stand-in handed to the title → its real pipeline
static PENDING: Mutex<Option<HashMap<usize, Arc<Pending>>>> = Mutex::new(None);

/// The build is done (`real`: its pipeline, 0 = failed).
fn complete(p: &Pending, real: usize) {
    if real == 0 {
        FAILED.fetch_add(1, Ordering::Relaxed);
    } else {
        BUILT.fetch_add(1, Ordering::Relaxed);
    }
    let orphan = {
        let mut done = p.done.lock().unwrap();
        let orphan = p.orphan.load(Ordering::Acquire);
        if !orphan {
            p.real.store(real, Ordering::Release);
        }
        *done = true;
        p.cv.notify_all();
        orphan
    };
    // outside `done`: the Release override takes PENDING (`counts` takes PENDING, then `done`)
    if orphan {
        release(real);
    }
}
static BUILT: AtomicUsize = AtomicUsize::new(0);
static FAILED: AtomicUsize = AtomicUsize::new(0);

pub fn enabled() -> bool {
    std::env::var("XCL_ASYNC_PSO").map_or(true, |v| v != "0")
}

/// Builds the real pipeline: its raw pointer, 0 = failed.
pub type Job = Box<dyn FnOnce() -> usize + Send>;

fn queue() -> &'static Mutex<mpsc::Sender<Arc<Pending>>> {
    static Q: OnceLock<Mutex<mpsc::Sender<Arc<Pending>>>> = OnceLock::new();
    Q.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Arc<Pending>>();
        let rx = Arc::new(Mutex::new(rx));
        // translation recurses deeply on big programs; DXC itself runs on its own pool
        let n = crate::gpu::recomp::dxc::compile_threads();
        for k in 0..n {
            let rx = rx.clone();
            let _ = std::thread::Builder::new().name(format!("xcl-pso-{k}")).stack_size(64 << 20).spawn(move || loop {
                let next = rx.lock().unwrap().recv();
                let Ok(p) = next else { return };
                // (None: a bind took it and builds it itself)
                let job = p.job.lock().unwrap().take();
                if let Some(job) = job {
                    let t0 = std::time::Instant::now();
                    let real = job();
                    // (a build of many seconds: the driver's compiler on a large program)
                    if t0.elapsed().as_secs() >= 5 {
                        crate::xlog_info!("D3D12.X: a {} pipeline took {} s to build in the background ({})", if p.compute { "compute" } else { "graphics" }, t0.elapsed().as_secs(), if real == 0 { "failed" } else { "built" });
                    }
                    complete(&p, real);
                }
            });
        }
        Mutex::new(tx)
    })
}

/// Release a pipeline state XCL owns (raw pointer, 0 = none).
pub fn release(p: usize) {
    if p != 0 {
        unsafe { drop(<windows::Win32::Graphics::Direct3D12::ID3D12PipelineState as windows::core::Interface>::from_raw(p as *mut std::ffi::c_void)) };
    }
}

/// `standin` went to the title; `job` builds the real pipeline on a worker.
pub fn submit(standin: usize, compute: bool, job: Job) {
    submit_as(standin, compute, job, true, 0);
}

/// Lazy pipeline creation (`XCL_LAZY_PSO=0`: off): a title's pipelines with console shaders get
/// their stand-in when made and are built when first used – the first bind of a graphics one
/// sends it to the workers, a compute one is built at its first bind. A retail game makes
/// ~51 000 pipelines in its first 30 s and uses few; each one built stays in the driver
/// (~400 KB), and the process ran out of commit.
pub fn lazy() -> bool {
    static L: OnceLock<bool> = OnceLock::new();
    *L.get_or_init(|| enabled() && std::env::var("XCL_LAZY_PSO").map_or(true, |v| v != "0"))
}

/// `submit`, sent to the workers only on its first `resolve` (`lazy`). `key`: of its shaders
/// (`used`); 0 = none.
pub fn submit_lazy(standin: usize, compute: bool, job: Job, key: u64) {
    submit_as(standin, compute, job, false, key);
}

fn submit_as(standin: usize, compute: bool, job: Job, send: bool, key: u64) {
    let p = Arc::new(Pending {
        job: Mutex::new(Some(job)),
        sent: std::sync::atomic::AtomicBool::new(send),
        real: AtomicUsize::new(0),
        done: Mutex::new(false),
        cv: Condvar::new(),
        compute,
        key,
        used: Default::default(),
        draw_waited: Default::default(),
        waited: Default::default(),
        orphan: Default::default(),
    });
    PENDING.lock().unwrap().get_or_insert_with(HashMap::new).insert(standin, p.clone());
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        crate::xlog_info!("D3D12.X: pipelines whose shaders are not compiled yet get a stand-in and are built in the background");
    });
    if send {
        let _ = queue().lock().unwrap().send(p);
    } else if let Some(q) = preload() {
        // what an earlier run used first; then the other compute pipelines (a skipped dispatch
        // leaves out what the title reads; a graphics pipeline nobody used is not worth its memory)
        let known = used::known(key);
        if known || compute {
            let mut g = q.lists.lock().unwrap();
            if known {
                g.0.push_back(p);
            } else {
                g.1.push_back(p);
            }
            q.cv.notify_one();
        }
    }
}

/// The pipelines a title used, by the hash of their shaders, kept between runs
/// (`used-pipelines.txt` beside the DXIL cache): the next run builds them while the title
/// loads, before their first use.
mod used {
    use std::collections::HashSet;
    use std::io::Write;
    use std::sync::{Mutex, OnceLock};

    struct Used {
        earlier: HashSet<u64>,
        now: Mutex<(HashSet<u64>, Option<std::fs::File>)>,
    }

    fn get() -> &'static Used {
        static U: OnceLock<Used> = OnceLock::new();
        U.get_or_init(|| {
            let path = crate::gpu::recomp::dxc::cache_dir().join("used-pipelines.txt");
            let earlier: HashSet<u64> = std::fs::read_to_string(&path).unwrap_or_default().lines().filter_map(|l| u64::from_str_radix(l.trim(), 16).ok()).collect();
            if !earlier.is_empty() {
                crate::xlog_info!("D3D12.X: {} pipelines of earlier runs are built ahead of their first use", earlier.len());
            }
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let file = std::fs::OpenOptions::new().create(true).append(true).open(&path).ok();
            Used { now: Mutex::new((earlier.clone(), file)), earlier }
        })
    }

    /// An earlier run used a pipeline of these shaders.
    pub fn known(key: u64) -> bool {
        key != 0 && get().earlier.contains(&key)
    }

    /// A pipeline of these shaders is used now.
    pub fn note(key: u64) {
        if key == 0 {
            return;
        }
        let mut g = get().now.lock().unwrap();
        if g.0.insert(key) {
            if let Some(f) = g.1.as_mut() {
                let _ = writeln!(f, "{key:016x}");
            }
        }
    }
}

/// Pipelines built ahead of their first use, while the title loads (`XCL_PSO_PRELOAD=0`: off):
/// first the ones an earlier run used (`used`), then the other compute pipelines. A draw or
/// dispatch whose pipeline is not built is skipped, and a retail title's first frames of its
/// world used hundreds of them at once: what they compute was missing and the title read it.
/// These workers are apart from the ones a bind sends to (`queue`) and at the lowest priority:
/// a pipeline that is bound is not held up by the ones made ahead (whoever takes `job` first
/// builds it), and the title's audio and video are not either.
struct Preload {
    lists: Mutex<(std::collections::VecDeque<Arc<Pending>>, std::collections::VecDeque<Arc<Pending>>)>,
    cv: Condvar,
}

fn preload() -> Option<&'static Preload> {
    static Q: OnceLock<Option<Preload>> = OnceLock::new();
    static STARTED: std::sync::Once = std::sync::Once::new();
    let q = Q
        .get_or_init(|| {
            if std::env::var("XCL_PSO_PRELOAD").is_ok_and(|v| v.trim() == "0") {
                return None;
            }
            Some(Preload { lists: Mutex::new(Default::default()), cv: Condvar::new() })
        })
        .as_ref()?;
    STARTED.call_once(|| {
        let n = (crate::gpu::recomp::dxc::compile_threads() / 4).clamp(2, 4);
        for k in 0..n {
            let _ = std::thread::Builder::new().name(format!("xcl-pso-pre-{k}")).stack_size(64 << 20).spawn(move || {
                unsafe {
                    use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_LOWEST};
                    SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_LOWEST);
                }
                static DONE: AtomicUsize = AtomicUsize::new(0);
                loop {
                    let p = {
                        let mut g = q.lists.lock().unwrap();
                        loop {
                            if let Some(p) = g.0.pop_front().or_else(|| g.1.pop_front()) {
                                break p;
                            }
                            g = q.cv.wait(g).unwrap();
                        }
                    };
                    // (released by the title meanwhile, or taken by a bind)
                    if p.orphan.load(Ordering::Acquire) {
                        continue;
                    }
                    let job = p.job.lock().unwrap().take();
                    if let Some(job) = job {
                        let real = job();
                        complete(&p, real);
                        let n = DONE.fetch_add(1, Ordering::Relaxed) + 1;
                        if n.is_power_of_two() {
                            crate::xlog_info!("D3D12.X: {n} pipelines built ahead of their first use");
                        }
                    }
                }
            });
        }
    });
    Some(q)
}

fn entry(pso: usize) -> Option<Arc<Pending>> {
    PENDING.lock().unwrap().as_ref()?.get(&pso).cloned()
}

/// The pipeline to use for `pso` now: the real one once built, else `pso` itself.
pub fn resolve(pso: usize) -> usize {
    let Some(p) = entry(pso) else { return pso };
    p.note_use();
    match p.real.load(Ordering::Acquire) {
        0 => {
            // a lazy one is wanted now: to the workers
            if !p.sent.swap(true, Ordering::AcqRel) {
                let _ = queue().lock().unwrap().send(p);
            }
            pso
        }
        r => r,
    }
}

/// `resolve`, waiting for a pipeline still being built – built here if no worker has started
/// it yet (a queue of a title's hundreds of pipelines must not hold up the one it binds now).
///
/// The wait has a limit: `XCL_PSO_WAIT_MS` (default 4000) the first time a pipeline is waited
/// for, none after that – the caller gets the stand-in and skips its dispatch until the build
/// is done. Without a limit (`XCL_PSO_WAIT_MS=0`) a retail title's thread sat here for the 45 s
/// and more the NVIDIA driver took on one compute pipeline, nothing was presented, the title's
/// watchdog ended it – and the driver never got to cache the pipeline, so every run stopped
/// there. With a limit the pipeline is not built on the title's thread either.
pub fn resolve_wait(pso: usize) -> usize {
    let Some(p) = entry(pso) else { return pso };
    p.note_use();
    static LIMIT: OnceLock<u64> = OnceLock::new();
    let limit = *LIMIT.get_or_init(|| std::env::var("XCL_PSO_WAIT_MS").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(100));
    if limit == 0 {
        let job = p.job.lock().unwrap().take();
        if let Some(job) = job {
            complete(&p, job());
        }
        let mut done = p.done.lock().unwrap();
        while !*done {
            done = p.cv.wait(done).unwrap();
        }
    } else if p.real.load(Ordering::Acquire) == 0 {
        if !p.sent.swap(true, Ordering::AcqRel) {
            let _ = queue().lock().unwrap().send(p.clone());
        }
        // (a pipeline is waited for once – when there was something left to wait with)
        let ms = match wait_allowance(limit) {
            0 => 0,
            _ if p.waited.swap(true, Ordering::AcqRel) => 0,
            ms => ms,
        };
        let t0 = std::time::Instant::now();
        let done = p.done.lock().unwrap();
        let (done, _) = p.cv.wait_timeout_while(done, std::time::Duration::from_millis(ms), |d| !*d).unwrap();
        if ms > 0 {
            waited(t0.elapsed());
        }
        if !*done {
            static N: AtomicUsize = AtomicUsize::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed) + 1;
            if n.is_power_of_two() {
                crate::xlog_warn!("D3D12.X: {n} uses so far of a {} pipeline that is still being built after the wait – skipped", if p.compute { "compute" } else { "graphics" });
            }
        }
    }
    match p.real.load(Ordering::Acquire) {
        0 => pso,
        r => r,
    }
}

/// The waits above are for a pipeline that is nearly done, and a title's frame may use hundreds
/// for the first time (a retail title's first frames of its world): a wait of the full limit
/// for each is tens of seconds on its render thread with nothing presented, and its watchdog
/// ends it – while no wait at all skips draws and dispatches whose pipelines would have been
/// there in a few milliseconds (their blobs are on disk), and what those dispatches compute is
/// missing when the title reads it. So the waits share an allowance of time: `WAIT_POOL_MS` at
/// most, refilled by `WAIT_REFILL_MS` a second; each wait takes what it used.
const WAIT_POOL_MS: i64 = 1500;
const WAIT_REFILL_MS: i64 = 500;
/// (milliseconds left, time of the last refill in ms)
static WAIT_POOL: Mutex<(i64, u64)> = Mutex::new((WAIT_POOL_MS, 0));

/// How long a wait may take now: up to `limit` ms, 0 when the allowance is used up.
fn wait_allowance(limit: u64) -> u64 {
    let now = (crate::log::uptime() * 1000.0) as u64;
    let mut g = WAIT_POOL.lock().unwrap();
    let add = (now.saturating_sub(g.1) as i64).saturating_mul(WAIT_REFILL_MS) / 1000;
    if add > 0 {
        g.0 = (g.0 + add).min(WAIT_POOL_MS);
        g.1 = now;
    }
    g.0.clamp(0, limit as i64) as u64
}

fn waited(took: std::time::Duration) {
    WAIT_POOL.lock().unwrap().0 -= took.as_millis() as i64;
}

/// For a draw: the real pipeline if it is built, else the stand-in (which draws nothing) – after
/// one wait of at most `XCL_PSO_DRAW_WAIT_MS` (default 250) the first time the pipeline is
/// drawn with. It is never built on the title's thread and never waited for without a limit:
/// on an NVIDIA GPU the driver took over 45 s on one pipeline of a retail title while its
/// render thread sat in `resolve_wait`; nothing was presented and the title's watchdog ended
/// it. The draws of a pipeline that is not ready are lost until it is.
pub fn resolve_for_draw(pso: usize) -> usize {
    let Some(p) = entry(pso) else { return pso };
    p.note_use();
    let real = p.real.load(Ordering::Acquire);
    if real != 0 {
        return real;
    }
    if !p.sent.swap(true, Ordering::AcqRel) {
        let _ = queue().lock().unwrap().send(p.clone());
    }
    static WAIT: OnceLock<u64> = OnceLock::new();
    let ms = *WAIT.get_or_init(|| std::env::var("XCL_PSO_DRAW_WAIT_MS").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(250));
    let ms = match wait_allowance(ms) {
        0 => 0,
        _ if p.draw_waited.swap(true, Ordering::AcqRel) => 0,
        ms => ms,
    };
    if ms > 0 {
        let t0 = std::time::Instant::now();
        let done = p.done.lock().unwrap();
        let _ = p.cv.wait_timeout_while(done, std::time::Duration::from_millis(ms), |d| !*d).unwrap();
        waited(t0.elapsed());
    }
    match p.real.load(Ordering::Acquire) {
        0 => {
            static N: AtomicUsize = AtomicUsize::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed) + 1;
            if n.is_power_of_two() {
                crate::xlog_info!("D3D12.X: {n} draws so far with a pipeline that is still being built – they draw nothing");
            }
            pso
        }
        r => r,
    }
}

/// For binding: compute pipelines are waited for, graphics ones are not.
pub fn resolve_for_bind(pso: usize) -> usize {
    match entry(pso) {
        Some(p) if p.compute => resolve_wait(pso),
        Some(_) => resolve(pso),
        None => pso,
    }
}

/// The title released stand-in `pso` for good: forget it (its address may come back for
/// another object) and return its real pipeline to release (0: none, or still being built –
/// the worker's result is then released when it arrives).
pub fn released(pso: usize) -> usize {
    let Some(p) = PENDING.lock().unwrap().as_mut().and_then(|m| m.remove(&pso)) else { return 0 };
    let done = p.done.lock().unwrap();
    if *done {
        p.real.swap(0, Ordering::AcqRel)
    } else {
        // the worker stores its result into a Pending nobody will read: mark it orphaned
        p.orphan.store(true, Ordering::Release);
        0
    }
}

/// `pso` is a stand-in whose real pipeline is still being built.
pub fn is_pending(pso: usize) -> bool {
    entry(pso).is_some_and(|p| !*p.done.lock().unwrap())
}

/// (built in the background, failed, still building)
pub fn counts() -> (usize, usize, usize) {
    let pending = PENDING.lock().unwrap().as_ref().map_or(0, |m| m.values().filter(|p| !*p.done.lock().unwrap()).count());
    (BUILT.load(Ordering::Relaxed), FAILED.load(Ordering::Relaxed), pending)
}
