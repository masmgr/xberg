//! `ExtractionConfig::max_content_chars` through the public extraction API: the content is cut at
//! exactly the limit with one `max_content_chars` warning, configs the limit cannot honour are
//! rejected, and a caller's token reused for another extraction carries no budget over.

#![cfg(feature = "tokio-runtime")]

mod helpers;
use helpers::extract_bytes_document;
use xberg::XbergError;
use xberg::cancellation::CancellationToken;
use xberg::core::config::{ExtractionConfig, OutputFormat};

const WARNING_SOURCE: &str = "max_content_chars";

fn limited(max_chars: Option<usize>) -> ExtractionConfig {
    ExtractionConfig {
        use_cache: false,
        max_content_chars: max_chars,
        ..Default::default()
    }
}

fn limit_warnings(result: &xberg::ExtractedDocument) -> usize {
    result
        .processing_warnings
        .iter()
        .filter(|warning| warning.source == WARNING_SOURCE)
        .count()
}

fn long_text() -> String {
    (0..200)
        .map(|line| format!("Line {line} of a long plain-text document."))
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn plain_text_is_cut_at_exactly_the_limit() {
    let text = long_text();
    let full = extract_bytes_document(text.as_bytes(), "text/plain", &limited(None))
        .await
        .expect("an unlimited extraction must succeed");
    assert!(full.content.chars().count() > 500);
    assert_eq!(limit_warnings(&full), 0);

    let result = extract_bytes_document(text.as_bytes(), "text/plain", &limited(Some(500)))
        .await
        .expect("a limited extraction must succeed");

    assert_eq!(result.content.chars().count(), 500);
    assert!(
        full.content.starts_with(&result.content),
        "the cut keeps the start of the text"
    );
    assert_eq!(limit_warnings(&result), 1, "{:?}", result.processing_warnings);
}

#[tokio::test]
async fn japanese_and_emoji_text_is_cut_on_a_character_boundary() {
    let text = "日本語の文書です。😀絵文字も含みます。".repeat(50);

    let result = extract_bytes_document(text.as_bytes(), "text/plain", &limited(Some(25)))
        .await
        .expect("a limited extraction must succeed");

    assert_eq!(result.content.chars().count(), 25);
    assert!(text.starts_with(&result.content));
    assert_eq!(limit_warnings(&result), 1);
}

#[tokio::test]
async fn configs_the_limit_cannot_honour_are_rejected() {
    let text = long_text();
    let mut rejected = vec![limited(Some(0))];
    for format in [
        OutputFormat::Json,
        OutputFormat::DocTags,
        OutputFormat::Custom("docx".into()),
    ] {
        rejected.push(ExtractionConfig {
            output_format: format,
            ..limited(Some(100))
        });
    }

    for config in rejected {
        let outcome = extract_bytes_document(text.as_bytes(), "text/plain", &config).await;
        assert!(
            matches!(outcome, Err(XbergError::Validation { .. })),
            "max_content_chars={:?} with {} must be rejected, got {outcome:?}",
            config.max_content_chars,
            config.output_format
        );
    }
}

#[tokio::test]
async fn a_reused_caller_token_carries_no_budget_into_the_next_extraction() {
    let text = long_text();
    let token = CancellationToken::new();

    let first = extract_bytes_document(
        text.as_bytes(),
        "text/plain",
        &ExtractionConfig {
            cancel_token: Some(token.clone()),
            ..limited(Some(100))
        },
    )
    .await
    .expect("a limited extraction must succeed");
    assert_eq!(first.content.chars().count(), 100);

    let second = extract_bytes_document(
        text.as_bytes(),
        "text/plain",
        &ExtractionConfig {
            cancel_token: Some(token.clone()),
            ..limited(None)
        },
    )
    .await
    .expect("an unlimited extraction must succeed");

    assert!(
        second.content.chars().count() > 100,
        "the second extraction is not limited"
    );
    assert_eq!(limit_warnings(&second), 0);
    assert!(!token.is_cancelled());
}

#[cfg(feature = "html")]
#[tokio::test]
async fn html_input_rendered_as_html_never_ends_inside_a_tag() {
    let html = format!(
        "<html><body>{}</body></html>",
        (0..50)
            .map(|paragraph| format!("<p class=\"body\">Paragraph {paragraph} &amp; more text.</p>"))
            .collect::<String>()
    );
    for max_chars in [40, 57, 63, 81, 120] {
        let config = ExtractionConfig {
            output_format: OutputFormat::Html,
            ..limited(Some(max_chars))
        };

        let result = extract_bytes_document(html.as_bytes(), "text/html", &config)
            .await
            .expect("a limited extraction must succeed");

        assert!(result.content.chars().count() <= max_chars);
        let open = result.content.rfind('<');
        assert!(
            open.is_none() || result.content.rfind('>') > open,
            "max_chars={max_chars}: the cut ends inside a tag: {:?}",
            result.content
        );
        let amp = result.content.rfind('&');
        assert!(
            amp.is_none() || result.content[amp.unwrap()..].contains(';'),
            "max_chars={max_chars}: the cut ends inside an entity: {:?}",
            result.content
        );
    }
}
