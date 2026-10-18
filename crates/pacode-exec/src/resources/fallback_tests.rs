use super::*;

const VM_STAT: &str = "Mach Virtual Memory Statistics: (page size of 16384 bytes)
Pages free:                               1000.
Pages active:                             5000.
Pages inactive:                           2000.
Pages speculative:                         500.
Pages purgeable:                           100.
";

#[test]
fn vm_stat_available_counts_reclaimable_pages() {
    let v = parse_vm_stat(VM_STAT).expect("parses");
    assert_eq!(v.page_size, 16384);
    assert_eq!(v.available_bytes(), (1000 + 2000 + 500 + 100) * 16384);
    assert_eq!(parse_vm_stat("nothing here"), None);
}

#[test]
fn ps_rss_sums_kilobytes() {
    assert_eq!(parse_ps_rss_bytes("  100\n 200\n\nx\n"), 300 * 1024);
}

#[test]
fn unsupported_sampler_reports_itself() {
    let s = UnsupportedSampler;
    assert!(!s.supported());
    assert!(s.host(std::path::Path::new("/")).is_none());
}
