//! 固定种子合成语料；只写指定的专用输出目录，不扫描真实笔记。
use notebase_core::index::{
    worker::{BackgroundIndex, WorkerOptions},
    IndexSession,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
const ID: &str = "0123456789abcdef";
const SEED: u64 = 20261029;
fn note(i: usize) -> String {
    let topics = [
        "数据结构",
        "动态规划",
        "图论算法",
        "操作系统",
        "编译原理",
        "机器学习",
        "分布式存储",
        "计算几何",
        "软件测试",
        "网络协议",
    ];
    let mut text = format!(
        "---\ntitle: 课程笔记{i:06}\ntags: [课程{}]\naliases: [别称{i:06}]\n---\n# {}\n",
        i % 10,
        topics[i % 10]
    );
    let mut state = SEED ^ (i as u64).wrapping_mul(6364136223846793005);
    while text.len() < 4900 {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        text.push_str(&format!(
            "段落{:08x}：概念与例题分析，边界条件、实现细节以及复杂度证明。\n",
            state as u32
        ));
    }
    if i.is_multiple_of(1000) {
        text.push_str("\n跨词边界子串测验 特有两字桥接词：桥接\n");
    }
    if i < 100 {
        text.push_str(&format!("\n植入标记词{i:04}终点\n"));
    }
    text
}
fn path(i: usize) -> String {
    format!("分组{:03}/笔记{i:06}.md", i / 1000)
}
fn expected(query: &str, count: usize) -> BTreeSet<String> {
    (0..count)
        .filter(|&i| match query {
            "态规划" => i % 10 == 1,
            "图论算法" => i % 10 == 2,
            "跨词边界" => i.is_multiple_of(1000),
            "桥接" => i.is_multiple_of(1000),
            "规划" => i % 10 == 1,
            "算法" => i % 10 == 2,
            _ => false,
        })
        .map(path)
        .collect()
}
fn wait_ready(
    worker: &BackgroundIndex,
    fatal: &Mutex<Option<Value>>,
    ready: &AtomicUsize,
    after: usize,
) {
    let start = Instant::now();
    let mut last = Instant::now();
    loop {
        assert!(
            fatal.lock().unwrap().is_none(),
            "benchmark stopped after backend failure; see failure.json"
        );
        let status = worker.status();
        if ready.load(Ordering::Acquire) > after {
            break;
        }
        if last.elapsed() > Duration::from_secs(15) {
            eprintln!("index status {}", serde_json::to_string(&status).unwrap());
            last = Instant::now();
        }
        assert!(
            start.elapsed() < Duration::from_secs(3600),
            "rebuild timed out"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}
fn query(worker: &BackgroundIndex, text: &str, limit: usize, cursor: Option<&str>) -> Value {
    worker
        .search(&json!({"query":text,"limit":limit,"cursor":cursor}))
        .unwrap_or_else(|e| panic!("query {text}: {} {}", e.code, e.message))
}
fn verify(worker: &BackgroundIndex, text: &str, expected: &BTreeSet<String>) {
    let mut cursor = None;
    let mut actual = BTreeSet::new();
    loop {
        let page = query(worker, text, 200, cursor.as_deref());
        assert_eq!(
            page["totalApprox"].as_u64().unwrap() as usize,
            expected.len(),
            "count {text}"
        );
        for item in page["items"].as_array().unwrap() {
            assert!(
                actual.insert(item["path"].as_str().unwrap().to_owned()),
                "duplicate {text}"
            );
        }
        cursor = page["nextCursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(&actual, expected, "recall {text}");
}
fn size(root: &Path) -> u64 {
    walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum()
}
fn run_queries(
    worker: &BackgroundIndex,
    root: &Path,
    count: usize,
    repetitions: usize,
    startup_ms: u128,
) {
    let mut metrics = Vec::new();
    for text in ["态规划", "图论算法", "跨词边界", "桥接", "规划", "算法"] {
        let first = Instant::now();
        query(worker, text, 50, None);
        let first_ms = first.elapsed().as_secs_f64() * 1000.0;
        verify(worker, text, &expected(text, count));
        let mut times = Vec::new();
        for _ in 0..repetitions {
            let start = Instant::now();
            query(worker, text, 50, None);
            times.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        let mut sorted = times.clone();
        sorted.sort_by(f64::total_cmp);
        let p95 = sorted[(sorted.len() * 95).div_ceil(100).saturating_sub(1)];
        eprintln!("query {text}: p95 {p95:.3}ms");
        metrics.push(json!({"query":text,"truthCount":expected(text,count).len(),"firstObservedMs":first_ms,"latencyMs":times,"p95Ms":p95,"fullSetVerified":true}));
    }
    for i in 0..100.min(count) {
        let term = format!("植入标记词{i:04}终点");
        verify(worker, &term, &BTreeSet::from([path(i)]));
    }
    let result = json!({"seed":SEED,"notes":count,"repetitions":repetitions,"startupMs":startup_ms,"queries":metrics,"markerWordsVerified":100.min(count),"cacheMode":"first observed before truth verification, then warm; OS cache not flushed"});
    fs::write(
        root.join("query.json"),
        serde_json::to_vec_pretty(&result).unwrap(),
    )
    .unwrap();
    println!("{result}");
}
fn run_watch(
    worker: &BackgroundIndex,
    root: &Path,
    count: usize,
    repetitions: usize,
    fatal: &Mutex<Option<Value>>,
) {
    let vault = root.join("vault");
    let mut times = Vec::new();
    for n in 0..repetitions {
        let name = vault.join("增量验收.md");
        let term = format!("增量标记{n:06}");
        let start = Instant::now();
        fs::write(&name, &term).unwrap();
        loop {
            if let Ok(page) = worker.search(&json!({"query":term})) {
                if page["totalApprox"] == 1 {
                    break;
                }
            }
            assert!(start.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(5));
        }
        times.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    let result = json!({"notes":count,"latencyMs":times});
    fs::write(
        root.join("watch.json"),
        serde_json::to_vec_pretty(&result).unwrap(),
    )
    .unwrap();
    println!("{result}");
    fs::remove_file(vault.join("增量验收.md")).unwrap();

    let removed = format!("增量标记{:06}", repetitions - 1);
    let deadline = Instant::now();
    loop {
        assert!(fatal.lock().unwrap().is_none(), "watch backend failed");
        if let Ok(page) = worker.search(&json!({"query":removed})) {
            if page["totalApprox"] == 0
                && worker.status().state == notebase_core::index::IndexState::Ready
            {
                break;
            }
        }
        assert!(deadline.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(5));
    }
    std::thread::sleep(Duration::from_millis(600));
}
fn run_concurrent(worker: &BackgroundIndex, root: &Path, count: usize, repetitions: usize) {
    std::thread::scope(|scope| {
        let jobs: Vec<_> = (0..4)
            .map(|_| {
                scope.spawn(|| {
                    let mut times = Vec::new();
                    for _ in 0..repetitions {
                        let start = Instant::now();
                        let page = query(worker, "跨词边界", 50, None);
                        assert_eq!(page["totalApprox"], expected("跨词边界", count).len());
                        times.push(start.elapsed().as_secs_f64() * 1000.0);
                    }
                    times
                })
            })
            .collect();
        let samples: Vec<_> = jobs.into_iter().map(|j| j.join().unwrap()).collect();
        let result = json!({"notes":count,"clients":4,"repetitionsPerClient":repetitions,"latencyMs":samples});
        fs::write(
            root.join("concurrent.json"),
            serde_json::to_vec_pretty(&result).unwrap(),
        )
        .unwrap();
        println!("{result}");
    });
}
fn main() {
    let args: Vec<_> = std::env::args().collect();
    assert!(
        args.len() >= 5,
        "perf <generate|build|rebuild|query|watch|concurrent|accept|steady> <absolute-root> <count> <repetitions>"
    );
    let mode = &args[1];
    assert!([
        "generate",
        "build",
        "rebuild",
        "query",
        "watch",
        "concurrent",
        "accept",
        "steady"
    ]
    .contains(&mode.as_str()));
    let root = PathBuf::from(&args[2]);
    assert!(root.is_absolute());
    let count: usize = args[3].parse().unwrap();
    let repetitions: usize = args[4].parse().unwrap();
    assert!(count > 0 && count <= 200000 && repetitions > 0);
    let vault = root.join("vault");
    let data = root.join("data");
    fs::create_dir_all(&root).unwrap();
    if mode == "generate" {
        assert!(!vault.exists(), "refuse to overwrite existing corpus");
        fs::create_dir(&vault).unwrap();
        let start = Instant::now();
        for i in 0..count {
            let file = vault.join(path(i));
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, note(i)).unwrap();
        }
        let result = json!({"seed":SEED,"notes":count,"bytes":size(&vault),"generationMs":start.elapsed().as_millis()});
        fs::write(
            root.join("corpus.json"),
            serde_json::to_vec_pretty(&result).unwrap(),
        )
        .unwrap();
        println!("{result}");
        return;
    }
    assert!(vault.is_dir());
    let corpus: Value = serde_json::from_slice(
        &fs::read(root.join("corpus.json")).expect("requires generated corpus manifest"),
    )
    .unwrap();
    assert_eq!(corpus["seed"], SEED);
    assert_eq!(corpus["notes"], count);

    let existed = data.join("vaults").join(ID).join("index.db").exists();
    let started = Instant::now();
    let fatal = Arc::new(Mutex::new(None));
    let sink_fatal = fatal.clone();
    let ready = Arc::new(AtomicUsize::new(0));
    let sink_ready = ready.clone();
    let ready_times = Arc::new(Mutex::new(Vec::<u128>::new()));
    let sink_times = ready_times.clone();
    let failure_path = root.join("failure.json");
    let native_watch = mode == "watch" || mode == "accept" || mode == "steady";
    let events = Arc::new(move |name: &str, payload: Value| {
        if name == "index.state" && payload["state"] == "ready" {
            sink_times
                .lock()
                .unwrap()
                .push(started.elapsed().as_millis());
            sink_ready.fetch_add(1, Ordering::Release);
        }
        if name == "error.raised" {
            eprintln!("{name}: {payload}");
            if native_watch || !payload["detail"]["pollIntervalMs"].is_number() {
                let _ = fs::write(&failure_path, serde_json::to_vec_pretty(&payload).unwrap());
                *sink_fatal.lock().unwrap() = Some(payload);
            }
        }
    });
    let worker = BackgroundIndex::start_with_options(
        data.clone(),
        vault.to_string_lossy().into_owned(),
        ID.into(),
        events,
        WorkerOptions {
            native_watch,
            reconcile_interval: Duration::from_secs(86400),
            ..WorkerOptions::default()
        },
    );
    wait_ready(&worker, &fatal, &ready, 0);
    let first_ready_ms = ready_times.lock().unwrap()[0];
    if mode == "build" {
        let mut result = json!({"notes":count,"startupBuildMs":first_ready_ms,"completion":"first index.state ready event","corpusBytes":size(&vault),"indexBytesLive":worker.status().index_bytes});
        worker.shutdown();
        result["dataBytesAfterClose"] = json!(size(&data));
        fs::write(
            root.join("build.json"),
            serde_json::to_vec_pretty(&result).unwrap(),
        )
        .unwrap();
        println!("{result}");
        return;
    }
    // 已有库的首个 ready 是打开旧索引；第二个 ready 才是启动对账结束。
    // 新库同样在发布后做一次启动对账。等待事件而非状态轮询，避免漏掉短暂的 ready。
    if existed || !native_watch {
        wait_ready(&worker, &fatal, &ready, 1);
    }
    let startup_ms = started.elapsed().as_millis();
    if mode == "query" || mode == "accept" || mode == "steady" {
        run_queries(&worker, &root, count, repetitions, startup_ms);
    }
    if mode == "watch" || mode == "accept" || mode == "steady" {
        run_watch(&worker, &root, count, repetitions, &fatal);
    }
    if mode == "concurrent" || mode == "accept" || mode == "steady" {
        run_concurrent(
            &worker,
            &root,
            count,
            if mode == "accept" || mode == "steady" {
                repetitions.div_ceil(4)
            } else {
                repetitions
            },
        );
    }
    let mut rebuilt = None;
    if mode == "rebuild" || (mode == "accept" && count == 100000) {
        let after = ready.load(Ordering::Acquire);
        let began = started.elapsed().as_millis();
        let job = worker.rebuild().unwrap();
        wait_ready(&worker, &fatal, &ready, after);
        let elapsed = ready_times.lock().unwrap()[after] - began;
        for text in ["态规划", "图论算法", "跨词边界", "桥接", "规划", "算法"] {
            verify(&worker, text, &expected(text, count));
        }
        for i in 0..100.min(count) {
            verify(
                &worker,
                &format!("植入标记词{i:04}终点"),
                &BTreeSet::from([path(i)]),
            );
        }
        rebuilt = Some(
            json!({"notes":count,"jobId":job,"rebuildMs":elapsed,"truthAfterRebuildVerified":true,"completion":"index.state ready after requested job","indexBytesLive":worker.status().index_bytes,"corpusBytes":size(&vault)}),
        );
    }
    let live_bytes = worker.status().index_bytes;
    worker.shutdown();
    if let Some(mut result) = rebuilt {
        result["dataBytesAfterClose"] = json!(size(&data));
        fs::write(
            root.join("rebuild.json"),
            serde_json::to_vec_pretty(&result).unwrap(),
        )
        .unwrap();
        println!("{result}");
    }
    if mode == "accept" || mode == "steady" {
        let result = json!({"notes":count,"openedExistingIndex":existed,"firstReadyMs":first_ready_ms,"startupSettledMs":startup_ms,"indexBytesLive":live_bytes,"dataBytesAfterClose":size(&data),"corpusBytes":size(&vault)});
        fs::write(
            root.join(format!("{mode}.json")),
            serde_json::to_vec_pretty(&result).unwrap(),
        )
        .unwrap();
        println!("{result}");
    }
}
