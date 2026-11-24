//! `max_content_chars` takes part in the result cache key: a cached limited result is never served
//! for an unlimited extraction of the same input, or the other way round.
//!
//! In its own test binary because it points `XBERG_CACHE_DIR` at a temporary directory.

#![cfg(feature = "tokio-runtime")]

mod helpers;
use helpers::extract_bytes_document;
use tempfile::tempdir;
use xberg::core::config::ExtractionConfig;

/// `std::env::set_var` is `unsafe` in edition 2024; this binary contains a single test, so no
/// other thread is reading the environment concurrently.
#[allow(unsafe_code)]
fn set_env(key: &str, value: &str) {
    unsafe {
        std::env::set_var(key, value);
    }
}

#[tokio::test]
async fn the_limit_is_part_of_the_cache_key() {
    let temp = tempdir().expect("temp dir");
    set_env(
        "XBERG_CACHE_DIR",
        temp.path().join("cache").to_str().expect("utf-8 cache root"),
    );
    let text = (0..100)
        .map(|line| format!("Cached line {line}."))
        .collect::<Vec<_>>()
        .join("\n");
    let cached = |max_content_chars| ExtractionConfig {
        use_cache: true,
        cache_namespace: Some("max-content-chars".to_string()),
        max_content_chars,
        ..Default::default()
    };

    let limited = extract_bytes_document(text.as_bytes(), "text/plain", &cached(Some(50)))
        .await
        .expect("a limited extraction must succeed");
    let unlimited = extract_bytes_document(text.as_bytes(), "text/plain", &cached(None))
        .await
        .expect("an unlimited extraction must succeed");
    let limited_again = extract_bytes_document(text.as_bytes(), "text/plain", &cached(Some(50)))
        .await
        .expect("a limited extraction must succeed");

    assert_eq!(limited.content.chars().count(), 50);
    assert!(
        unlimited.content.chars().count() > 50,
        "the limited result must not be served from the cache for an unlimited extraction"
    );
    assert_eq!(limited_again.content, limited.content);
}
