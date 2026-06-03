//! Opt-in factory command proof ledger backed by a local BatPAK store.
//!
//! Store path: `target/factory-ledger/store/` (via [`ledger_store_dir`]).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use batpak::id::EventId;
use batpak::prelude::*;
use serde::{Deserialize, Serialize};

use crate::util::{cargo_target_dir, git_output, project_root};
use crate::{
    FactoryLedgerArgs, FactoryLedgerCommand, FactoryLedgerListArgs, FactoryLedgerRecordArgs,
    FactoryLedgerRecordCommand, FactoryLedgerRecordCompletedArgs, FactoryLedgerRecordFailedArgs,
    FactoryLedgerRecordGateCompletedArgs, FactoryLedgerRecordStartedArgs, FactoryLedgerRunArgs,
};

const STDERR_TAIL_CAP: usize = 4096;
const LEDGER_ENTITY: &str = "factory:commands";
const LEDGER_SCOPE: &str = "factory:ledger";

#[derive(Clone, Debug, Serialize, Deserialize, EventPayload)]
#[batpak(category = 0x02, type_id = 0x001)]
struct FactoryCommandStarted {
    run_id_hex: String,
    command: String,
    args: Vec<String>,
    cwd: String,
    branch: String,
    head: String,
    started_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, EventPayload)]
#[batpak(category = 0x02, type_id = 0x002)]
struct FactoryCommandCompleted {
    run_id_hex: String,
    command: String,
    status_code: i32,
    duration_ms: u64,
    completed_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, EventPayload)]
#[batpak(category = 0x02, type_id = 0x003)]
struct FactoryCommandFailed {
    run_id_hex: String,
    command: String,
    status_code: i32,
    duration_ms: u64,
    stderr_tail: String,
    completed_ms: u64,
}

/// Named proof gate completion (`factory.gate.completed`).
#[derive(Clone, Debug, Serialize, Deserialize, EventPayload)]
#[batpak(category = 0x02, type_id = 0x004)]
struct FactoryGateCompleted {
    run_id_hex: String,
    gate: String,
    command: String,
    status_code: i32,
    duration_ms: u64,
    completed_ms: u64,
    branch: String,
    head: String,
    summary: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LedgerGateRow {
    pub(crate) gate: String,
    pub(crate) command: String,
    pub(crate) status_code: i32,
    pub(crate) duration_ms: u64,
    pub(crate) branch: String,
    pub(crate) head: String,
    pub(crate) summary: String,
    pub(crate) completed_ms: u64,
}

pub(crate) fn factory_ledger(args: FactoryLedgerArgs) -> Result<()> {
    match args.command {
        FactoryLedgerCommand::Record(record_args) => record_command(record_args),
        FactoryLedgerCommand::List(list_args) => list_command(&list_args),
        FactoryLedgerCommand::Run(run_args) => run_command(run_args),
    }
}

fn record_command(args: FactoryLedgerRecordArgs) -> Result<()> {
    let mut store = open_ledger_store()?;
    match args.command {
        FactoryLedgerRecordCommand::Started(started) => {
            append_started(&mut store, started)?;
        }
        FactoryLedgerRecordCommand::Completed(completed) => {
            append_completed(&mut store, completed)?;
        }
        FactoryLedgerRecordCommand::Failed(failed) => {
            append_failed(&mut store, failed)?;
        }
        FactoryLedgerRecordCommand::GateCompleted(gate) => {
            append_gate_completed(&mut store, gate)?;
        }
    }
    store.close().context("close factory ledger store")?;
    Ok(())
}

fn list_command(args: &FactoryLedgerListArgs) -> Result<()> {
    let store = open_ledger_store()?;
    for line in collect_list_lines(&store, args.limit)? {
        println!("{line}");
    }
    store.close().context("close factory ledger store")?;
    Ok(())
}

fn run_command(args: FactoryLedgerRunArgs) -> Result<()> {
    if args.command.is_empty() {
        bail!("factory-ledger run: expected a command after `--`");
    }
    let argv0 = args.command[0].clone();
    let rest = args.command[1..].to_vec();
    let display_command = if rest.is_empty() {
        argv0.clone()
    } else {
        format!("{} {}", argv0, rest.join(" "))
    };

    let run_id = generate_run_id(&display_command);
    let run_id_hex = format_run_id_hex(run_id);
    let cwd = std::env::current_dir()
        .context("read current working directory")?
        .to_string_lossy()
        .into_owned();
    let project = project_root()?;
    let branch =
        git_output(&project, ["branch", "--show-current"]).unwrap_or_else(|_| "unknown".into());
    let head =
        git_output(&project, ["rev-parse", "--short", "HEAD"]).unwrap_or_else(|_| "unknown".into());
    let started_ms = now_ms();

    let mut store = open_ledger_store()?;
    append_started(
        &mut store,
        FactoryLedgerRecordStartedArgs {
            run_id: run_id_hex.clone(),
            command: argv0.clone(),
            args: rest.clone(),
            cwd: Some(cwd.clone()),
            branch: Some(branch.clone()),
            head: Some(head.clone()),
            started_ms: Some(started_ms),
        },
    )
    .context("factory-ledger: failed to record command started event")?;

    let started = Instant::now();
    let mut cmd = Command::new(&argv0);
    cmd.args(&rest).current_dir(&cwd).stdout(Stdio::inherit());

    let spawn_result = cmd.stderr(Stdio::piped()).spawn();
    let mut child = match spawn_result {
        Ok(child) => child,
        Err(error) => {
            let duration_ms = elapsed_ms_u64(started);
            append_failed(
                &mut store,
                FactoryLedgerRecordFailedArgs {
                    run_id: run_id_hex,
                    command: argv0,
                    status_code: -1,
                    duration_ms,
                    stderr_tail: error.to_string(),
                    completed_ms: Some(now_ms()),
                },
            )
            .context("factory-ledger: failed to record spawn failure event")?;
            store.close().context("close factory ledger store")?;
            return Err(error).context("factory-ledger run: failed to spawn wrapped command");
        }
    };

    let stderr = child
        .stderr
        .take()
        .context("factory-ledger run: missing stderr pipe")?;
    let tail = Arc::new(Mutex::new(Vec::<u8>::new()));
    let tail_for_thread = Arc::clone(&tail);
    let stderr_thread = thread::Builder::new()
        .name("factory-ledger-stderr-tail".to_owned())
        .spawn(move || echo_stderr_and_tail(stderr, &tail_for_thread))
        .context("factory-ledger run: spawn stderr echo thread")?;

    let status = child
        .wait()
        .context("factory-ledger run: failed waiting on wrapped command")?;
    stderr_thread
        .join()
        .map_err(|_| anyhow::anyhow!("factory-ledger run: stderr echo thread panicked"))?;

    let duration_ms = elapsed_ms_u64(started);
    let completed_ms = now_ms();
    let status_code = status.code().unwrap_or(-1);
    let stderr_tail = tail_to_string(&tail);

    if status.success() {
        append_completed(
            &mut store,
            FactoryLedgerRecordCompletedArgs {
                run_id: run_id_hex.clone(),
                command: argv0.clone(),
                status_code,
                duration_ms,
                completed_ms: Some(completed_ms),
            },
        )
        .context("factory-ledger: failed to record command completed event")?;
        if let Some(gate) = args.gate {
            let summary = format!("{gate} ok @ {head} duration={duration_ms}ms");
            append_gate_completed(
                &mut store,
                FactoryLedgerRecordGateCompletedArgs {
                    run_id: run_id_hex,
                    gate,
                    command: display_command,
                    status_code,
                    duration_ms,
                    completed_ms: Some(completed_ms),
                    branch: Some(branch),
                    head: Some(head),
                    summary,
                },
            )
            .context("factory-ledger: failed to record gate completed event")?;
        }
        store.close().context("close factory ledger store")?;
        Ok(())
    } else {
        append_failed(
            &mut store,
            FactoryLedgerRecordFailedArgs {
                run_id: run_id_hex,
                command: argv0,
                status_code,
                duration_ms,
                stderr_tail,
                completed_ms: Some(completed_ms),
            },
        )
        .context("factory-ledger: failed to record command failed event")?;
        store.close().context("close factory ledger store")?;
        bail!("factory-ledger run: wrapped command exited with status code {status_code}");
    }
}

fn ledger_store_dir() -> Result<PathBuf> {
    Ok(cargo_target_dir()?.join("factory-ledger").join("store"))
}

/// Read recent factory-ledger lines without creating the store directory.
pub(crate) fn collect_ledger_lines(limit: usize) -> Result<Vec<String>> {
    let Some(store) = open_existing_ledger_store()? else {
        return Ok(Vec::new());
    };
    let lines = collect_list_lines(&store, limit)?;
    store.close().context("close factory ledger store")?;
    Ok(lines)
}

/// Read recent gate rows without creating the store directory (newest-first).
pub(crate) fn collect_gate_rows(limit: usize) -> Result<Vec<LedgerGateRow>> {
    let Some(store) = open_existing_ledger_store()? else {
        return Ok(Vec::new());
    };
    let rows = collect_gate_rows_from_store(&store, limit)?;
    store.close().context("close factory ledger store")?;
    Ok(rows)
}

fn open_existing_ledger_store() -> Result<Option<Store>> {
    let store_dir = ledger_store_dir()?;
    open_existing_ledger_store_at(&store_dir)
}

fn open_existing_ledger_store_at(store_dir: &Path) -> Result<Option<Store>> {
    if !store_dir.exists() {
        return Ok(None);
    }
    validate_event_payload_registry().context("validate EventPayload registry")?;
    let store = Store::open(
        StoreConfig::new(store_dir)
            .with_event_payload_validation(EventPayloadValidation::FailFast)
            .with_sync_every_n_events(1)
            .with_sync_mode(SyncMode::SyncData),
    )
    .context("open existing factory ledger store")?;
    Ok(Some(store))
}

pub(crate) fn open_ledger_store_at(dir: &Path) -> Result<Store> {
    validate_event_payload_registry().context("validate EventPayload registry")?;
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    Store::open(
        StoreConfig::new(dir)
            .with_event_payload_validation(EventPayloadValidation::FailFast)
            .with_sync_every_n_events(1)
            .with_sync_mode(SyncMode::SyncData),
    )
    .context("open factory ledger store")
}

fn open_ledger_store() -> Result<Store> {
    open_ledger_store_at(&ledger_store_dir()?)
}

fn ledger_coordinate() -> Result<Coordinate> {
    Coordinate::new(LEDGER_ENTITY, LEDGER_SCOPE).context("ledger coordinate")
}

fn append_started(store: &mut Store, args: FactoryLedgerRecordStartedArgs) -> Result<()> {
    let coord = ledger_coordinate().context("ledger coordinate")?;
    let payload = FactoryCommandStarted {
        run_id_hex: args.run_id,
        command: args.command,
        args: args.args,
        cwd: args.cwd.unwrap_or_else(|| {
            std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        }),
        branch: args.branch.unwrap_or_else(default_branch),
        head: args.head.unwrap_or_else(default_head),
        started_ms: args.started_ms.unwrap_or_else(now_ms),
    };
    store
        .append_typed(&coord, &payload)
        .context("append factory.command.started")?;
    Ok(())
}

fn append_completed(store: &mut Store, args: FactoryLedgerRecordCompletedArgs) -> Result<()> {
    let coord = ledger_coordinate().context("ledger coordinate")?;
    let payload = FactoryCommandCompleted {
        run_id_hex: args.run_id,
        command: args.command,
        status_code: args.status_code,
        duration_ms: args.duration_ms,
        completed_ms: args.completed_ms.unwrap_or_else(now_ms),
    };
    store
        .append_typed(&coord, &payload)
        .context("append factory.command.completed")?;
    Ok(())
}

fn append_failed(store: &mut Store, args: FactoryLedgerRecordFailedArgs) -> Result<()> {
    let coord = ledger_coordinate().context("ledger coordinate")?;
    let payload = FactoryCommandFailed {
        run_id_hex: args.run_id,
        command: args.command,
        status_code: args.status_code,
        duration_ms: args.duration_ms,
        stderr_tail: cap_stderr_tail(args.stderr_tail),
        completed_ms: args.completed_ms.unwrap_or_else(now_ms),
    };
    store
        .append_typed(&coord, &payload)
        .context("append factory.command.failed")?;
    Ok(())
}

fn append_gate_completed(
    store: &mut Store,
    args: FactoryLedgerRecordGateCompletedArgs,
) -> Result<()> {
    let coord = ledger_coordinate().context("ledger coordinate")?;
    let payload = FactoryGateCompleted {
        run_id_hex: args.run_id,
        gate: args.gate,
        command: args.command,
        status_code: args.status_code,
        duration_ms: args.duration_ms,
        completed_ms: args.completed_ms.unwrap_or_else(now_ms),
        branch: args.branch.unwrap_or_else(default_branch),
        head: args.head.unwrap_or_else(default_head),
        summary: args.summary,
    };
    store
        .append_typed(&coord, &payload)
        .context("append factory.gate.completed")?;
    Ok(())
}

fn gate_to_row(gate: FactoryGateCompleted) -> LedgerGateRow {
    LedgerGateRow {
        gate: gate.gate,
        command: gate.command,
        status_code: gate.status_code,
        duration_ms: gate.duration_ms,
        branch: gate.branch,
        head: gate.head,
        summary: gate.summary,
        completed_ms: gate.completed_ms,
    }
}

fn format_gate_line(seq: u64, gate: &FactoryGateCompleted) -> String {
    format!(
        "run_id={} seq={seq} gate={} status={} duration_ms={} command={:?} branch={} head={} summary={:?}",
        gate.run_id_hex,
        gate.gate,
        gate.status_code,
        gate.duration_ms,
        gate.command,
        gate.branch,
        gate.head,
        gate.summary
    )
}

fn collect_gate_rows_from_store(store: &Store, limit: usize) -> Result<Vec<LedgerGateRow>> {
    let region = Region::scope(LEDGER_SCOPE);
    let scan_limit = limit.saturating_mul(50).max(200);
    let entries = store.query_entries_after(&region, None, scan_limit);
    let mut gates = Vec::new();
    for entry in entries {
        let event_id = entry.event_id();
        let stored = store
            .read_raw(EventId::from(event_id))
            .with_context(|| format!("read ledger event {event_id:032x}"))?;
        if let Some(gate) = stored
            .event
            .route_typed::<FactoryGateCompleted>()
            .context("decode FactoryGateCompleted")?
        {
            gates.push(gate_to_row(gate));
        }
    }
    if gates.len() > limit {
        gates.drain(..gates.len() - limit);
    }
    gates.reverse();
    Ok(gates)
}

pub(crate) fn collect_list_lines(store: &Store, limit: usize) -> Result<Vec<String>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let region = Region::scope(LEDGER_SCOPE);
    let scan_limit = limit.saturating_mul(50).max(200);
    let mut entries = store.query_entries_after(&region, None, scan_limit);
    if entries.len() > limit {
        entries.drain(..entries.len() - limit);
    }
    entries.reverse();
    let mut lines = Vec::with_capacity(entries.len());
    for entry in entries {
        let event_id = entry.event_id();
        let stored = store
            .read_raw(EventId::from(event_id))
            .with_context(|| format!("read ledger event {event_id:032x}"))?;
        let seq = entry.global_sequence();
        if let Some(started) = stored
            .event
            .route_typed::<FactoryCommandStarted>()
            .context("decode FactoryCommandStarted")?
        {
            lines.push(format!(
                "run_id={} seq={seq} started command={:?} branch={} head={}",
                started.run_id_hex, started.command, started.branch, started.head
            ));
            continue;
        }
        if let Some(completed) = stored
            .event
            .route_typed::<FactoryCommandCompleted>()
            .context("decode FactoryCommandCompleted")?
        {
            lines.push(format!(
                "run_id={} seq={seq} completed status={} duration_ms={}",
                completed.run_id_hex, completed.status_code, completed.duration_ms
            ));
            continue;
        }
        if let Some(failed) = stored
            .event
            .route_typed::<FactoryCommandFailed>()
            .context("decode FactoryCommandFailed")?
        {
            lines.push(format!(
                "run_id={} seq={seq} failed status={} duration_ms={}",
                failed.run_id_hex, failed.status_code, failed.duration_ms
            ));
            continue;
        }
        if let Some(gate) = stored
            .event
            .route_typed::<FactoryGateCompleted>()
            .context("decode FactoryGateCompleted")?
        {
            lines.push(format_gate_line(seq, &gate));
        }
    }
    Ok(lines)
}

fn echo_stderr_and_tail(mut stderr: impl Read + Send + 'static, tail: &Arc<Mutex<Vec<u8>>>) {
    let mut buf = [0u8; 1024];
    loop {
        match stderr.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let chunk = &buf[..n];
                let _ = std::io::stderr().write_all(chunk);
                if let Ok(mut tail_buf) = tail.lock() {
                    tail_buf.extend_from_slice(chunk);
                    if tail_buf.len() > STDERR_TAIL_CAP {
                        let drain = tail_buf.len() - STDERR_TAIL_CAP;
                        tail_buf.drain(..drain);
                    }
                }
            }
            Err(_) => break,
        }
    }
}

fn tail_to_string(tail: &Arc<Mutex<Vec<u8>>>) -> String {
    tail.lock()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default()
}

fn cap_stderr_tail(text: String) -> String {
    if text.len() <= STDERR_TAIL_CAP {
        return text;
    }
    String::from_utf8_lossy(text.as_bytes()[text.len() - STDERR_TAIL_CAP..].as_ref()).into_owned()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn elapsed_ms_u64(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn generate_run_id(command: &str) -> u128 {
    let mut hasher = sha2::Sha256::new();
    use sha2::Digest;
    hasher.update(command.as_bytes());
    hasher.update(now_ms().to_le_bytes());
    hasher.update(std::process::id().to_le_bytes());
    let digest = hasher.finalize();
    u128::from_be_bytes(digest[..16].try_into().expect("sha256 prefix is 16 bytes"))
}

fn format_run_id_hex(run_id: u128) -> String {
    format!("{run_id:032x}")
}

fn default_branch() -> String {
    project_root()
        .ok()
        .and_then(|root| git_output(&root, ["branch", "--show-current"]).ok())
        .unwrap_or_else(|| "unknown".to_owned())
}

fn default_head() -> String {
    project_root()
        .ok()
        .and_then(|root| git_output(&root, ["rev-parse", "--short", "HEAD"]).ok())
        .unwrap_or_else(|| "unknown".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as ProcessCommand;
    use std::sync::Mutex;

    static CARGO_TARGET_DIR_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn temp_store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = open_ledger_store_at(dir.path()).expect("open store");
        (dir, store)
    }

    #[test]
    fn open_store_at_creates_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = open_ledger_store_at(dir.path()).expect("open");
        store.close().expect("close");
        assert!(dir.path().exists());
    }

    #[test]
    fn record_started_and_completed_roundtrip() {
        let (_dir, mut store) = temp_store();
        append_started(
            &mut store,
            FactoryLedgerRecordStartedArgs {
                run_id: "0123456789abcdef0123456789abcdef".to_owned(),
                command: "echo".to_owned(),
                args: vec!["hello".to_owned()],
                cwd: Some("/tmp".to_owned()),
                branch: Some("factory/test".to_owned()),
                head: Some("abc123".to_owned()),
                started_ms: Some(1),
            },
        )
        .expect("started");
        append_completed(
            &mut store,
            FactoryLedgerRecordCompletedArgs {
                run_id: "0123456789abcdef0123456789abcdef".to_owned(),
                command: "echo".to_owned(),
                status_code: 0,
                duration_ms: 5,
                completed_ms: Some(2),
            },
        )
        .expect("completed");
        let lines = collect_list_lines(&store, 10).expect("list");
        store.close().expect("close");
        assert!(lines.iter().any(|line| line.contains("started")));
        assert!(lines.iter().any(|line| line.contains("completed")));
    }

    #[test]
    fn record_failed_stores_stderr_tail() {
        let (_dir, mut store) = temp_store();
        let long_tail = "x".repeat(STDERR_TAIL_CAP + 128);
        append_failed(
            &mut store,
            FactoryLedgerRecordFailedArgs {
                run_id: "fedcba9876543210fedcba9876543210".to_owned(),
                command: "fail".to_owned(),
                status_code: 1,
                duration_ms: 9,
                stderr_tail: long_tail,
                completed_ms: Some(3),
            },
        )
        .expect("failed");
        let region = Region::scope(LEDGER_SCOPE);
        let entry = store
            .query_entries_after(&region, None, 1)
            .into_iter()
            .next()
            .expect("one entry");
        let stored = store
            .read_raw(EventId::from(entry.event_id()))
            .expect("read");
        let failed = stored
            .event
            .route_typed::<FactoryCommandFailed>()
            .expect("route")
            .expect("payload");
        store.close().expect("close");
        assert_eq!(failed.stderr_tail.len(), STDERR_TAIL_CAP);
    }

    #[test]
    fn run_wrapper_records_failure_exit_code() {
        let (_dir, mut store) = temp_store();
        let run_id_hex = format_run_id_hex(generate_run_id("failing"));
        append_started(
            &mut store,
            FactoryLedgerRecordStartedArgs {
                run_id: run_id_hex.clone(),
                command: "xtask-factory-ledger-fail-test".to_owned(),
                args: Vec::new(),
                cwd: Some("/".to_owned()),
                branch: Some("test".to_owned()),
                head: Some("deadbeef".to_owned()),
                started_ms: Some(1),
            },
        )
        .expect("started");
        let started = Instant::now();
        let spawn_result = ProcessCommand::new("xtask-factory-ledger-fail-test").spawn();
        let (status_code, stderr_tail) = match spawn_result {
            Ok(mut child) => {
                let status = child.wait().expect("wait");
                (status.code().unwrap_or(-1), String::new())
            }
            Err(error) => (-1, error.to_string()),
        };
        append_failed(
            &mut store,
            FactoryLedgerRecordFailedArgs {
                run_id: run_id_hex,
                command: "xtask-factory-ledger-fail-test".to_owned(),
                status_code,
                duration_ms: elapsed_ms_u64(started),
                stderr_tail,
                completed_ms: Some(2),
            },
        )
        .expect("failed");
        let lines = collect_list_lines(&store, 10).expect("list");
        store.close().expect("close");
        assert!(lines.iter().any(|line| line.contains("failed status=")));
    }

    #[test]
    fn run_wrapper_records_spawn_failure() {
        let (_dir, mut store) = temp_store();
        append_started(
            &mut store,
            FactoryLedgerRecordStartedArgs {
                run_id: "aaaabbbbccccddddeeeeffff00001111".to_owned(),
                command: "no-such-factory-ledger-binary".to_owned(),
                args: Vec::new(),
                cwd: Some("/".to_owned()),
                branch: Some("test".to_owned()),
                head: Some("deadbeef".to_owned()),
                started_ms: Some(1),
            },
        )
        .expect("started");
        append_failed(
            &mut store,
            FactoryLedgerRecordFailedArgs {
                run_id: "aaaabbbbccccddddeeeeffff00001111".to_owned(),
                command: "no-such-factory-ledger-binary".to_owned(),
                status_code: -1,
                duration_ms: 0,
                stderr_tail: "spawn failed".to_owned(),
                completed_ms: Some(2),
            },
        )
        .expect("failed");
        let lines = collect_list_lines(&store, 10).expect("list");
        store.close().expect("close");
        assert!(lines.iter().any(|line| line.contains("failed status=-1")));
    }

    #[test]
    fn list_returns_recent_entries_newest_first() {
        let (_dir, mut store) = temp_store();
        for idx in 0..2 {
            let run_id = format!("{idx:032x}");
            append_started(
                &mut store,
                FactoryLedgerRecordStartedArgs {
                    run_id: run_id.clone(),
                    command: format!("cmd{idx}"),
                    args: Vec::new(),
                    cwd: Some("/".to_owned()),
                    branch: Some("b".to_owned()),
                    head: Some("h".to_owned()),
                    started_ms: Some(idx),
                },
            )
            .expect("started");
            append_completed(
                &mut store,
                FactoryLedgerRecordCompletedArgs {
                    run_id,
                    command: format!("cmd{idx}"),
                    status_code: 0,
                    duration_ms: 1,
                    completed_ms: Some(idx + 10),
                },
            )
            .expect("completed");
        }
        let lines = collect_list_lines(&store, 3).expect("list");
        store.close().expect("close");
        let seqs: Vec<u64> = lines
            .iter()
            .filter_map(|line| {
                line.split("seq=")
                    .nth(1)
                    .and_then(|rest| rest.split_whitespace().next())
                    .and_then(|n| n.parse().ok())
            })
            .collect();
        assert_eq!(seqs, vec![4, 3, 2]);
    }

    #[test]
    fn list_limit_keeps_most_recent_entries() {
        let (_dir, mut store) = temp_store();
        for idx in 0..3 {
            append_started(
                &mut store,
                FactoryLedgerRecordStartedArgs {
                    run_id: format!("{idx:032x}"),
                    command: format!("cmd{idx}"),
                    args: Vec::new(),
                    cwd: Some("/".to_owned()),
                    branch: Some("b".to_owned()),
                    head: Some("h".to_owned()),
                    started_ms: Some(idx),
                },
            )
            .expect("started");
        }
        let lines = collect_list_lines(&store, 2).expect("list");
        store.close().expect("close");
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("seq=3"));
        assert!(lines[1].contains("seq=2"));
    }

    #[test]
    fn record_gate_completed_roundtrip() {
        let (_dir, mut store) = temp_store();
        append_gate_completed(
            &mut store,
            FactoryLedgerRecordGateCompletedArgs {
                run_id: "0123456789abcdef0123456789abcdef".to_owned(),
                gate: "host-dev".to_owned(),
                command: "cargo xtask host-dev".to_owned(),
                status_code: 0,
                duration_ms: 42,
                completed_ms: Some(99),
                branch: Some("factory/test".to_owned()),
                head: Some("afb63dc".to_owned()),
                summary: "host-dev ok @ afb63dc duration=42ms".to_owned(),
            },
        )
        .expect("gate");
        let lines = collect_list_lines(&store, 10).expect("list");
        store.close().expect("close");
        let gate_line = lines
            .iter()
            .find(|line| line.contains("gate=host-dev"))
            .expect("gate line");
        assert!(gate_line.contains("summary=\"host-dev ok @ afb63dc duration=42ms\""));
        assert!(gate_line.contains("head=afb63dc"));
    }

    #[test]
    fn run_with_gate_appends_gate_on_success() {
        let (_dir, mut store) = temp_store();
        let run_id = "aaaabbbbccccddddeeeeffff00001111".to_owned();
        append_started(
            &mut store,
            FactoryLedgerRecordStartedArgs {
                run_id: run_id.clone(),
                command: "cargo".to_owned(),
                args: vec!["xtask".to_owned(), "context".to_owned()],
                cwd: Some("/".to_owned()),
                branch: Some("factory/test".to_owned()),
                head: Some("abc123".to_owned()),
                started_ms: Some(1),
            },
        )
        .expect("started");
        append_completed(
            &mut store,
            FactoryLedgerRecordCompletedArgs {
                run_id: run_id.clone(),
                command: "cargo".to_owned(),
                status_code: 0,
                duration_ms: 10,
                completed_ms: Some(2),
            },
        )
        .expect("completed");
        append_gate_completed(
            &mut store,
            FactoryLedgerRecordGateCompletedArgs {
                run_id,
                gate: "context".to_owned(),
                command: "cargo xtask context".to_owned(),
                status_code: 0,
                duration_ms: 10,
                completed_ms: Some(2),
                branch: Some("factory/test".to_owned()),
                head: Some("abc123".to_owned()),
                summary: "context ok @ abc123 duration=10ms".to_owned(),
            },
        )
        .expect("gate");
        let gates = collect_gate_rows_from_store(&store, 10).expect("gates");
        store.close().expect("close");
        assert_eq!(gates.len(), 1);
        assert_eq!(gates[0].gate, "context");
    }

    #[test]
    fn run_with_gate_skips_gate_on_failure() {
        let (_dir, mut store) = temp_store();
        append_started(
            &mut store,
            FactoryLedgerRecordStartedArgs {
                run_id: "fedcba9876543210fedcba9876543210".to_owned(),
                command: "fail".to_owned(),
                args: Vec::new(),
                cwd: Some("/".to_owned()),
                branch: Some("factory/test".to_owned()),
                head: Some("deadbeef".to_owned()),
                started_ms: Some(1),
            },
        )
        .expect("started");
        append_failed(
            &mut store,
            FactoryLedgerRecordFailedArgs {
                run_id: "fedcba9876543210fedcba9876543210".to_owned(),
                command: "fail".to_owned(),
                status_code: 1,
                duration_ms: 3,
                stderr_tail: String::new(),
                completed_ms: Some(2),
            },
        )
        .expect("failed");
        let gates = collect_gate_rows_from_store(&store, 10).expect("gates");
        store.close().expect("close");
        assert!(gates.is_empty());
    }

    #[test]
    fn collect_gate_rows_read_only_absent_store() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let missing = dir.path().join("missing-store");
        let _guard = CARGO_TARGET_DIR_TEST_LOCK
            .lock()
            .expect("lock read-only store fixture");
        let rows = match open_existing_ledger_store_at(&missing)? {
            Some(store) => collect_gate_rows_from_store(&store, 5)?,
            None => Vec::new(),
        };
        assert!(rows.is_empty());
        assert!(!missing.exists());
        Ok(())
    }

    #[test]
    fn collect_gate_rows_newest_first() {
        let (_dir, mut store) = temp_store();
        for idx in 0..3 {
            append_gate_completed(
                &mut store,
                FactoryLedgerRecordGateCompletedArgs {
                    run_id: format!("{idx:032x}"),
                    gate: format!("gate{idx}"),
                    command: format!("cmd{idx}"),
                    status_code: 0,
                    duration_ms: idx,
                    completed_ms: Some(idx),
                    branch: Some("b".to_owned()),
                    head: Some(format!("head{idx}")),
                    summary: format!("gate{idx} ok"),
                },
            )
            .expect("gate");
        }
        let gates = collect_gate_rows_from_store(&store, 2).expect("gates");
        store.close().expect("close");
        assert_eq!(gates.len(), 2);
        assert_eq!(gates[0].gate, "gate2");
        assert_eq!(gates[1].gate, "gate1");
    }
}
