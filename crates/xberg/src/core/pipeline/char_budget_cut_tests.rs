//! `max_content_chars` through the whole pipeline (`char_budget_cut`): the plain text is cut before the post-processors
//! see it, the rendering is cut again before chunking, and one warning reports both.

use super::*;
use crate::core::config::OutputFormat;
use crate::plugins::{Plugin, PostProcessor, ProcessingStage};
use crate::types::internal_builder::InternalDocumentBuilder;
use std::sync::{Arc, Mutex};

const MAX_CHARS: usize = 50;
const HEADING: &str = "Quarterly report";

/// A heading and paragraphs that hold far more than [`MAX_CHARS`] characters.
fn document() -> InternalDocument {
    let mut builder = InternalDocumentBuilder::new("test");
    builder.push_heading(1, HEADING, Some(1), None);
    for paragraph in 0..10 {
        builder.push_paragraph(
            &format!("Paragraph {paragraph} holds enough words to pass the character budget."),
            Vec::new(),
            Some(1),
            None,
        );
    }
    builder.build()
}

fn config(output_format: OutputFormat) -> ExtractionConfig {
    ExtractionConfig {
        output_format,
        max_content_chars: Some(MAX_CHARS),
        enable_quality_processing: false,
        use_cache: false,
        disable_ocr: true,
        ..Default::default()
    }
}

fn limit_warnings(result: &ExtractedDocument) -> Vec<&str> {
    result
        .processing_warnings
        .iter()
        .filter(|warning| warning.source == char_budget_cut::WARNING_SOURCE)
        .map(|warning| warning.message.as_ref())
        .collect()
}

async fn run(config: &ExtractionConfig, state: Arc<initialization::ProcessorRegistryState>) -> ExtractedDocument {
    run_pipeline_with_isolated_registry(document(), config, state)
        .await
        .expect("the pipeline must succeed")
}

async fn run_without_processors(config: &ExtractionConfig) -> ExtractedDocument {
    run(config, Arc::new(initialization::ProcessorRegistryState::new_isolated())).await
}

/// Records the length of `content` each time its stage runs.
struct RecordLength {
    name: &'static str,
    stage: ProcessingStage,
    seen: Arc<Mutex<Vec<(&'static str, usize)>>>,
}

impl Plugin for RecordLength {
    fn name(&self) -> &str {
        self.name
    }
}

#[async_trait::async_trait]
impl PostProcessor for RecordLength {
    async fn process(&self, result: &mut ExtractedDocument, _: &ExtractionConfig) -> Result<()> {
        self.seen
            .lock()
            .unwrap()
            .push((self.name, result.content.chars().count()));
        Ok(())
    }

    fn processing_stage(&self) -> ProcessingStage {
        self.stage
    }
}

#[tokio::test]
async fn every_post_processor_stage_sees_the_cut_text() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let state = Arc::new(initialization::ProcessorRegistryState::new_isolated());
    for (name, stage) in [
        ("record-early", ProcessingStage::Early),
        ("record-middle", ProcessingStage::Middle),
        ("record-late", ProcessingStage::Late),
    ] {
        state
            .register(Arc::new(RecordLength {
                name,
                stage,
                seen: Arc::clone(&seen),
            }))
            .unwrap();
    }

    let result = run(&config(OutputFormat::Plain), state).await;

    let seen = seen.lock().unwrap();
    let stages: Vec<&str> = seen.iter().map(|(name, _)| *name).collect();
    assert_eq!(stages, ["record-early", "record-middle", "record-late"]);
    for (name, chars) in seen.iter() {
        assert!(*chars <= MAX_CHARS, "{name} saw {chars} characters");
    }
    assert_eq!(result.content.chars().count(), MAX_CHARS);
    assert_eq!(limit_warnings(&result).len(), 1);
}

#[tokio::test]
async fn markdown_output_is_cut_once_and_keeps_its_rendering() {
    let result = run_without_processors(&config(OutputFormat::Markdown)).await;

    assert!(result.content.chars().count() <= MAX_CHARS, "{:?}", result.content);
    assert!(
        result.content.starts_with(&format!("# {HEADING}")),
        "the rendering must survive the plain-text cut: {:?}",
        result.content
    );
    assert!(
        result
            .processing_warnings
            .iter()
            .all(|warning| warning.source != "output_format"),
        "the rendering must not be discarded as stale: {:?}",
        result.processing_warnings
    );
    assert_eq!(limit_warnings(&result).len(), 1, "{:?}", result.processing_warnings);
}

#[tokio::test]
async fn html_output_never_ends_inside_a_tag() {
    let result = run_without_processors(&config(OutputFormat::Html)).await;

    assert!(result.content.chars().count() <= MAX_CHARS);
    let tail_open = result.content.rfind('<');
    let tail_close = result.content.rfind('>');
    assert!(
        tail_open.is_none() || tail_close > tail_open,
        "the cut ends inside a tag: {:?}",
        result.content
    );
    assert!(!result.content.ends_with('&'));
    assert_eq!(limit_warnings(&result).len(), 1);
}

#[tokio::test]
async fn the_document_structure_is_omitted_when_the_text_is_cut() {
    let config = ExtractionConfig {
        include_document_structure: true,
        ..config(OutputFormat::Plain)
    };

    let result = run_without_processors(&config).await;

    assert!(result.document.is_none());
    let warnings = limit_warnings(&result);
    assert_eq!(warnings.len(), 1);
    assert!(
        warnings[0].contains(char_budget_cut::STRUCTURE_OMITTED),
        "{}",
        warnings[0]
    );
}

#[tokio::test]
async fn a_document_under_the_limit_is_left_alone() {
    let config = ExtractionConfig {
        max_content_chars: Some(1_000_000),
        include_document_structure: true,
        ..config(OutputFormat::Markdown)
    };

    let result = run_without_processors(&config).await;

    assert!(result.document.is_some());
    assert!(limit_warnings(&result).is_empty());
}

#[cfg(feature = "chunking")]
#[tokio::test]
async fn chunks_cover_only_the_kept_text() {
    let config = ExtractionConfig {
        chunking: Some(crate::core::config::ChunkingConfig {
            max_characters: 20,
            overlap: 0,
            ..Default::default()
        }),
        ..config(OutputFormat::Plain)
    };

    let result = run_without_processors(&config).await;

    let chunks = result.chunks.as_ref().expect("chunking was requested");
    assert!(!chunks.is_empty());
    for chunk in chunks {
        assert!(
            chunk.metadata.byte_end <= result.content.len(),
            "chunk {} ends past the kept content",
            chunk.metadata.chunk_index
        );
        assert!(result.content.contains(chunk.content.trim()));
    }
    let chunk_chars: usize = chunks.iter().map(|chunk| chunk.content.chars().count()).sum();
    assert!(chunk_chars <= MAX_CHARS, "chunks hold {chunk_chars} characters");
}

/// The core entry points can be reached without `ExtractionConfig::validate`; they must reject a
/// limit they cannot honour themselves.
#[tokio::test]
async fn the_core_entry_point_rejects_a_limit_it_cannot_honour() {
    for config in [
        ExtractionConfig {
            max_content_chars: Some(0),
            ..config(OutputFormat::Plain)
        },
        config(OutputFormat::Json),
    ] {
        let outcome = crate::core::extractor::extract_bytes(b"some plain text", "text/plain", &config).await;
        assert!(
            matches!(outcome, Err(crate::XbergError::Validation { .. })),
            "{:?} with {} must be rejected, got {outcome:?}",
            config.max_content_chars,
            config.output_format
        );
    }
}
