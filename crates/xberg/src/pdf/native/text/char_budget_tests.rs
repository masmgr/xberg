//! `max_content_chars`: the native text loop stops reading pages once the pages it read fill the
//! budget, and the result is cut at exactly the budget with a `max_content_chars` warning naming
//! the first page that was not fully included.

use super::NativeDocument;
use super::tests::build_paged_text_pdf;
use super::{NativePageTextOptions, extract_all_page_texts};
use crate::core::config::{ExtractionConfig, PageConfig};
use crate::core::extractor::extract_bytes;

const PAGE_COUNT: usize = 20;

fn limited_config(max_chars: usize) -> ExtractionConfig {
    ExtractionConfig {
        use_cache: false,
        disable_ocr: true,
        max_content_chars: Some(max_chars),
        pages: Some(PageConfig {
            extract_pages: true,
            ..Default::default()
        }),
        ..Default::default()
    }
}

#[test]
fn the_text_loop_stops_once_the_pages_read_fill_the_budget() {
    let pdf = build_paged_text_pdf(4, 10);
    let doc = NativeDocument::open_bytes(&pdf).expect("fixture must open");
    let (all_pages, _) = extract_all_page_texts(&doc.doc, NativePageTextOptions::default(), None)
        .expect("an unlimited extraction must succeed");
    let first_page_chars = all_pages[0].chars().count();

    let token = crate::cancellation::CancellationToken::new();
    let options = NativePageTextOptions {
        max_content_chars: Some(first_page_chars + 1),
        ..NativePageTextOptions::default()
    };
    let (read, _) = extract_all_page_texts(&doc.doc, options, Some(&token)).expect("a budget stop is not an error");

    assert_eq!(
        read.len(),
        all_pages.len(),
        "unread pages are padded to keep page order"
    );
    assert_eq!(
        read[..2],
        all_pages[..2],
        "the page that crosses the budget is still read whole"
    );
    assert!(
        read[2..].iter().all(String::is_empty),
        "no page after the budget is read"
    );
    assert_eq!(token.first_page_past_char_budget(), Some(3));
    assert_eq!(
        token.first_unread_page(),
        None,
        "a budget stop is not a time-limit stop"
    );
    assert!(!token.cut_short_by_time_limit());
}

#[tokio::test]
async fn extraction_returns_exactly_the_budget_with_a_warning() {
    let pdf = build_paged_text_pdf(PAGE_COUNT, 10);
    let unlimited = extract_bytes(
        &pdf,
        "application/pdf",
        &ExtractionConfig {
            use_cache: false,
            disable_ocr: true,
            ..Default::default()
        },
    )
    .await
    .expect("an unlimited extraction must succeed");
    let max_chars = 500;
    assert!(
        unlimited.content.chars().count() > max_chars * 4,
        "the fixture must exceed the budget"
    );

    let result = extract_bytes(&pdf, "application/pdf", &limited_config(max_chars))
        .await
        .expect("a limited extraction must succeed");

    assert_eq!(result.content.chars().count(), max_chars);
    assert!(
        unlimited.content.starts_with(&result.content),
        "the limited content must be the start of the full content"
    );
    let warning = result
        .processing_warnings
        .iter()
        .find(|warning| warning.source == crate::core::pipeline::char_budget_cut::WARNING_SOURCE)
        .expect("a cut result must carry the max_content_chars warning");
    assert!(
        warning
            .message
            .contains(crate::core::pipeline::char_budget_cut::STOPPED_EARLY),
        "{}",
        warning.message
    );
    assert!(
        !result
            .processing_warnings
            .iter()
            .any(|warning| warning.source == "timeout"),
        "a budget stop must not be reported as a time-limit stop"
    );
    let pages = result.pages.as_ref().expect("pages were requested");
    assert!(pages.len() < PAGE_COUNT, "pages past the budget must be dropped");
    let page_chars: usize = pages.iter().map(|page| page.content.chars().count()).sum();
    assert!(page_chars <= max_chars, "pages hold {page_chars} characters");
}

#[tokio::test]
async fn a_budget_above_the_document_changes_nothing() {
    let pdf = build_paged_text_pdf(2, 5);
    let unlimited = extract_bytes(
        &pdf,
        "application/pdf",
        &ExtractionConfig {
            use_cache: false,
            disable_ocr: true,
            ..Default::default()
        },
    )
    .await
    .expect("an unlimited extraction must succeed");

    let result = extract_bytes(&pdf, "application/pdf", &limited_config(1_000_000))
        .await
        .expect("a limited extraction must succeed");

    assert_eq!(result.content, unlimited.content);
    assert!(
        !result
            .processing_warnings
            .iter()
            .any(|warning| warning.source == crate::core::pipeline::char_budget_cut::WARNING_SOURCE)
    );
}
