#[path = "../src/fs_entry.rs"] mod fs_entry;
#[path = "../src/search.rs"] mod search;
#[path = "../src/tab.rs"] mod tab;
use fs_entry::FsEntry;
fn owned_bytes(entries: &Vec<FsEntry>) -> usize {
    entries.capacity() * std::mem::size_of::<FsEntry>()
        + entries.iter().map(|e| e.name.capacity() + e.path.capacity()).sum::<usize>()
}
fn main() {
    let mut tab = tab::Tab::new(r"C:\audit".into());
    tab.listing = std::sync::Arc::new((0..100_000).map(|i| {
        let name = format!("file-{i:06}.txt");
        FsEntry { path: std::path::PathBuf::from(format!(r"C:\audit\{}\{name}", "folder".repeat(15))), name,
            is_dir: false, size: 0, modified: None, archive: false, readonly: false, hidden: false, system: false }
    }).collect());
    let baseline = owned_bytes(&tab.listing);
    assert_eq!(tab.display_entries("", "name", true).len(), 100_000);
    let cache = &tab.display_cache.as_ref().unwrap().1;
    assert_eq!(cache[0], 0);
    println!("FsEntry_size={} listing_owned_bytes={} display_cache_owned_bytes={} index_bytes={}",
             std::mem::size_of::<FsEntry>(), baseline, cache.capacity() * std::mem::size_of::<usize>(), tab.listing.len() * std::mem::size_of::<usize>());
    assert_eq!(tab.display_entries("file-000", "name", true).len(), 1000);
    let cache = &tab.display_cache.as_ref().unwrap().1;
    println!("filtered_len={} filtered_capacity={} filtered_owned_bytes={}",cache.len(), cache.capacity(), cache.capacity() * std::mem::size_of::<usize>());
}
