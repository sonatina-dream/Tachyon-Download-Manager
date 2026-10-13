mod common;

use std::sync::atomic::Ordering;
use std::time::Duration;

use common::*;
use tachyon_engine::{AddRequest, DownloadStatus, EngineEvent, PART_SUFFIX, RequestSpec};

fn request(url: &str, start: bool) -> AddRequest {
    AddRequest {
        request: RequestSpec::new(url),
        start,
        ..Default::default()
    }
}

#[test]
fn segmented_download_uses_many_connections() {
    let rt = server_runtime();
    // 8 MiB served at 2 MiB/s per connection: one connection would need 4 s.
    let server = rt.block_on(TestServer::start(ServerConfig {
        size: 8 << 20,
        throttle: 2 << 20,
        ..Default::default()
    }));
    let dir = tempfile::tempdir().unwrap();
    let engine = open_engine(dir.path(), |s| s.max_connections = 8);

    let started = std::time::Instant::now();
    let id = engine.add(request(&server.url, true)).unwrap();
    let snap = wait_finished(&engine, id);
    let elapsed = started.elapsed();

    assert_eq!(snap.status, DownloadStatus::Completed, "{:?}", snap.error);
    assert_eq!(snap.file_name, "test file.bin");
    assert_eq!(snap.size, Some(8 << 20));
    assert_eq!(snap.resumable, Some(true));
    assert_eq!(read(snap.path()), *server.body);
    assert!(!snap.directory.join(format!("test file.bin{PART_SUFFIX}")).exists());
    let max_active = server.stats.max_active.load(Ordering::SeqCst);
    assert!(max_active >= 4, "only {max_active} parallel connections");
    assert!(elapsed < Duration::from_secs(3), "took {elapsed:?}");
    assert!(engine.drain_events().iter().any(|e| matches!(e, EngineEvent::Completed(i) if *i == id)));
}

#[test]
fn backs_off_when_the_server_limits_connections() {
    let rt = server_runtime();
    let server = rt.block_on(TestServer::start(ServerConfig {
        size: 4 << 20,
        throttle: 2 << 20,
        max_concurrent: 2,
        ..Default::default()
    }));
    let dir = tempfile::tempdir().unwrap();
    let engine = open_engine(dir.path(), |s| s.max_connections = 8);
    let id = engine.add(request(&server.url, true)).unwrap();
    let snap = wait_finished(&engine, id);
    assert_eq!(snap.status, DownloadStatus::Completed, "{:?}", snap.error);
    assert_eq!(read(snap.path()), *server.body);
    // Each 429 closes a connection, so the engine settles at the server's limit
    // instead of hammering it.
    let throttled = server.stats.throttled.load(Ordering::SeqCst);
    assert!(throttled <= 8, "server had to refuse {throttled} requests");
}

#[test]
fn server_without_ranges_downloads_on_one_connection() {
    let rt = server_runtime();
    let server = rt.block_on(TestServer::start(ServerConfig {
        size: 3 << 20,
        ranges: false,
        ..Default::default()
    }));
    let dir = tempfile::tempdir().unwrap();
    let engine = open_engine(dir.path(), |_| {});
    let id = engine.add(request(&server.url, true)).unwrap();
    let snap = wait_finished(&engine, id);
    assert_eq!(snap.status, DownloadStatus::Completed, "{:?}", snap.error);
    assert_eq!(snap.resumable, Some(false));
    assert_eq!(read(snap.path()), *server.body);
    assert_eq!(server.stats.requests.load(Ordering::SeqCst), 1);
}

#[test]
fn unknown_length_stream_completes() {
    let rt = server_runtime();
    let server = rt.block_on(TestServer::start(ServerConfig {
        size: 1 << 20,
        ranges: false,
        content_length: false,
        ..Default::default()
    }));
    let dir = tempfile::tempdir().unwrap();
    let engine = open_engine(dir.path(), |_| {});
    let id = engine.add(request(&server.url, true)).unwrap();
    let snap = wait_finished(&engine, id);
    assert_eq!(snap.status, DownloadStatus::Completed, "{:?}", snap.error);
    assert_eq!(snap.size, Some(1 << 20));
    assert_eq!(read(snap.path()), *server.body);
}

#[test]
fn recovers_from_dropped_connections() {
    let rt = server_runtime();
    let server = rt.block_on(TestServer::start(ServerConfig {
        size: 4 << 20,
        fail_first: 3,
        ..Default::default()
    }));
    let dir = tempfile::tempdir().unwrap();
    let engine = open_engine(dir.path(), |s| s.max_connections = 4);
    let id = engine.add(request(&server.url, true)).unwrap();
    let snap = wait_finished(&engine, id);
    assert_eq!(snap.status, DownloadStatus::Completed, "{:?}", snap.error);
    assert_eq!(read(snap.path()), *server.body);
}

#[test]
fn pause_and_resume_after_restart() {
    let rt = server_runtime();
    let server = rt.block_on(TestServer::start(ServerConfig {
        size: 6 << 20,
        throttle: 512 << 10,
        ..Default::default()
    }));
    let dir = tempfile::tempdir().unwrap();

    let (id, paused_at) = {
        let engine = open_engine(dir.path(), |s| s.max_connections = 4);
        let id = engine.add(request(&server.url, true)).unwrap();
        wait_for(&engine, id, Duration::from_secs(20), |s| s.downloaded > 1 << 20);
        engine.pause(id);
        let snap = wait_for(&engine, id, Duration::from_secs(5), |s| s.status == DownloadStatus::Stopped);
        assert!(snap.downloaded < 6 << 20);
        let details = engine.details(id);
        assert!(details.segments.len() >= 4, "expected splits, got {:?}", details.segments);
        engine.shutdown();
        (id, snap.downloaded)
    };

    // A fresh engine (as after relaunching the app) picks up where we left off.
    server.set_throttle(0);
    let requests_before = server.stats.range_starts.lock().len();
    let engine = open_engine(dir.path(), |s| s.max_connections = 4);
    let snap = engine.get(id).unwrap();
    assert_eq!(snap.status, DownloadStatus::Stopped);
    assert!(snap.downloaded >= paused_at);
    engine.start(id).unwrap();
    let snap = wait_finished(&engine, id);
    assert_eq!(snap.status, DownloadStatus::Completed, "{:?}", snap.error);
    assert_eq!(read(snap.path()), *server.body);
    // Every request after the restart continued somewhere inside the file.
    let resumed: Vec<u64> = server.stats.range_starts.lock()[requests_before..].to_vec();
    assert!(!resumed.is_empty());
    assert!(resumed.iter().all(|&s| s > 0), "restarted from zero: {resumed:?}");
}

#[test]
fn changed_file_restarts_from_scratch() {
    let rt = server_runtime();
    let server = rt.block_on(TestServer::start(ServerConfig {
        size: 4 << 20,
        throttle: 512 << 10,
        ..Default::default()
    }));
    let dir = tempfile::tempdir().unwrap();
    let engine = open_engine(dir.path(), |s| s.max_connections = 2);
    let id = engine.add(request(&server.url, true)).unwrap();
    wait_for(&engine, id, Duration::from_secs(20), |s| s.downloaded > 512 << 10);
    engine.pause(id);
    wait_for(&engine, id, Duration::from_secs(5), |s| s.status == DownloadStatus::Stopped);

    server.set_etag("\"v2\"");
    server.set_throttle(0);
    engine.start(id).unwrap();
    let snap = wait_finished(&engine, id);
    assert_eq!(snap.status, DownloadStatus::Completed, "{:?}", snap.error);
    assert_eq!(read(snap.path()), *server.body);
}

#[test]
fn http_errors_fail_the_download() {
    let rt = server_runtime();
    let server = rt.block_on(TestServer::start(ServerConfig {
        status_override: Some(404),
        ..Default::default()
    }));
    let dir = tempfile::tempdir().unwrap();
    let engine = open_engine(dir.path(), |_| {});
    let id = engine.add(request(&server.url, true)).unwrap();
    let snap = wait_finished(&engine, id);
    assert_eq!(snap.status, DownloadStatus::Failed);
    assert!(snap.error.unwrap().contains("404"));
}

#[test]
fn speed_limit_is_respected() {
    let rt = server_runtime();
    let server = rt.block_on(TestServer::start(ServerConfig {
        size: 1 << 20,
        ..Default::default()
    }));
    let dir = tempfile::tempdir().unwrap();
    let engine = open_engine(dir.path(), |s| s.global_speed_limit = 512 << 10);
    let started = std::time::Instant::now();
    let id = engine.add(request(&server.url, true)).unwrap();
    let snap = wait_finished(&engine, id);
    assert_eq!(snap.status, DownloadStatus::Completed, "{:?}", snap.error);
    let elapsed = started.elapsed().as_secs_f64();
    assert!(elapsed > 1.5, "1 MiB at 512 KiB/s finished in {elapsed:.2}s");
}

#[test]
fn queue_runs_downloads_in_order_with_concurrency_limit() {
    let rt = server_runtime();
    let server = rt.block_on(TestServer::start(ServerConfig {
        size: 512 << 10,
        throttle: 1 << 20,
        ..Default::default()
    }));
    let dir = tempfile::tempdir().unwrap();
    let engine = open_engine(dir.path(), |s| {
        s.queue.max_concurrent = 1;
        s.max_connections = 1;
    });
    let ids: Vec<_> = (0..3)
        .map(|_| {
            engine
                .add(AddRequest {
                    queue: true,
                    ..request(&server.url, false)
                })
                .unwrap()
        })
        .collect();
    assert!(engine.snapshot().iter().all(|s| s.status == DownloadStatus::Stopped));
    engine.start_queue();
    for &id in &ids {
        let snap = wait_finished(&engine, id);
        assert_eq!(snap.status, DownloadStatus::Completed, "{:?}", snap.error);
        assert!(!snap.in_queue);
    }
    assert_eq!(server.stats.max_active.load(Ordering::SeqCst), 1);
    let names: std::collections::HashSet<_> = engine.snapshot().into_iter().map(|s| s.file_name).collect();
    assert_eq!(names.len(), 3, "duplicate names must be numbered: {names:?}");
    std::thread::sleep(Duration::from_millis(1500));
    assert!(!engine.is_queue_running());
    assert!(engine.drain_events().iter().any(|e| matches!(e, EngineEvent::QueueFinished { .. })));
}

#[test]
fn removing_a_running_download_deletes_its_partial_file() {
    let rt = server_runtime();
    let server = rt.block_on(TestServer::start(ServerConfig {
        size: 4 << 20,
        throttle: 256 << 10,
        ..Default::default()
    }));
    let dir = tempfile::tempdir().unwrap();
    let engine = open_engine(dir.path(), |_| {});
    let id = engine.add(request(&server.url, true)).unwrap();
    let snap = wait_for(&engine, id, Duration::from_secs(10), |s| s.downloaded > 0);
    let part = snap.directory.join(format!("{}{PART_SUFFIX}", snap.file_name));
    assert!(part.exists());
    engine.remove(&[id], true);
    assert!(engine.get(id).is_none());
    for _ in 0..100 {
        if !part.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!part.exists());
}
