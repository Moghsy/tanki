//! Opt-in legacy LaTeX generation. Only the collection actor calls this module.
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct Job {
    filename: String,
    body: String,
    pre: String,
    post: String,
    svg: bool,
}
enum State {
    Pending,
    Ready(String),
    Failed(String),
}
type JobResult = (Job, Result<Vec<u8>, String>);
struct Jobs {
    tx: Option<mpsc::SyncSender<Job>>,
    rx: Option<mpsc::Receiver<JobResult>>,
    states: HashMap<Job, State>,
    cancel: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Jobs {
    fn start() -> Self {
        Self::start_with(|job, cancel| {
            generate_with(
                &job.body,
                &job.pre,
                &job.post,
                job.svg,
                Path::new("latex"),
                Path::new(if job.svg { "dvisvgm" } else { "dvipng" }),
                cancel,
            )
        })
    }
    fn start_with<F>(generate: F) -> Self
    where
        F: Fn(&Job, &AtomicBool) -> Result<Vec<u8>, String> + Send + 'static,
    {
        let (tx, jobs) = mpsc::sync_channel::<Job>(8);
        let (done, rx) = mpsc::sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let worker = std::thread::Builder::new()
            .name("tanki-latex".into())
            .spawn(move || {
                while let Ok(job) = jobs.recv() {
                    if worker_cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    let result = generate(&job, &worker_cancel);
                    if done.send((job, result)).is_err() {
                        break;
                    }
                }
            })
            .ok();
        Self {
            tx: Some(tx),
            rx: Some(rx),
            states: HashMap::new(),
            cancel,
            worker,
        }
    }
    fn request(&mut self, job: Job) -> Result<Option<String>, String> {
        match self.states.get(&job) {
            Some(State::Pending) => return Ok(None),
            Some(State::Ready(name)) => return Ok(Some(name.clone())),
            Some(State::Failed(error)) => return Err(error.clone()),
            None => {}
        }
        if self.states.len() >= 128 {
            return Err("LaTeX job limit reached; reopen the profile to retry".into());
        }
        if self
            .tx
            .as_ref()
            .is_none_or(|tx| tx.try_send(job.clone()).is_err())
        {
            let error = "LaTeX queue unavailable or full; reopen the profile to retry".to_owned();
            self.states.insert(job, State::Failed(error.clone()));
            return Err(error);
        }
        self.states.insert(job, State::Pending);
        Ok(None)
    }
    fn collect<F>(&mut self, mut add_media: F) -> bool
    where
        F: FnMut(&str, Vec<u8>) -> Result<String, String>,
    {
        let mut changed = false;
        while let Some((job, result)) = self.rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            let state = match result.and_then(|bytes| add_media(&job.filename, bytes)) {
                Ok(name) => State::Ready(name),
                Err(error) => State::Failed(error),
            };
            self.states.insert(job, state);
            changed = true;
        }
        changed
    }
    fn pending(&self) -> bool {
        self.states.values().any(|s| matches!(s, State::Pending))
    }
}
impl Drop for Jobs {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.tx.take();
        // Unblock a worker waiting to return a bitmap before joining it.
        self.rx.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
thread_local! { static JOBS: RefCell<Option<Jobs>> = const { RefCell::new(None) }; }

pub(crate) fn request(
    filename: &str,
    body: &str,
    pre: &str,
    post: &str,
    svg: bool,
) -> Result<Option<String>, String> {
    if pre.len() + body.len() + post.len() > 1 << 20 {
        return Err("LaTeX source exceeds 1 MiB".into());
    }
    JOBS.with(|cell| {
        cell.borrow_mut()
            .get_or_insert_with(Jobs::start)
            .request(Job {
                filename: filename.into(),
                body: body.into(),
                pre: pre.into(),
                post: post.into(),
                svg,
            })
    })
}
/// The actor is the only thread that can register completed media.
pub(crate) fn collect(col: &mut anki::collection::Collection) -> bool {
    JOBS.with(|cell| {
        cell.borrow_mut().as_mut().is_some_and(|jobs| {
            jobs.collect(|name, bytes| {
                anki::services::MediaService::add_media_file(
                    col,
                    anki_proto::media::AddMediaFileRequest {
                        desired_name: name.into(),
                        data: bytes,
                    },
                )
                .map(|result| result.val)
                .map_err(|error| error.to_string())
            })
        })
    })
}
pub(crate) fn pending() -> bool {
    JOBS.with(|cell| cell.borrow().as_ref().is_some_and(Jobs::pending))
}
pub(crate) fn shutdown() {
    JOBS.with(|cell| {
        cell.borrow_mut().take();
    });
}

fn generate_with(
    body: &str,
    pre: &str,
    post: &str,
    svg: bool,
    latex: &Path,
    converter: &Path,
    cancel: &AtomicBool,
) -> Result<Vec<u8>, String> {
    if pre.len() + body.len() + post.len() > 1 << 20 {
        return Err("LaTeX source exceeds 1 MiB".into());
    }
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    std::fs::write(dir.path().join("tmp.tex"), format!("{pre}\n{body}\n{post}"))
        .map_err(|e| e.to_string())?;
    run(
        latex,
        &[
            "-no-shell-escape",
            "-interaction=nonstopmode",
            "-halt-on-error",
            "tmp.tex",
        ],
        dir.path(),
        cancel,
    )?;
    let (args, file): (&[&str], _) = if svg {
        (
            &[
                "--no-fonts",
                "--exact",
                "-Z",
                "2",
                "tmp.dvi",
                "-o",
                "tmp.svg",
            ],
            "tmp.svg",
        )
    } else {
        (
            &[
                "-bg",
                "Transparent",
                "-D",
                "200",
                "-T",
                "tight",
                "tmp.dvi",
                "-o",
                "tmp.png",
            ],
            "tmp.png",
        )
    };
    run(converter, args, dir.path(), cancel)?;
    let path = dir.path().join(file);
    if std::fs::metadata(&path).map_err(|e| e.to_string())?.len() > 64 << 20 {
        return Err("generated LaTeX image exceeds 64 MiB".into());
    }
    std::fs::read(path).map_err(|e| e.to_string())
}

fn run(program: &Path, args: &[&str], dir: &Path, cancel: &AtomicBool) -> Result<(), String> {
    // No shell is involved. LaTeX cannot run shell commands even after opt-in.
    let mut child = Command::new(program)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("{}: {e}", program.display()))?;
    let start = Instant::now();
    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("LaTeX generation cancelled".into());
        }
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(format!("{} exited with {status}", program.display())),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e.to_string());
            }
            Ok(None) if start.elapsed() < Duration::from_secs(15) => {
                std::thread::sleep(Duration::from_millis(10))
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{} timed out", program.display()));
            }
        }
    }
}

pub(crate) fn error_html(error: &str) -> String {
    let escaped = error
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;");
    format!("<div class=latex-error>LaTeX generation failed: {escaped}</div>")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job() -> Job {
        Job {
            filename: "latex-test.png".into(),
            body: "x".into(),
            pre: "".into(),
            post: "".into(),
            svg: false,
        }
    }

    #[test]
    fn jobs_return_immediately_deduplicate_and_register_on_collector() {
        let (release, gate) = mpsc::channel();
        let mut jobs = Jobs::start_with(move |_, _| {
            gate.recv().unwrap();
            Ok(b"image".to_vec())
        });
        let start = Instant::now();
        assert_eq!(jobs.request(job()).unwrap(), None);
        assert_eq!(jobs.request(job()).unwrap(), None);
        assert!(start.elapsed() < Duration::from_millis(100));
        assert_eq!(jobs.states.len(), 1);
        assert!(jobs.pending());
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !jobs.collect(|name, bytes| {
            assert_eq!(name, "latex-test.png");
            assert_eq!(bytes, b"image");
            Ok("actual-name.png".into())
        }) {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(jobs.request(job()).unwrap(), Some("actual-name.png".into()));
        assert!(!jobs.pending());
    }

    #[test]
    fn failed_jobs_are_cached_and_shutdown_cancels_work() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = calls.clone();
        let mut jobs = Jobs::start_with(move |_, _| {
            count.fetch_add(1, Ordering::Relaxed);
            Err("bad formula".into())
        });
        jobs.request(job()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !jobs.collect(|_, _| panic!("failed output must never be imported")) {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(jobs.request(job()), Err("bad formula".into()));
        assert_eq!(jobs.request(job()), Err("bad formula".into()));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        let (started, running) = mpsc::channel();
        let mut pending = Jobs::start_with(move |_, cancelled| {
            started.send(()).unwrap();
            while !cancelled.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(2));
            }
            Err("cancelled".into())
        });
        pending.request(job()).unwrap();
        running.recv_timeout(Duration::from_secs(2)).unwrap();
        let start = Instant::now();
        drop(pending);
        assert!(start.elapsed() < Duration::from_millis(100));
    }

    #[test]
    fn shutdown_unblocks_a_full_result_queue() {
        let (started, running) = mpsc::channel();
        let mut jobs = Jobs::start_with(move |_, _| {
            started.send(()).unwrap();
            Ok(vec![0; 10])
        });
        let first = job();
        let mut second = first.clone();
        second.filename = "latex-second.png".into();
        jobs.request(first).unwrap();
        jobs.request(second).unwrap();
        running.recv_timeout(Duration::from_secs(2)).unwrap();
        running.recv_timeout(Duration::from_secs(2)).unwrap();
        let start = Instant::now();
        drop(jobs);
        assert!(start.elapsed() < Duration::from_millis(100));
    }
    #[test]
    fn absent_tool_is_an_error_and_html_is_escaped() {
        assert!(generate_with(
            "x",
            "",
            "",
            false,
            Path::new("/tanki-test/nonexistent-latex"),
            Path::new("/missing"),
            &AtomicBool::new(false)
        )
        .is_err());
        assert_eq!(
            error_html("<bad>&"),
            "<div class=latex-error>LaTeX generation failed: &lt;bad&gt;&amp;</div>"
        );
    }

    #[cfg(unix)]
    #[test]
    fn tools_use_private_working_directory_and_anki_arguments() {
        use std::os::unix::fs::PermissionsExt;
        let tools = tempfile::tempdir().unwrap();
        let latex = tools.path().join("latex");
        let converter = tools.path().join("dvipng");
        std::fs::write(&latex, "#!/bin/sh\n[ \"$1\" = -no-shell-escape ] || exit 4\n[ \"$4\" = tmp.tex ] || exit 5\n[ -f tmp.tex ] || exit 6\nprintf dvi > tmp.dvi\n").unwrap();
        std::fs::write(&converter, "#!/bin/sh\n[ \"$1\" = -bg ] || exit 4\n[ -f tmp.dvi ] || exit 5\nprintf generated > tmp.png\n").unwrap();
        for path in [&latex, &converter] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        assert_eq!(
            generate_with(
                "x",
                "pre",
                "post",
                false,
                &latex,
                &converter,
                &AtomicBool::new(false)
            )
            .unwrap(),
            b"generated"
        );
        assert!(!tools.path().join("tmp.tex").exists());
    }
}
