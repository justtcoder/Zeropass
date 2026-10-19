use std::{
    collections::{HashMap, HashSet},
    fs,
    io::SeekFrom,
    path::{Component, Path, PathBuf},
    sync::{
        atomic::{AtomicU32, AtomicU64, Ordering},
        Arc,
    },
    time::Instant,
};

use anyhow::{bail, ensure, Context, Result};
use memmap2::MmapOptions;
use quinn::{Connection, RecvStream, SendStream};
use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    task::JoinSet,
};

use super::{clipboard, quic_engine};

const STREAM_CHUNK_BYTES: usize = 8 * 1024 * 1024;
const MAX_FILE_BYTES: u64 = 1024 * 1024 * 1024 * 1024;
const MAX_RELATIVE_PATH_BYTES: usize = 4096;
const FILE_STREAM_KIND: u8 = 1;
const RESUME_STATUS_READY: u8 = 0;
const RESUME_STATUS_COMPLETE: u8 = 1;
const MAX_PARALLEL_FILE_STREAMS: usize = 4;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum EntryKind {
    File,
    Directory,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TransferEntry {
    token: String,
    transfer_id: String,
    relative_path: String,
    kind: EntryKind,
    file_size: u64,
    entry_index: u32,
    entry_count: u32,
    file_count: u32,
    total_bytes: u64,
    mode: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferProgress {
    pub transfer_id: String,
    pub direction: String,
    pub current_path: String,
    pub transferred_bytes: u64,
    pub total_bytes: u64,
    pub completed_entries: u32,
    pub total_entries: u32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferSummary {
    pub transfer_id: String,
    pub files: u32,
    pub entries: u32,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TransferComplete {
    transfer_id: String,
    files: u32,
    entries: u32,
    total_bytes: u64,
    saved_to: String,
}

#[derive(Debug)]
struct SourceEntry {
    source: Option<PathBuf>,
    relative_path: String,
    kind: EntryKind,
    file_size: u64,
}

pub type TransferSessions = Arc<tokio::sync::Mutex<HashMap<String, TransferSession>>>;

pub struct TransferSession {
    root: PathBuf,
    total_bytes: u64,
    total_entries: u32,
    file_count: u32,
    received_bytes: u64,
    completed_entries: HashSet<u32>,
    entry_paths: HashMap<u32, String>,
    entry_bytes: HashMap<u32, u64>,
    received_files: u32,
    entry_locks: HashMap<u32, Arc<tokio::sync::Mutex<()>>>,
}

pub fn new_transfer_sessions() -> TransferSessions {
    Arc::new(tokio::sync::Mutex::new(HashMap::new()))
}

pub async fn has_incomplete_transfers(sessions: &TransferSessions) -> bool {
    sessions
        .lock()
        .await
        .values()
        .any(|session| session.completed_entries.len() < session.total_entries as usize)
}

pub async fn send_paths(
    address: std::net::SocketAddr,
    certificate_der: &[u8],
    token: &str,
    transfer_id: String,
    paths: Vec<String>,
    app: tauri::AppHandle,
) -> Result<TransferSummary> {
    ensure!(!paths.is_empty(), "select at least one file or folder");
    let entries = tokio::task::spawn_blocking(move || collect_sources(paths))
        .await
        .context("file indexing worker failed")??;
    ensure!(
        !entries.is_empty(),
        "selected folders contain no transferable entries"
    );
    ensure!(
        entries.len() <= u32::MAX as usize,
        "too many selected entries"
    );

    let total_bytes = entries.iter().try_fold(0_u64, |total, entry| {
        total
            .checked_add(entry.file_size)
            .context("selected data size overflow")
    })?;
    let file_count = entries
        .iter()
        .filter(|entry| entry.kind == EntryKind::File)
        .count() as u32;
    let entry_count = entries.len() as u32;
    ensure!(is_transfer_id(&transfer_id), "invalid transfer identifier");

    let mut endpoint = quinn::Endpoint::client("0.0.0.0:0".parse()?)?;
    endpoint.set_default_client_config(quic_engine::client_config(certificate_der)?);
    let connection = endpoint.connect(address, "zeropass.local")?.await?;
    let started = Instant::now();
    let transferred_bytes = Arc::new(AtomicU64::new(0));
    let completed_entries = Arc::new(AtomicU32::new(0));
    let mut pending_entries = entries.into_iter().enumerate();
    let mut tasks = JoinSet::new();

    for _ in 0..MAX_PARALLEL_FILE_STREAMS {
        if let Some((index, entry)) = pending_entries.next() {
            spawn_entry(
                &mut tasks,
                index,
                entry,
                connection.clone(),
                token.to_owned(),
                transfer_id.clone(),
                entry_count,
                file_count,
                total_bytes,
                app.clone(),
                transferred_bytes.clone(),
                completed_entries.clone(),
            );
        }
    }
    while let Some(result) = tasks.join_next().await {
        result.context("file transfer worker failed")??;
        if let Some((index, entry)) = pending_entries.next() {
            spawn_entry(
                &mut tasks,
                index,
                entry,
                connection.clone(),
                token.to_owned(),
                transfer_id.clone(),
                entry_count,
                file_count,
                total_bytes,
                app.clone(),
                transferred_bytes.clone(),
                completed_entries.clone(),
            );
        }
    }

    connection.close(0_u32.into(), b"transfer complete");
    endpoint.wait_idle().await;
    let elapsed = started.elapsed().as_secs_f64().max(f64::EPSILON);
    let transferred_bytes = transferred_bytes.load(Ordering::Relaxed);
    eprintln!(
        "sent {:.1} MiB/s",
        transferred_bytes as f64 / elapsed / 1_048_576.0
    );
    let summary = TransferSummary {
        transfer_id,
        files: file_count,
        entries: entry_count,
        total_bytes,
    };
    let _ = app.emit("file-transfer-sent", &summary);
    Ok(summary)
}

#[allow(clippy::too_many_arguments)]
fn spawn_entry(
    tasks: &mut JoinSet<Result<()>>,
    index: usize,
    entry: SourceEntry,
    connection: Connection,
    token: String,
    transfer_id: String,
    entry_count: u32,
    file_count: u32,
    total_bytes: u64,
    app: tauri::AppHandle,
    transferred_bytes: Arc<AtomicU64>,
    completed_entries: Arc<AtomicU32>,
) {
    tasks.spawn(async move {
        send_entry(
            connection,
            index,
            entry,
            token,
            transfer_id,
            entry_count,
            file_count,
            total_bytes,
            app,
            transferred_bytes,
            completed_entries,
        )
        .await
    });
}

#[allow(clippy::too_many_arguments)]
async fn send_entry(
    connection: Connection,
    index: usize,
    entry: SourceEntry,
    token: String,
    transfer_id: String,
    entry_count: u32,
    file_count: u32,
    total_bytes: u64,
    app: tauri::AppHandle,
    transferred_bytes: Arc<AtomicU64>,
    completed_entries: Arc<AtomicU32>,
) -> Result<()> {
    let file_mode = entry
        .source
        .as_ref()
        .and_then(|source| fs::metadata(source).ok())
        .map(|metadata| metadata.permissions())
        .and_then(|permissions| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                Some(permissions.mode() & 0o7777)
            }
            #[cfg(not(unix))]
            {
                let _ = permissions;
                None
            }
        });
    let frame = TransferEntry {
        token,
        transfer_id: transfer_id.clone(),
        relative_path: entry.relative_path.clone(),
        kind: entry.kind,
        file_size: entry.file_size,
        entry_index: index as u32,
        entry_count,
        file_count,
        total_bytes,
        mode: file_mode,
    };
    let (mut send, mut receive) = connection.open_bi().await?;
    send.write_u8(FILE_STREAM_KIND).await?;
    write_frame(&mut send, &frame).await?;

    let mut resume_status = [0_u8; 1];
    receive.read_exact(&mut resume_status).await?;
    if resume_status[0] == RESUME_STATUS_COMPLETE {
        if entry.kind == EntryKind::File {
            transferred_bytes.fetch_add(entry.file_size, Ordering::Relaxed);
        }
        send.finish()?;
        let completed = completed_entries.fetch_add(1, Ordering::Relaxed) + 1;
        emit_progress(
            &app,
            &transfer_id,
            "sending",
            &entry.relative_path,
            transferred_bytes.load(Ordering::Relaxed),
            total_bytes,
            completed,
            entry_count,
        );
        return Ok(());
    }
    ensure!(
        resume_status[0] == RESUME_STATUS_READY,
        "receiver returned an invalid resume status"
    );
    let resume_offset = receive.read_u64().await?;
    ensure!(
        resume_offset <= entry.file_size,
        "receiver resume offset exceeds file size"
    );
    ensure!(
        resume_offset % STREAM_CHUNK_BYTES as u64 == 0,
        "receiver resume offset is not chunk-aligned"
    );
    transferred_bytes.fetch_add(resume_offset, Ordering::Relaxed);

    if let Some(source) = &entry.source {
        let file = fs::File::open(source).context("open selected file")?;
        ensure!(
            file.metadata()?.len() == entry.file_size,
            "source changed after indexing"
        );
        let mut hasher = blake3::Hasher::new();
        let mapping = if entry.file_size == 0 {
            None
        } else {
            Some(unsafe { MmapOptions::new().map(&file).context("map selected file")? })
        };
        if let Some(mapping) = mapping.as_ref() {
            let prefix_len =
                usize::try_from(resume_offset).context("resume offset exceeds address space")?;
            hasher.update(&mapping[..prefix_len]);
        }
        if let Some(mapping) = mapping {
            let mut last_report = Instant::now();
            let offset =
                usize::try_from(resume_offset).context("resume offset exceeds address space")?;
            for chunk in mapping[offset..].chunks(STREAM_CHUNK_BYTES) {
                hasher.update(chunk);
                send.write_all(chunk).await?;
                let total_sent = transferred_bytes.fetch_add(chunk.len() as u64, Ordering::Relaxed)
                    + chunk.len() as u64;
                if last_report.elapsed().as_millis() >= 150 {
                    emit_progress(
                        &app,
                        &transfer_id,
                        "sending",
                        &entry.relative_path,
                        total_sent,
                        total_bytes,
                        completed_entries.load(Ordering::Relaxed),
                        entry_count,
                    );
                    last_report = Instant::now();
                }
            }
        }
        send.write_all(hasher.finalize().as_bytes()).await?;
    }

    send.finish()?;
    let mut acknowledgement = [0_u8; 2];
    receive.read_exact(&mut acknowledgement).await?;
    ensure!(
        &acknowledgement == b"OK",
        "receiver rejected {}",
        entry.relative_path
    );
    let completed = completed_entries.fetch_add(1, Ordering::Relaxed) + 1;
    emit_progress(
        &app,
        &transfer_id,
        "sending",
        &entry.relative_path,
        transferred_bytes.load(Ordering::Relaxed),
        total_bytes,
        completed,
        entry_count,
    );
    Ok(())
}

pub async fn receive_entry(
    send: &mut SendStream,
    receive: &mut RecvStream,
    expected_token: &str,
    app: &tauri::AppHandle,
    sessions: &TransferSessions,
) -> Result<()> {
    let frame: TransferEntry = read_frame(receive).await?;
    clipboard::validate_token(&frame.token, expected_token)?;
    ensure!(
        is_transfer_id(&frame.transfer_id),
        "invalid transfer identifier"
    );
    ensure!(
        frame.file_size <= MAX_FILE_BYTES,
        "file exceeds the 1 TiB transfer limit"
    );
    ensure!(
        frame.entry_count > 0 && frame.entry_index < frame.entry_count,
        "invalid transfer entry index"
    );
    ensure!(
        frame.file_count <= frame.entry_count,
        "invalid transfer file count"
    );
    ensure!(
        frame.total_bytes >= frame.file_size,
        "invalid transfer byte count"
    );
    ensure!(
        frame.kind != EntryKind::Directory || frame.file_size == 0,
        "directory entries cannot contain file data"
    );
    let relative_path = validate_relative_path(&frame.relative_path)?;

    let downloads = app
        .path()
        .download_dir()
        .context("resolve Downloads directory")?;
    tokio::fs::create_dir_all(&downloads)
        .await
        .context("create Downloads directory")?;
    let storage_root = downloads.join("ZEROPASS");
    tokio::fs::create_dir_all(&storage_root)
        .await
        .context("create ZEROPASS Downloads folder")?;
    ensure!(
        !tokio::fs::symlink_metadata(&storage_root)
            .await?
            .file_type()
            .is_symlink(),
        "ZEROPASS Downloads folder cannot be a symbolic link"
    );

    let (transfer_root, entry_lock, already_complete) = {
        let mut active_sessions = sessions.lock().await;
        if !active_sessions.contains_key(&frame.transfer_id) {
            let root = storage_root.join(&frame.transfer_id);
            tokio::fs::create_dir(&root)
                .await
                .context("create unique transfer folder")?;
            active_sessions.insert(
                frame.transfer_id.clone(),
                TransferSession {
                    root,
                    total_bytes: frame.total_bytes,
                    total_entries: frame.entry_count,
                    file_count: frame.file_count,
                    received_bytes: 0,
                    completed_entries: HashSet::new(),
                    entry_paths: HashMap::new(),
                    entry_bytes: HashMap::new(),
                    received_files: 0,
                    entry_locks: HashMap::new(),
                },
            );
        }
        let session = active_sessions
            .get_mut(&frame.transfer_id)
            .expect("session was inserted");
        ensure!(
            session.total_bytes == frame.total_bytes
                && session.total_entries == frame.entry_count
                && session.file_count == frame.file_count,
            "inconsistent transfer manifest"
        );
        if let Some(known_path) = session.entry_paths.get(&frame.entry_index) {
            ensure!(
                known_path == &frame.relative_path,
                "transfer entry path changed during resume"
            );
        } else {
            session
                .entry_paths
                .insert(frame.entry_index, frame.relative_path.clone());
        }
        ensure!(
            session
                .entry_paths
                .iter()
                .filter(
                    |(index, path)| **index != frame.entry_index && *path == &frame.relative_path
                )
                .count()
                == 0,
            "duplicate transfer path"
        );
        let entry_lock = session
            .entry_locks
            .entry(frame.entry_index)
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        (
            session.root.clone(),
            entry_lock,
            session.completed_entries.contains(&frame.entry_index),
        )
    };

    if already_complete {
        send.write_u8(RESUME_STATUS_COMPLETE).await?;
        send.finish()?;
        return Ok(());
    }

    let _entry_guard = entry_lock.lock().await;
    let already_complete = sessions
        .lock()
        .await
        .get(&frame.transfer_id)
        .expect("session exists")
        .completed_entries
        .contains(&frame.entry_index);
    if already_complete {
        send.write_u8(RESUME_STATUS_COMPLETE).await?;
        send.finish()?;
        return Ok(());
    }

    let destination = transfer_root.join(relative_path);
    match frame.kind {
        EntryKind::Directory => {
            send.write_u8(RESUME_STATUS_READY).await?;
            send.write_u64(0).await?;
            tokio::fs::create_dir_all(&destination)
                .await
                .context("create received folder")?;
            apply_mode_bits(&destination, frame.mode).await?;
        }
        EntryKind::File => {
            if let Some(parent) = destination.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .context("create received parent folder")?;
            }
            let partial_path = partial_path(&destination, &frame.transfer_id)?;
            if let Ok(metadata) = tokio::fs::symlink_metadata(&partial_path).await {
                ensure!(
                    !metadata.file_type().is_symlink(),
                    "partial file cannot be a symbolic link"
                );
            }
            let mut file = tokio::fs::OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .open(&partial_path)
                .await
                .context("open resumable partial file")?;
            let partial_len = file.metadata().await?.len();
            let resume_offset = aligned_resume_offset(partial_len, frame.file_size);
            let cumulative = {
                let mut active_sessions = sessions.lock().await;
                let session = active_sessions
                    .get_mut(&frame.transfer_id)
                    .expect("session exists");
                reconcile_entry_progress(session, frame.entry_index, resume_offset)
            };
            file.set_len(resume_offset).await?;
            file.seek(SeekFrom::Start(0)).await?;
            let mut hasher = blake3::Hasher::new();
            let mut prefix_remaining = resume_offset;
            let mut buffer = vec![0_u8; STREAM_CHUNK_BYTES];
            while prefix_remaining > 0 {
                let amount = prefix_remaining.min(buffer.len() as u64) as usize;
                file.read_exact(&mut buffer[..amount]).await?;
                hasher.update(&buffer[..amount]);
                prefix_remaining -= amount as u64;
            }
            file.seek(SeekFrom::Start(resume_offset)).await?;
            send.write_u8(RESUME_STATUS_READY).await?;
            send.write_u64(resume_offset).await?;

            if resume_offset > 0 {
                emit_progress(
                    app,
                    &frame.transfer_id,
                    "receiving",
                    &frame.relative_path,
                    cumulative,
                    frame.total_bytes,
                    frame.entry_index,
                    frame.entry_count,
                );
            }

            let mut remaining = frame.file_size - resume_offset;
            let mut last_report = Instant::now();
            while remaining > 0 {
                let amount = remaining.min(buffer.len() as u64) as usize;
                receive.read_exact(&mut buffer[..amount]).await?;
                hasher.update(&buffer[..amount]);
                file.write_all(&buffer[..amount]).await?;
                remaining -= amount as u64;
                let cumulative = {
                    let mut active_sessions = sessions.lock().await;
                    let session = active_sessions
                        .get_mut(&frame.transfer_id)
                        .expect("session exists");
                    session.received_bytes = session.received_bytes.saturating_add(amount as u64);
                    let total = session.entry_bytes.entry(frame.entry_index).or_default();
                    *total = total.saturating_add(amount as u64);
                    session.received_bytes
                };
                if last_report.elapsed().as_millis() >= 150 {
                    emit_progress(
                        app,
                        &frame.transfer_id,
                        "receiving",
                        &frame.relative_path,
                        cumulative,
                        frame.total_bytes,
                        frame.entry_index,
                        frame.entry_count,
                    );
                    last_report = Instant::now();
                }
            }
            file.flush().await?;
            file.sync_all().await?;
            let mut received_hash = [0_u8; 32];
            receive.read_exact(&mut received_hash).await?;
            if received_hash != *hasher.finalize().as_bytes() {
                drop(file);
                let _ = tokio::fs::remove_file(&partial_path).await;
                bail!("BLAKE3 verification failed for {}", frame.relative_path);
            }
            drop(file);
            tokio::fs::rename(&partial_path, &destination)
                .await
                .context("atomically finalize verified file")?;
            apply_mode_bits(&destination, frame.mode).await?;
        }
    }

    let (cumulative_bytes, completed_entries, completion) = {
        let mut active_sessions = sessions.lock().await;
        let session = active_sessions
            .get_mut(&frame.transfer_id)
            .expect("session exists");
        ensure!(
            session.completed_entries.insert(frame.entry_index),
            "duplicate completed entry"
        );
        if frame.kind == EntryKind::File {
            session.received_files += 1;
        }
        let completed_count = session.completed_entries.len() as u32;
        let complete = if completed_count == session.total_entries {
            ensure!(
                session.received_bytes == session.total_bytes,
                "transfer byte count mismatch"
            );
            ensure!(
                session.received_files == session.file_count,
                "transfer file count mismatch"
            );
            Some(TransferComplete {
                transfer_id: frame.transfer_id.clone(),
                files: session.file_count,
                entries: session.total_entries,
                total_bytes: session.total_bytes,
                saved_to: session.root.display().to_string(),
            })
        } else {
            None
        };
        (session.received_bytes, completed_count, complete)
    };

    send.write_all(b"OK").await?;
    send.finish()?;
    emit_progress(
        app,
        &frame.transfer_id,
        "receiving",
        &frame.relative_path,
        cumulative_bytes,
        frame.total_bytes,
        completed_entries,
        frame.entry_count,
    );
    if let Some(complete) = completion {
        let _ = app.emit("file-transfer-received", complete);
    }
    Ok(())
}

fn partial_path(destination: &Path, transfer_id: &str) -> Result<PathBuf> {
    let file_name = destination
        .file_name()
        .context("destination has no file name")?
        .to_string_lossy();
    let parent = destination.parent().context("destination has no parent")?;
    Ok(parent.join(format!(".{file_name}.{transfer_id}.part")))
}

fn aligned_resume_offset(partial_len: u64, file_size: u64) -> u64 {
    if partial_len > file_size {
        return 0;
    }
    partial_len / STREAM_CHUNK_BYTES as u64 * STREAM_CHUNK_BYTES as u64
}

fn reconcile_entry_progress(
    session: &mut TransferSession,
    entry_index: u32,
    disk_offset: u64,
) -> u64 {
    let previous = session.entry_bytes.entry(entry_index).or_default();
    session.received_bytes = session
        .received_bytes
        .saturating_sub(*previous)
        .saturating_add(disk_offset);
    *previous = disk_offset;
    session.received_bytes
}

fn emit_progress(
    app: &tauri::AppHandle,
    transfer_id: &str,
    direction: &str,
    current_path: &str,
    transferred_bytes: u64,
    total_bytes: u64,
    completed_entries: u32,
    total_entries: u32,
) {
    let progress = TransferProgress {
        transfer_id: transfer_id.to_owned(),
        direction: direction.to_owned(),
        current_path: current_path.to_owned(),
        transferred_bytes,
        total_bytes,
        completed_entries,
        total_entries,
    };
    let _ = app.emit("file-transfer-progress", progress);
}

async fn write_frame<T: Serialize>(stream: &mut SendStream, value: &T) -> Result<()> {
    let frame = serde_json::to_vec(value)?;
    ensure!(frame.len() <= 64 * 1024, "file metadata frame is too large");
    stream.write_u32(frame.len() as u32).await?;
    stream.write_all(&frame).await?;
    Ok(())
}

async fn read_frame<T: for<'de> Deserialize<'de>>(stream: &mut RecvStream) -> Result<T> {
    let frame_len = stream.read_u32().await? as usize;
    ensure!(
        frame_len > 0 && frame_len <= 64 * 1024,
        "invalid file metadata frame size"
    );
    let mut frame = vec![0; frame_len];
    stream.read_exact(&mut frame).await?;
    Ok(serde_json::from_slice(&frame)?)
}

fn collect_sources(paths: Vec<String>) -> Result<Vec<SourceEntry>> {
    let mut entries = Vec::new();
    for selected in paths {
        ensure!(
            !fs::symlink_metadata(&selected)?.file_type().is_symlink(),
            "symbolic links are not included in transfers"
        );
        let canonical = fs::canonicalize(&selected)
            .with_context(|| format!("resolve selected path: {selected}"))?;
        let name = canonical
            .file_name()
            .context("selected path has no file name")?
            .to_string_lossy()
            .into_owned();
        let relative = PathBuf::from(name);
        let metadata = fs::symlink_metadata(&canonical)?;
        if metadata.file_type().is_symlink() {
            bail!("symbolic links are not included in transfers");
        } else if metadata.is_file() {
            entries.push(SourceEntry {
                source: Some(canonical),
                relative_path: portable_path(&relative),
                kind: EntryKind::File,
                file_size: metadata.len(),
            });
        } else if metadata.is_dir() {
            collect_directory(&canonical, &relative, &mut entries)?;
        } else {
            bail!("selected path is neither a regular file nor a folder");
        }
    }

    entries.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    let mut seen = HashSet::new();
    for entry in &entries {
        validate_relative_path(&entry.relative_path)?;
        ensure!(
            seen.insert(entry.relative_path.clone()),
            "duplicate selected path: {}",
            entry.relative_path
        );
    }
    Ok(entries)
}

fn collect_directory(root: &Path, relative: &Path, entries: &mut Vec<SourceEntry>) -> Result<()> {
    entries.push(SourceEntry {
        source: None,
        relative_path: portable_path(relative),
        kind: EntryKind::Directory,
        file_size: 0,
    });
    let mut children = fs::read_dir(root)?.collect::<std::io::Result<Vec<_>>>()?;
    children.sort_by_key(|entry| entry.file_name());
    for child in children {
        let metadata = fs::symlink_metadata(child.path())?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        let child_relative = relative.join(child.file_name());
        if metadata.is_dir() {
            collect_directory(&child.path(), &child_relative, entries)?;
        } else if metadata.is_file() {
            entries.push(SourceEntry {
                source: Some(child.path()),
                relative_path: portable_path(&child_relative),
                kind: EntryKind::File,
                file_size: metadata.len(),
            });
        }
    }
    Ok(())
}

async fn apply_mode_bits(path: &Path, mode: Option<u32>) -> Result<()> {
    let Some(mode) = mode else {
        return Ok(());
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_mode(mode);
        fs::set_permissions(path, permissions)?;
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
    Ok(())
}

fn portable_path(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn validate_relative_path(raw: &str) -> Result<PathBuf> {
    ensure!(
        !raw.is_empty() && raw.len() <= MAX_RELATIVE_PATH_BYTES,
        "invalid relative path length"
    );
    ensure!(
        !raw.starts_with('/') && !raw.starts_with('\\'),
        "absolute paths are not allowed"
    );
    let normalized = raw.replace('\\', "/");
    let path = Path::new(&normalized);
    let components: Vec<_> = path.components().collect();
    ensure!(
        !components.is_empty() && components.len() <= 128,
        "invalid relative path depth"
    );
    for component in &components {
        match component {
            Component::Normal(part) => {
                let name = part.to_string_lossy();
                ensure!(!name.contains(':'), "invalid path component");
            }
            _ => bail!("path traversal is not allowed"),
        }
    }
    Ok(path.to_path_buf())
}

fn is_transfer_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{HashMap, HashSet},
        fs,
        path::Path,
    };

    use super::{
        aligned_resume_offset, collect_sources, is_transfer_id, partial_path,
        reconcile_entry_progress, validate_relative_path, EntryKind, TransferSession,
        STREAM_CHUNK_BYTES,
    };

    #[test]
    fn rejects_parent_and_absolute_paths() {
        assert!(validate_relative_path("../outside.txt").is_err());
        assert!(validate_relative_path("folder/../../outside.txt").is_err());
        assert!(validate_relative_path("/etc/passwd").is_err());
        assert!(validate_relative_path("C:\\Windows\\file.txt").is_err());
    }

    #[test]
    fn permits_nested_relative_paths() {
        assert!(validate_relative_path("folder/subfolder/file.bin").is_ok());
    }

    #[test]
    fn validates_transfer_identifiers() {
        assert!(is_transfer_id("0123456789abcdef0123456789abcdef"));
        assert!(!is_transfer_id("../outside"));
    }

    #[test]
    fn resumes_only_from_complete_chunks_within_file_bounds() {
        let chunk = STREAM_CHUNK_BYTES as u64;
        assert_eq!(aligned_resume_offset(chunk * 3 + 19, chunk * 5), chunk * 3);
        assert_eq!(aligned_resume_offset(chunk * 3, chunk * 5), chunk * 3);
        assert_eq!(aligned_resume_offset(chunk * 6, chunk * 5), 0);
    }

    #[test]
    fn partial_paths_are_isolated_by_transfer_id() {
        let destination = Path::new("/downloads/transfer/report.bin");
        let first = partial_path(destination, "aabb").unwrap();
        let second = partial_path(destination, "ccdd").unwrap();
        assert_ne!(first, second);
        assert_eq!(first.file_name().unwrap(), ".report.bin.aabb.part");
    }

    #[test]
    fn reconciles_partial_progress_without_double_counting() {
        let mut session = TransferSession {
            root: Path::new("/tmp/transfer").to_path_buf(),
            total_bytes: 0,
            total_entries: 1,
            file_count: 1,
            received_bytes: 5_000_000,
            completed_entries: HashSet::new(),
            entry_paths: HashMap::new(),
            entry_bytes: HashMap::from([(0, 5_000_000)]),
            received_files: 0,
            entry_locks: HashMap::new(),
        };
        assert_eq!(
            reconcile_entry_progress(&mut session, 0, 4_194_304),
            4_194_304
        );
        assert_eq!(
            reconcile_entry_progress(&mut session, 0, 4_194_304),
            4_194_304
        );
        assert_eq!(
            reconcile_entry_progress(&mut session, 0, 8_388_608),
            8_388_608
        );
    }

    #[test]
    fn indexes_nested_files_and_empty_directories() {
        let root = std::env::temp_dir().join(format!(
            "zeropass-index-test-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let nested = root.join("source").join("nested");
        let empty = root.join("source").join("empty");
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir_all(&empty).unwrap();
        fs::write(nested.join("payload.bin"), b"data").unwrap();

        let entries =
            collect_sources(vec![root.join("source").to_string_lossy().into_owned()]).unwrap();
        assert!(entries.iter().any(
            |entry| entry.relative_path == "source/empty" && entry.kind == EntryKind::Directory
        ));
        assert!(entries.iter().any(
            |entry| entry.relative_path == "source/nested/payload.bin" && entry.file_size == 4
        ));
        fs::remove_dir_all(root).unwrap();
    }
}
