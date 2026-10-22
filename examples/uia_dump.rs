//! Prints every UI Automation descendant of the taskbar (class, id, rect); with `bench`,
//! measures the per-poll cost of the two query styles instead.
fn main() {
    if std::env::args().nth(1).as_deref() == Some("bench") {
        print!("{}", winpager::taskbar_space::bench());
        return;
    }
    let t = std::time::Instant::now();
    print!("{}", winpager::taskbar_space::dump());
    eprintln!("took {:?}", t.elapsed());
}
