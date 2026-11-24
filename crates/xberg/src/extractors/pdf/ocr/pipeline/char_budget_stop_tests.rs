//! `max_content_chars` in the OCR page loops: once the pages before a batch fill the budget, counted
//! in page order, no later page is sent to OCR. The pages left unread are not OCR failures and not
//! a time-limit stop.

use super::super::test_pdfs::build_minimal_multi_page_pdf;
use super::{extract_mixed_ocr_native_with_layout_pages, mixed_page_chars, native_chars_by_page};
use crate::cancellation::CancellationToken;
use crate::core::config::{ConcurrencyConfig, ExtractionConfig, OcrConfig};
use crate::types::page::page_boundary;
use serial_test::serial;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const BACKEND: &str = "char-budget-stop-test";
/// What the backend reads on every page.
const PAGE_TEXT: &str = "Plain readable words fill this scanned page here.";

struct CountingBackend {
    calls: AtomicUsize,
}

impl crate::plugins::Plugin for CountingBackend {
    fn name(&self) -> &str {
        BACKEND
    }

    fn version(&self) -> String {
        "1.0.0".to_string()
    }

    fn initialize(&self) -> crate::Result<()> {
        Ok(())
    }

    fn shutdown(&self) -> crate::Result<()> {
        Ok(())
    }
}

#[async_trait::async_trait]
impl crate::plugins::OcrBackend for CountingBackend {
    fn backend_type(&self) -> crate::plugins::OcrBackendType {
        crate::plugins::OcrBackendType::Custom
    }

    fn supports_language(&self, _lang: &str) -> bool {
        true
    }

    async fn process_image(
        &self,
        _image_bytes: &[u8],
        _config: &OcrConfig,
    ) -> crate::Result<crate::types::ExtractedDocument> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(crate::types::ExtractedDocument {
            content: PAGE_TEXT.to_string(),
            mime_type: std::borrow::Cow::Borrowed("text/plain"),
            ..Default::default()
        })
    }
}

struct RegisteredOcrBackendGuard;

impl Drop for RegisteredOcrBackendGuard {
    fn drop(&mut self) {
        let _ = crate::plugins::unregister_ocr_backend(BACKEND);
    }
}

fn page_chars() -> usize {
    PAGE_TEXT.chars().count()
}

/// One page per batch, so the budget is checked before every page.
fn setup(max_content_chars: Option<usize>) -> (Arc<CountingBackend>, ExtractionConfig, RegisteredOcrBackendGuard) {
    let backend = Arc::new(CountingBackend {
        calls: AtomicUsize::new(0),
    });
    crate::plugins::register_ocr_backend(backend.clone()).unwrap();
    let config = ExtractionConfig {
        cancel_token: Some(CancellationToken::new()),
        extraction_timeout_secs: None,
        max_content_chars,
        concurrency: Some(ConcurrencyConfig {
            max_threads: Some(1),
            ..Default::default()
        }),
        ocr: Some(OcrConfig {
            backend: BACKEND.to_string(),
            language: vec!["eng".to_string()],
            ..Default::default()
        }),
        ..Default::default()
    };
    (backend, config, RegisteredOcrBackendGuard)
}

fn token(config: &ExtractionConfig) -> &CancellationToken {
    config.cancel_token.as_ref().expect("the setup installs a token")
}

#[tokio::test]
#[serial]
async fn the_mixed_loop_stops_before_the_batch_after_the_budget_fills() {
    // Two pages of OCR text fill the budget: page 1 leaves it short, page 2 crosses it.
    let (backend, config, _registration) = setup(Some(page_chars() + 10));
    let pages = [1, 2, 3, 4, 5, 6];

    let run = extract_mixed_ocr_native_with_layout_pages(
        "",
        &[],
        (&pages, &[]),
        &build_minimal_multi_page_pdf(6),
        &config,
        None,
        (&[], &[]),
    )
    .await
    .expect("the pages read before the budget are the result");

    assert_eq!(
        backend.calls.load(Ordering::Relaxed),
        2,
        "no page after the budget is OCR'd"
    );
    assert_eq!(token(&config).first_page_past_char_budget(), Some(3));
    assert_eq!(
        token(&config).first_unread_page(),
        None,
        "a budget stop is not a time-limit stop"
    );
    let (_, accepted, _, _, _, _, _, _, _, failures) = run.output;
    assert!(
        failures.iter().all(|failure| failure.page < 3),
        "pages past the budget are not OCR failures: {failures:?}"
    );
    assert!(accepted.keys().all(|page| *page < 3), "{:?}", accepted.keys());
}

#[tokio::test]
#[serial]
async fn native_text_on_a_later_page_does_not_fill_the_budget_of_earlier_pages() {
    let (backend, config, _registration) = setup(Some(page_chars() + 10));
    // Only page 6 holds native text, far more than the budget.
    let native_text = "n".repeat(1_000);
    let boundaries = [page_boundary(0, native_text.len(), 6)];

    extract_mixed_ocr_native_with_layout_pages(
        &native_text,
        &boundaries,
        (&[1, 2, 3, 4, 5], &[]),
        &build_minimal_multi_page_pdf(6),
        &config,
        None,
        (&[], &[]),
    )
    .await
    .expect("the OCR pages before the budget are read");

    assert_eq!(
        backend.calls.load(Ordering::Relaxed),
        2,
        "the budget is counted in page order, so pages 1 and 2 are read"
    );
    assert_eq!(token(&config).first_page_past_char_budget(), Some(3));
}

#[tokio::test]
#[serial]
async fn native_text_on_an_earlier_page_counts_toward_the_budget() {
    let (backend, config, _registration) = setup(Some(60));
    // Page 1 is native with 100 characters, more than the budget, and is not OCR'd.
    let native_text = "n".repeat(100);
    let boundaries = [page_boundary(0, native_text.len(), 1)];

    extract_mixed_ocr_native_with_layout_pages(
        &native_text,
        &boundaries,
        (&[2, 3, 4], &[]),
        &build_minimal_multi_page_pdf(4),
        &config,
        None,
        (&[], &[]),
    )
    .await
    .expect("the native page is the result");

    assert_eq!(backend.calls.load(Ordering::Relaxed), 0);
    assert_eq!(token(&config).first_page_past_char_budget(), Some(2));
}

#[tokio::test]
#[serial]
async fn without_a_budget_every_page_is_read() {
    let (backend, config, _registration) = setup(None);

    extract_mixed_ocr_native_with_layout_pages(
        "",
        &[],
        (&[1, 2, 3, 4], &[]),
        &build_minimal_multi_page_pdf(4),
        &config,
        None,
        (&[], &[]),
    )
    .await
    .expect("every page is read");

    assert_eq!(backend.calls.load(Ordering::Relaxed), 4);
    assert_eq!(token(&config).first_page_past_char_budget(), None);
}

#[test]
fn a_mixed_page_counts_its_ocr_text_or_else_its_native_text() {
    // Page 1 "aaaa", page 2 "bb", page 3 "ccc"; page 4 has no range (a scan without a text layer),
    // and a boundary with page 0 must not panic.
    let native_text = "aaaa

bb

ccc";
    let boundaries = [
        page_boundary(0, 4, 1),
        page_boundary(6, 8, 2),
        page_boundary(10, 13, 3),
        page_boundary(0, 0, 0),
    ];
    let native_chars = native_chars_by_page(native_text, &boundaries);
    let mut ocr_results = ahash::AHashMap::new();
    let chars = |ocr_results: &ahash::AHashMap<u32, String>| {
        (0..4)
            .map(|idx| mixed_page_chars(&native_chars, ocr_results, idx))
            .collect::<Vec<_>>()
    };

    assert_eq!(chars(&ocr_results), [4, 2, 3, 0]);

    // OCR text replaces the native text of its page, and counts for a page with no range.
    ocr_results.insert(2, "OCR text".to_string());
    ocr_results.insert(4, "scan".to_string());
    assert_eq!(chars(&ocr_results), [4, 8, 3, 4]);
}

#[tokio::test]
#[serial]
async fn the_whole_document_loop_stops_before_the_batch_after_the_budget_fills() {
    let (backend, config, _registration) = setup(Some(page_chars() + 10));

    super::extract_with_ocr(
        Some(&build_minimal_multi_page_pdf(6)),
        None,
        #[cfg(feature = "layout-detection")]
        None,
        &config,
        None,
    )
    .await
    .expect("the pages read before the budget are the result");

    assert_eq!(
        backend.calls.load(Ordering::Relaxed),
        2,
        "no page after the budget is OCR'd"
    );
    assert_eq!(token(&config).first_page_past_char_budget(), Some(3));
    assert_eq!(token(&config).first_unread_page(), None);
}

#[tokio::test]
#[serial]
async fn the_whole_document_loop_reads_every_page_without_a_budget() {
    let (backend, config, _registration) = setup(None);

    super::extract_with_ocr(
        Some(&build_minimal_multi_page_pdf(4)),
        None,
        #[cfg(feature = "layout-detection")]
        None,
        &config,
        None,
    )
    .await
    .expect("every page is read");

    assert_eq!(backend.calls.load(Ordering::Relaxed), 4);
}
