//! Test aid: create N virtual desktops at the END of the list, hold for S seconds, then remove
//! exactly those desktops again (never touching the ones that existed before).
//! Usage: desk_test <n> <hold_seconds>
fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let n: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(1);
    let hold: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(5);
    let (before, _) = winpager::vd::desktops()?;
    let base = before.len();
    println!("existing desktops: {base}");
    for _ in 0..n {
        winpager::vd::create()?;
    }
    let (mid, _) = winpager::vd::desktops()?;
    println!("after create: {}", mid.len());
    std::thread::sleep(std::time::Duration::from_secs(hold));
    // Remove only the desktops we created, from the end, verifying the count each time.
    for _ in 0..n {
        let (now, _) = winpager::vd::desktops()?;
        if now.len() <= base {
            break;
        }
        winpager::vd::remove(now.len() - 1)?;
    }
    let (after, _) = winpager::vd::desktops()?;
    println!("after remove: {}", after.len());
    Ok(())
}
