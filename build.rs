fn main() {
    // Windows exe icon (ignored on other targets).
    #[cfg(target_os = "windows")]
    {
        let mut res = winres::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("ProductName", "eduway");
        res.set("FileDescription", "eduway - download your Edunex courses");
        if let Err(e) = res.compile() {
            eprintln!("winres failed: {e}");
        }
    }
}
