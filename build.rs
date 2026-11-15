// 把圖示與版本資訊嵌進 exe（檔案總管、工作列會顯示）。
// Embeds the icon and version info into the exe (shown in Explorer and the taskbar).
fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/icon.ico");
    res.set("ProductName", "DiskCleaner");
    res.set("FileDescription", "DiskCleaner - disk analyzer and cleaner");
    res.set("LegalCopyright", "Copyright (c) 2026 ailazytech. MIT License.");
    if let Err(e) = res.compile() {
        // 沒有 Windows SDK（rc.exe）時仍可編譯，只是 exe 沒有圖示
        println!("cargo:warning=could not embed icon resource: {e}");
    }
}
