//! Test-Fehlertyp dieses Crates (siehe `harw-test-support`): ersetzt
//! `panic!`/`unwrap`/`expect` in Tests (Rust Coding Bible R087/R165/R182).

harw_test_support::define_test_error!(pub(crate));

/// Installs the host command port once per test binary (the `!` path runs
/// only through the job runtime).
pub(crate) fn install_host_port() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let dir = harw_test_support::unique_tmp("harw-tui", "command-jobs");
        if let Ok(port) = harw_command::JobCommandPort::host(&dir) {
            let _ = harw_command::install(std::sync::Arc::new(port));
        }
    });
}

/// Visible buffer text, skipping continuation cells of wide graphemes.
pub(crate) fn display_buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
    let area = buffer.area();
    let mut text = String::new();
    for y in area.top()..area.bottom() {
        let mut x = area.left();
        while x < area.right() {
            let symbol = buffer[(x, y)].symbol();
            text.push_str(symbol);
            let width = unicode_width::UnicodeWidthStr::width(symbol).max(1);
            x = x.saturating_add(u16::try_from(width).unwrap_or(u16::MAX));
        }
        text.push('\n');
    }
    text
}
