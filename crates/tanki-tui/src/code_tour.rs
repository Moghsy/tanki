//! Bounded, cancellable JSON bridge to code-tour.nvim. No shell interpolation.
use serde_json::{json, Value};
use std::{
    io::Read,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread,
    time::{Duration, Instant},
};
use tanki_core::{code_tour::CodeTourRef, config::CodeTourSettings};

pub type BridgeResult = Result<Value, String>;
const OUTPUT_LIMIT: usize = 1024 * 1024;

pub fn request(
    settings: &CodeTourSettings,
    args: &[String],
    cancelled: &AtomicBool,
) -> BridgeResult {
    let (program, prefix) = settings
        .command
        .split_first()
        .ok_or("Configure code_tour.command")?;
    if program.is_empty() {
        return Err("Configure code_tour.command".into());
    }
    if settings.nvim_appname.is_empty()
        || std::path::Path::new(&settings.nvim_appname).is_absolute()
        || settings.nvim_appname.split('/').any(|p| p == "..")
    {
        return Err("Invalid Neovim profile".into());
    }
    let mut command = Command::new(program);
    command
        .args(prefix)
        .args(args)
        .env("NVIM_APPNAME", &settings.nvim_appname)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (name, path) in [
        ("XDG_CONFIG_HOME", &settings.xdg_config_home),
        ("XDG_DATA_HOME", &settings.xdg_data_home),
        ("XDG_STATE_HOME", &settings.xdg_state_home),
    ] {
        if let Some(path) = path {
            if !path.is_absolute() {
                return Err(format!("{name} must be absolute"));
            }
            command.env(name, path);
        }
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("Could not start code-tour: {e}"))?;
    let overflow = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    for (index, pipe) in [
        (
            0,
            Box::new(child.stdout.take().unwrap()) as Box<dyn Read + Send>,
        ),
        (
            1,
            Box::new(child.stderr.take().unwrap()) as Box<dyn Read + Send>,
        ),
    ] {
        let tx = tx.clone();
        let overflow = overflow.clone();
        thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = pipe.take(OUTPUT_LIMIT as u64 + 1).read_to_end(&mut bytes);
            if bytes.len() > OUTPUT_LIMIT {
                overflow.store(true, Ordering::Relaxed);
            }
            let _ = tx.send((index, result.map(|_| bytes).map_err(|e| e.to_string())));
        });
    }
    drop(tx);
    let start = Instant::now();
    let status = loop {
        if cancelled.load(Ordering::Relaxed)
            || overflow.load(Ordering::Relaxed)
            || start.elapsed()
                > Duration::from_secs(if args.iter().any(|a| a == "--new-editor") {
                    45
                } else {
                    20
                })
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(if cancelled.load(Ordering::Relaxed) {
                "Tour request cancelled"
            } else if overflow.load(Ordering::Relaxed) {
                "Tour response exceeds 1 MiB"
            } else {
                "Tour request timed out"
            }
            .into());
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e.to_string());
            }
        }
    };
    let mut output = [Vec::new(), Vec::new()];
    for _ in 0..2 {
        let (index, bytes) = rx
            .recv_timeout(Duration::from_secs(1))
            .map_err(|_| "Tour output pipe did not close")?;
        output[index] = bytes?;
    }
    if output.iter().any(|b| b.len() > OUTPUT_LIMIT) {
        return Err("Tour response exceeds 1 MiB".into());
    }
    if !status.success() {
        return Err(String::from_utf8_lossy(&output[1])
            .chars()
            .take(1000)
            .collect());
    }
    let value: Value =
        serde_json::from_slice(&output[0]).map_err(|e| format!("Invalid code-tour JSON: {e}"))?;
    if let Some(error) = value.get("error").filter(|v| !v.is_null()) {
        return Err(error.to_string());
    }
    Ok(value)
}
fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| (*s).to_owned()).collect()
}
pub fn capabilities(settings: &CodeTourSettings, cancel: &AtomicBool) -> BridgeResult {
    let value = request(settings, &strings(&["capabilities"]), cancel)?;
    if value["protocol"] != 1
        || value["standalone"] != true
        || value["inspect"] != true
        || value["no_focus"] != true
        || (settings.lazy_claude && value["lazy_agent"] != true)
        || (settings.launch_in_herdr && value["new_editor"] != true)
    {
        return Err("Update code-tour.nvim using scripts/install-code-tour-bridge.py".into());
    }
    Ok(value)
}
pub fn inspect(
    settings: &CodeTourSettings,
    reference: &CodeTourRef,
    cancel: &AtomicBool,
) -> BridgeResult {
    let value = request(
        settings,
        &strings(&["saved", "inspect", reference.id()]),
        cancel,
    )?;
    if value["id"] != reference.id() {
        return Err("Tour metadata ID mismatch".into());
    }
    Ok(value)
}
pub fn list(settings: &CodeTourSettings, limit: usize, cancel: &AtomicBool) -> BridgeResult {
    if limit == 0 || limit > 10000 {
        return Err("Tour list limit must be between 1 and 10000".into());
    }
    request(
        settings,
        &strings(&["saved", "list", "--limit", &limit.to_string()]),
        cancel,
    )
}
pub fn editors(settings: &CodeTourSettings, cancel: &AtomicBool) -> BridgeResult {
    capabilities(settings, cancel)?;
    let value = request(settings, &strings(&["--list"]), cancel)?;
    let editors = value.as_array().ok_or("Invalid editor list")?;
    if editors
        .iter()
        .any(|e| e["address"].as_str().is_none() || e["instance_id"].as_str().is_none())
    {
        return Err("Invalid Neovim instance identity".into());
    }
    Ok(value)
}
pub fn open(
    settings: &CodeTourSettings,
    reference: &CodeTourRef,
    editor: &Value,
    cancel: &AtomicBool,
) -> BridgeResult {
    let launch = editor["launch"] == true;
    let live = editors(settings, cancel)?;
    let mut args = strings(&["saved", "open", reference.id(), "--standalone"]);
    if launch {
        if !settings.launch_in_herdr
            || std::env::var("HERDR_ENV").as_deref() != Ok("1")
            || std::env::var("HERDR_SESSION")
                .unwrap_or_default()
                .is_empty()
        {
            return Err(
                "No editor found. Run tanki inside Herdr to launch Neovim, or start Neovim first"
                    .into(),
            );
        }
        args.push("--new-editor".into());
    } else {
        let address = editor["address"]
            .as_str()
            .ok_or("No selected Neovim address")?;
        if !live
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["address"] == address && e["instance_id"] == editor["instance_id"])
        {
            return Err("Selected Neovim instance has closed or changed; choose it again".into());
        }
        args.extend(strings(&["--pane", address]));
    }
    if settings.lazy_claude {
        args.extend(strings(&["--lazy-agent", "claude"]));
    }
    // The CLI records focus failures separately from activation failures.
    for (repo, path) in &settings.repositories {
        if !path.is_absolute() {
            return Err("Repository mappings must be absolute".into());
        }
        args.extend([
            "--repo".into(),
            repo.clone(),
            path.to_string_lossy().into_owned(),
        ]);
    }
    let value = request(settings, &args, cancel)?;
    if value["id"] != reference.id() || value["opened"].as_u64().unwrap_or(0) == 0 {
        return Err("Neovim did not activate the requested tour".into());
    }
    Ok(value)
}

pub struct Job {
    receiver: mpsc::Receiver<BridgeResult>,
    cancel: Arc<AtomicBool>,
}
impl Job {
    pub fn spawn(f: impl FnOnce(&AtomicBool) -> BridgeResult + Send + 'static) -> Self {
        let (tx, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        thread::spawn(move || {
            let _ = tx.send(f(&worker_cancel));
        });
        Self { receiver, cancel }
    }
    pub fn poll(&self) -> Option<BridgeResult> {
        match self.receiver.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(_) => Some(Err("Tour worker disconnected".into())),
        }
    }
    #[cfg(test)]
    pub fn ready(result: BridgeResult) -> Self {
        let (tx, receiver) = mpsc::channel();
        tx.send(result).unwrap();
        Self {
            receiver,
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }
}
impl Drop for Job {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

pub fn doctor(settings: &CodeTourSettings) -> BridgeResult {
    let cancel = AtomicBool::new(false);
    Ok(
        json!({"capabilities": capabilities(settings, &cancel)?, "editors": editors(settings, &cancel)?, "nvim_appname": settings.nvim_appname, "enabled": settings.enabled}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn settings(root: &std::path::Path, source: &str) -> CodeTourSettings {
        let script = root.join("fake.py");
        std::fs::write(&script, source).unwrap();
        CodeTourSettings {
            command: vec!["python3".into(), script.to_string_lossy().into_owned()],
            nvim_appname: "synthetic".into(),
            ..Default::default()
        }
    }
    #[test]
    fn validates_capabilities_json_and_bounded_output() {
        let root = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(false);
        let s = settings(
            root.path(),
            "print('{\"protocol\":1,\"standalone\":true,\"inspect\":true,\"no_focus\":true,\"lazy_agent\":true,\"new_editor\":true}')",
        );
        assert!(capabilities(&s, &cancel).is_ok());
        let s = settings(root.path(), "print('invalid')");
        assert!(capabilities(&s, &cancel).unwrap_err().contains("Invalid"));
        let s = settings(root.path(), "print('x' * 1100000)");
        assert!(request(&s, &[], &cancel).unwrap_err().contains("exceeds"));
        let s = settings(root.path(), "import time; time.sleep(30)");
        let start = Instant::now();
        assert!(request(&s, &[], &AtomicBool::new(true)).is_err());
        assert!(start.elapsed() < Duration::from_secs(2));
    }
    #[test]
    fn open_uses_argument_arrays_profile_and_instance_identity() {
        let root = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(false);
        let s = settings(
            root.path(),
            r#"import json, os, sys
args = sys.argv[1:]
if args == ['capabilities']: print(json.dumps(dict(protocol=1, standalone=True, inspect=True, no_focus=True,lazy_agent=True,new_editor=True)))
elif args == ['--list']: print(json.dumps([dict(address='/tmp/with spaces;literal', instance_id='one')]))
else:
 assert args == ['saved','open','0123456789abcdef0123456789abcdef','--standalone','--pane','/tmp/with spaces;literal','--lazy-agent','claude'], args
 assert os.environ['NVIM_APPNAME'] == 'synthetic'
 print(json.dumps(dict(id=args[2],opened=1)))
"#,
        );
        let r = CodeTourRef::from_id("0123456789abcdef0123456789abcdef").unwrap();
        let editor = json!({"address":"/tmp/with spaces;literal","instance_id":"one"});
        assert!(open(&s, &r, &editor, &cancel).is_ok());
        let changed = json!({"address":"/tmp/with spaces;literal","instance_id":"replaced"});
        assert!(open(&s, &r, &changed, &cancel)
            .unwrap_err()
            .contains("closed or changed"));
    }
}
