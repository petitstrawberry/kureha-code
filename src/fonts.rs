//! Editor-only fonts. Never replace ScarletUI's global UI font stack.
use scarlet_ui::FontStack;
use std::sync::OnceLock;

/// Cached once per process so clones retain the same typography cache identity.
/// Hosts embed the licensed assets, making a moved executable self-contained.
/// Scarlet uses the desktop's shared font files instead of inflating the app.
pub fn editor_stack() -> Option<FontStack> {
    static STACK: OnceLock<Option<FontStack>> = OnceLock::new();
    STACK.get_or_init(load).clone()
}

#[cfg(not(target_os = "scarlet"))]
fn load() -> Option<FontStack> {
    FontStack::new(include_bytes!(
        "../assets/fonts/JetBrainsMonoNerdFontMono-Regular.ttf"
    ))
    .ok()?
    .with_fallback(include_bytes!("../assets/fonts/Mplus1-Regular.ttf"))
    .ok()?
    .with_fallback(include_bytes!("../assets/fonts/NotoSansCJKjp-Regular.otf"))
    .ok()
}

#[cfg(target_os = "scarlet")]
fn load() -> Option<FontStack> {
    // FontStack requires process-lifetime bytes. Each successful allocation is
    // retained once by this OnceLock, never once per tab or widget rebuild.
    let paths = [
        "/share/fonts/JetBrainsMonoNerdFontMono-Regular.ttf",
        "/share/fonts/Mplus1-Regular.ttf",
        "/share/fonts/NotoSansCJKjp-Regular.otf",
    ];
    let mut stack: Option<FontStack> = None;
    for path in paths {
        if let Ok(bytes) = std::fs::read(path) {
            let bytes: &'static [u8] = Box::leak(bytes.into_boxed_slice());
            if let Some(stack) = stack.as_mut() {
                let _ = stack.add_fallback(bytes);
            } else {
                stack = FontStack::new(bytes).ok();
            }
        }
    }
    stack
}

#[cfg(all(test, not(target_os = "scarlet")))]
mod tests {
    use super::*;
    #[test]
    fn bundled_editor_stack_parses_and_reuses_its_cache_identity() {
        let first = editor_stack().expect("bundled fonts must parse");
        let second = editor_stack().unwrap();
        assert_eq!(first.cache_id(), second.cache_id());
        let latin = scarlet_ui::graphics::measure_text_sized_with_font_stack("iiii", 14.0, &first);
        let wide = scarlet_ui::graphics::measure_text_sized_with_font_stack("WWWW", 14.0, &first);
        assert_eq!(latin.0, wide.0, "editor primary must be monospaced");
    }
}
