#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod cache;
mod cleaner;
mod fmt;
mod index;
mod mft;
mod models;
mod projects;
mod results_cache;
mod rules;
mod scan;
mod settings;
mod tree;
mod treemap;
mod ui_analyze;
mod ui_clean;
mod win;

use std::sync::atomic::AtomicBool;

/// 命令列測試模式：--bench <路徑> [cache]
fn bench(path: &str, prefer_cache: bool) {
    let prog = scan::Progress::default();
    let t = std::time::Instant::now();
    match index::run(std::path::Path::new(path), &prog, prefer_cache) {
        Ok(mut out) => {
            if let Some(h) = out.saver.take() {
                let ts = std::time::Instant::now();
                let _ = h.join();
                println!("（寫入快取 {:.2}s）", ts.elapsed().as_secs_f32());
            }
            let t = &out.tree;
            let v = &t.nodes[out.view as usize];
            println!(
                "{} | {} | nodes={} dirs={} files={} size={} | {:.2}s | errors={}",
                out.target.display(),
                out.origin.describe(),
                t.nodes.len(),
                t.stats.dirs,
                v.files,
                fmt::size(v.size),
                out.secs,
                t.errors
            );
            for c in v.children().take(8) {
                println!("  {:>12}  {}", fmt::size(t.nodes[c as usize].size), t.name(c));
            }
        }
        Err(e) => println!("錯誤：{e}"),
    }
    println!("總耗時 {:.2}s；管理員={}", t.elapsed().as_secs_f32(), win::is_elevated());
}

/// --rules：列出每條清理規則可釋放的大小（不刪除）
fn bench_rules() {
    let t = std::time::Instant::now();
    let rules = rules::catalog();
    let cancel = AtomicBool::new(false);
    let res = parking_lot::Mutex::new(Vec::new());
    cleaner::measure_all(&rules, &cancel, |i, m| res.lock().push((i, m)));
    let mut res = res.into_inner();
    res.sort_by(|a, b| b.1.bytes.cmp(&a.1.bytes));
    let mut total = 0;
    for (i, m) in &res {
        let r = &rules[*i];
        if m.bytes == 0 {
            continue;
        }
        if !matches!(r.action, rules::Action::Command { .. }) {
            total += m.bytes;
        }
        println!("{:>10}  {:?}  {}{}", fmt::size(m.bytes), r.risk, r.name, if m.errors > 0 { format!("（{} 個無法讀取）", m.errors) } else { String::new() });
    }
    println!("合計 {}，耗時 {:.2}s", fmt::size(total), t.elapsed().as_secs_f32());
}

/// --projects <路徑>...：搜尋專案建置產物（不刪除）
fn bench_projects(roots: &[String]) {
    let t = std::time::Instant::now();
    let prog = projects::FindProgress::default();
    let roots: Vec<_> = roots.iter().map(std::path::PathBuf::from).collect();
    projects::find(&roots, &prog);
    let mut res = prog.results.into_inner();
    res.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    let total: u64 = res.iter().map(|a| a.bytes).sum();
    for a in res.iter().take(40) {
        println!("{:>10}  {:<36} {}  {}", fmt::size(a.bytes), a.kind.label, fmt::ago(a.newest), a.path.display());
    }
    let mut ms = prog.models.into_inner();
    models::dedup(&mut ms);
    ms.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    let mtotal: u64 = ms.iter().map(|m| m.bytes).sum();
    println!("--- AI 模型 ---");
    for m in ms.iter().take(40) {
        println!("{:>10}  {:<16} {:<10} {}  ({})", fmt::size(m.bytes), m.source, fmt::ago(m.newest), m.name, m.path.display());
    }
    println!(
        "產物 {} 個 {}；模型 {} 個 {}；搜尋 {} 個資料夾，耗時 {:.2}s",
        res.len(),
        fmt::size(total),
        ms.len(),
        fmt::size(mtotal),
        prog.dirs.into_inner(),
        t.elapsed().as_secs_f32()
    );
}

/// --selftest：在暫存資料夾建立測試環境，驗證偵測與刪除的安全性。
fn selftest() -> bool {
    use std::fs;
    use std::path::Path;
    let base = std::env::temp_dir().join("dc_selftest");
    let _ = fs::remove_dir_all(&base);
    let mut ok = true;
    let mut check = |name: &str, cond: bool| {
        println!("{} {name}", if cond { "PASS" } else { "FAIL" });
        ok &= cond;
    };
    let w = |p: &Path, s: &str| {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, s).unwrap();
    };
    // 連結目標：絕對不能被刪
    let keep = base.join("keep");
    w(&keep.join("important.txt"), "do not delete");
    // 專案
    let proj = base.join("proj");
    w(&proj.join("package.json"), "{}");
    w(&proj.join("package-lock.json"), "{}");
    let nm = proj.join("node_modules");
    w(&nm.join("a").join("index.js"), &"x".repeat(5000));
    w(&nm.join(".bin").join("tool.cmd"), "echo");
    let ro = nm.join("a").join("readonly.js");
    w(&ro, "ro");
    let mut perm = fs::metadata(&ro).unwrap().permissions();
    perm.set_readonly(true);
    fs::set_permissions(&ro, perm).unwrap();
    let st = std::process::Command::new("cmd")
        .args(["/c", "mklink", "/J"])
        .arg(nm.join("link"))
        .arg(&keep)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    check("建立 junction", st);
    // 沒有 lock 檔的 node_modules → 注意
    let p2 = base.join("nolock");
    w(&p2.join("package.json"), "{}");
    w(&p2.join("node_modules").join("x.js"), "x");
    // 沒有 package.json 的 node_modules → 不列出
    w(&base.join("loose").join("node_modules").join("y.js"), "y");
    // Rust target
    w(&base.join("rs").join("Cargo.toml"), "[package]");
    w(&base.join("rs").join("target").join("debug").join("app.exe"), "bin");

    // 1. 偵測
    let prog = projects::FindProgress::default();
    projects::find(std::slice::from_ref(&base), &prog);
    let res = prog.results.into_inner();
    let find = |p: &Path| res.iter().find(|a| a.path == p);
    check("偵測 node_modules（有 lock → 安全）", find(&nm).is_some_and(|a| a.kind.id == "node"));
    check("node_modules 大小不含 junction 目標", find(&nm).is_some_and(|a| a.bytes == 5000 + 4 + 2));
    check("無 lock 檔 → 注意", find(&p2.join("node_modules")).is_some_and(|a| a.kind.id == "node_nolock"));
    check("沒有 package.json 不列出", find(&base.join("loose").join("node_modules")).is_none());
    check("偵測 Rust target", find(&base.join("rs").join("target")).is_some_and(|a| a.kind.id == "rust"));
    check("不會把 keep 當成產物", !res.iter().any(|a| a.path.starts_with(&keep)));

    // 2. 刪除
    let dp = cleaner::DelProgress::default();
    let deleted = cleaner::delete_dir(&nm, &dp);
    check("node_modules 已刪除（含唯讀檔）", deleted && !nm.exists());
    check("junction 目標完好", keep.join("important.txt").exists());
    check("釋放大小正確", dp.freed.load(std::sync::atomic::Ordering::Relaxed) == 5000 + 4 + 2);

    // 3. 保護清單
    let home = std::env::var("USERPROFILE").unwrap_or_default();
    check("拒絕磁碟根目錄", cleaner::is_protected(Path::new("C:\\")));
    check("拒絕使用者資料夾", cleaner::is_protected(Path::new(&home)));
    check("拒絕 Windows", cleaner::is_protected(Path::new("C:\\Windows")));
    check("拒絕 Documents", cleaner::is_protected(&Path::new(&home).join("Documents")));
    check("允許一般專案資料夾", !cleaner::is_protected(&proj));
    let dp2 = cleaner::DelProgress::default();
    check("delete_dir 拒絕受保護位置", !cleaner::delete_dir(Path::new("C:\\Windows"), &dp2));

    // 4. 依時間保留新檔
    let td = base.join("tempdir");
    w(&td.join("old.tmp"), "old");
    w(&td.join("new.tmp"), "new");
    let f = fs::File::options().write(true).open(td.join("old.tmp")).unwrap();
    f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3 * 86400)).unwrap();
    drop(f);
    let dp3 = cleaner::DelProgress::default();
    cleaner::delete_children(&td, cleaner::cutoff(1), &dp3);
    check("只刪 1 天前的檔案", !td.join("old.tmp").exists() && td.join("new.tmp").exists() && td.exists());

    // 5. Ollama：共用的 blob 不能刪
    let om = base.join("ollama");
    let man = |digests: &[(&str, u64)]| {
        let layers: Vec<String> = digests
            .iter()
            .map(|(d, s)| format!("{{\"mediaType\":\"application/vnd.ollama.image.model\",\"digest\":\"{d}\",\"size\":{s}}}"))
            .collect();
        format!("{{\"schemaVersion\":2,\"layers\":[{}]}}", layers.join(","))
    };
    let lib = om.join("manifests").join("registry.ollama.ai").join("library");
    w(&lib.join("alpha").join("7b"), &man(&[("sha256:shared", 6), ("sha256:onlya", 5)]));
    w(&lib.join("beta").join("latest"), &man(&[("sha256:shared", 6), ("sha256:onlyb", 5)]));
    w(&om.join("blobs").join("sha256-shared"), "shared");
    w(&om.join("blobs").join("sha256-onlya"), "aaaaa");
    w(&om.join("blobs").join("sha256-onlyb"), "bbbbb");
    // SAFETY: 自我測試在單執行緒階段設定環境變數
    unsafe { std::env::set_var("OLLAMA_MODELS", &om) };
    let mut found = Vec::new();
    let cancel = AtomicBool::new(false);
    for m in models::find_known(&cancel) {
        if m.source == "Ollama" {
            found.push(m);
        }
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    check("讀出 Ollama 模型名稱", found.iter().map(|m| m.name.as_str()).collect::<Vec<_>>() == ["alpha:7b", "beta:latest"]);
    check("Ollama 模型大小", found.first().is_some_and(|m| m.bytes == 11));
    let freed = models::delete_ollama(&lib.join("alpha").join("7b"));
    check("刪除 alpha 只釋放獨有的 blob", freed == Ok(5));
    check("共用 blob 保留", om.join("blobs").join("sha256-shared").exists() && om.join("blobs").join("sha256-onlyb").exists());
    check("獨有 blob 已刪", !om.join("blobs").join("sha256-onlya").exists() && !lib.join("alpha").join("7b").exists());
    check("模型副檔名判斷", models::is_model_file("x.SafeTensors") && models::is_model_file("a.gguf") && !models::is_model_file("a.bin"));

    // 6. 任何名稱的 venv（含 CACHEDIR.TAG）都算虛擬環境
    let vroot = base.join("venvs");
    w(&vroot.join("see-through").join("pyvenv.cfg"), "home = x");
    w(&vroot.join("see-through").join("CACHEDIR.TAG"), "Signature: 8a477f597d28d172789f06886806bc55");
    w(&vroot.join("see-through").join("lib.py"), "x");
    let prog = projects::FindProgress::default();
    projects::find(std::slice::from_ref(&vroot), &prog);
    check("uv venv 判定為虛擬環境（注意）", prog.results.into_inner().iter().any(|a| a.kind.id == "venv"));

    // 7. 萬用字元
    check("萬用字元比對", rules::wildcard("thumbcache_*.db", "ThumbCache_1024.DB") && !rules::wildcard("*.exe", "a.exe.txt"));

    let _ = fs::remove_dir_all(&base);
    println!("{}", if ok { "全部通過" } else { "有測試失敗" });
    ok
}

fn icon() -> eframe::egui::IconData {
    // 由 examples/gen_icon.rs 產生的 128×128 RGBA
    const RGBA: &[u8] = include_bytes!("../assets/icon-128.rgba");
    eframe::egui::IconData { rgba: RGBA.to_vec(), width: 128, height: 128 }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("--bench") if args.len() >= 3 => {
            bench(&args[2], args.get(3).is_some_and(|s| s == "cache"));
            return;
        }
        Some("--selftest") => {
            std::process::exit(if selftest() { 0 } else { 1 });
        }
        Some("--rules") => {
            bench_rules();
            return;
        }
        Some("--projects") if args.len() >= 3 => {
            bench_projects(&args[2..]);
            return;
        }
        _ => {}
    }
    // --analyze <路徑> 或直接給路徑：啟動後馬上分析
    let analyze = match args.get(1).map(|s| s.as_str()) {
        Some("--analyze") => args.get(2).map(std::path::PathBuf::from),
        Some(p) if !p.starts_with("--") => Some(std::path::PathBuf::from(p)),
        _ => None,
    };

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("DiskCleaner — 磁碟分析與清理")
            .with_inner_size([1360.0, 860.0])
            .with_min_inner_size([960.0, 600.0])
            .with_drag_and_drop(true)
            .with_icon(std::sync::Arc::new(icon())),
        ..Default::default()
    };
    let _ = eframe::run_native("DiskCleaner", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, analyze)))));
}
