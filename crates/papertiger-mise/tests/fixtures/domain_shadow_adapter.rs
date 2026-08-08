use std::io::{Read, Write};

fn main() {
    let mut request = Vec::new();
    std::io::stdin()
        .read_to_end(&mut request)
        .expect("read fixture request");
    assert!(!request.is_empty(), "fixture request must not be empty");
    let result = std::env::var("PAPERTIGER_MISE_DOMAIN_SHADOW_FIXTURE_RESULT")
        .expect("exact fixture result");
    std::io::stdout()
        .write_all(result.as_bytes())
        .expect("write fixture result");
}
