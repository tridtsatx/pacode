use super::*;

#[test]
fn shared_web_client_is_one_instance_per_process() {
    let first = shared_web_client().expect("shared client builds");
    let second = shared_web_client().expect("shared client builds");
    assert!(
        std::ptr::eq(first, second),
        "web tools must share one client"
    );
}
