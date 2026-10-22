//! Test aid: print the desktop count and current index; with an index argument, switch to it.
fn main() -> anyhow::Result<()> {
    if let Some(i) = std::env::args().nth(1).and_then(|s| s.parse::<usize>().ok()) {
        winpager::vd::switch_to(i)?;
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    let (d, cur) = winpager::vd::desktops()?;
    println!("count {} current {}", d.len(), cur);
    Ok(())
}
