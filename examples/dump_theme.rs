//! Print the theme derived from assets/cover.jpg as hex, so we can inspect the
//! reactive derivation without opening a TUI.
use myx::gradient::Rgb;
use myx::reactive::derive_theme;

fn hex(c: Rgb) -> String { format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b) }

fn main() {
    let img = image::open("assets/cover.jpg").expect("cover");
    let t = derive_theme(&img, "album");
    println!("derived theme '{}' from M83 cover:", t.name);
    for (k, c) in [
        ("background      ", t.background),
        ("background_panel", t.background_panel),
        ("background_elem ", t.background_element),
        ("text            ", t.text),
        ("text_muted      ", t.text_muted),
        ("primary         ", t.primary),
        ("secondary       ", t.secondary),
        ("accent          ", t.accent),
        ("error           ", t.error),
        ("warning         ", t.warning),
        ("success         ", t.success),
        ("border          ", t.border),
        ("border_active   ", t.border_active),
    ] {
        println!("  {k} {}", hex(c));
    }
}
