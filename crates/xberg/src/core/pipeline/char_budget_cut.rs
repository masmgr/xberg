//! `ExtractionConfig::max_content_chars`: cut the result at the character budget.
//!
//! The PDF page loops already stop reading once the pages they read fill the budget (see
//! `core::char_budget`), but they stop at a page or batch boundary, and every other format is
//! read in full. Two stages make the limit exact:
//!
//! 1. [`cut_before_processing`] runs right after derivation, on the plain text, before the
//!    post-processors, language detection, keywords and the LLM stages, so none of them works on
//!    text the result will not hold.
//! 2. [`apply`] runs after the output format is applied and before chunking, on the rendered
//!    `content`, so the returned text and the chunks hold no more than the budget. It reports the
//!    cut once, for both stages.
//!
//! `ExtractionConfig::validate_max_content_chars` restricts the limit to the formats for which
//! `OutputFormat::is_text_rendering` holds: a cut would leave any other rendering unparseable.

use crate::core::config::{ExtractionConfig, OutputFormat};
use crate::types::{ExtractedDocument, PageContent};

/// `ProcessingWarning::source` of the warning that reports the budget.
pub(crate) const WARNING_SOURCE: &str = "max_content_chars";
/// In the warning when a page loop stopped before its last page for the budget.
pub(crate) const STOPPED_EARLY: &str = "reading stopped early";
/// In the warning when the cut dropped the document structure or the Djot content.
pub(crate) const STRUCTURE_OMITTED: &str = "The document structure and Djot content were omitted";

/// What a stage cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Cut {
    /// Characters of `content` before the cut.
    original_chars: usize,
    /// The lowest page whose text the cut removed in part or in full, when pages are known.
    first_page_not_included: Option<u32>,
    /// Whether the document structure or the Djot content was dropped with the cut.
    dropped_structure: bool,
}

/// Stage 1: cut the plain `content` and the surfaces derived from it before post-processing.
pub(super) fn cut_before_processing(result: &mut ExtractedDocument, config: &ExtractionConfig) -> Option<Cut> {
    let max_chars = config.max_content_chars?;
    let offset = cut_offset(&result.content, max_chars)?;
    Some(cut(result, offset, PageCut::TruncateText))
}

/// Stage 2: cut the rendered `content` and report the limit in `processing_warnings`, together with
/// what [`cut_before_processing`] cut and the pages a page loop left unread for the budget.
pub(super) fn apply(result: &mut ExtractedDocument, config: &ExtractionConfig, early: Option<Cut>) {
    let Some(max_chars) = config.max_content_chars else {
        return;
    };
    let unread_from = config
        .cancel_token
        .as_ref()
        .and_then(crate::cancellation::CancellationToken::first_page_past_char_budget);
    let late = cut_offset(&result.content, max_chars).map(|offset| {
        let offset = if config.output_format == OutputFormat::Html {
            outside_markup(&result.content, offset)
        } else {
            offset
        };
        // The page text was cut on the plain text in stage 1; the boundaries now index a rendering.
        cut(result, offset, PageCut::DropOnly)
    });
    if early.is_none() && late.is_none() && unread_from.is_none() {
        return;
    }

    // Like a time-limit stop, a page loop the budget cut short leaves a document that must not be
    // kept by the operation-wide caches, which are not keyed by the budget.
    #[cfg(all(feature = "tokio-runtime", not(target_arch = "wasm32")))]
    if unread_from.is_some() {
        crate::core::extractor::cacheability::invalidate_current();
    }

    let message = warning_message(max_chars, early, late, unread_from);
    crate::core::diagnostics::push_warning(&mut result.processing_warnings, WARNING_SOURCE, message);
}

/// The one warning for both stages and the page loops.
fn warning_message(max_chars: usize, early: Option<Cut>, late: Option<Cut>, unread_from: Option<u32>) -> String {
    let cuts = || [early, late].into_iter().flatten();
    let original_chars = early.or(late).map(|cut| cut.original_chars);
    let mut message = match (original_chars, unread_from) {
        (Some(original), None) => {
            format!("Content was cut at max_content_chars ({max_chars} characters) from {original} characters.")
        }
        (Some(_), Some(_)) => format!(
            "Content reached max_content_chars ({max_chars} characters): {STOPPED_EARLY} and the text was cut there."
        ),
        (None, _) => format!("Content reached max_content_chars ({max_chars} characters): {STOPPED_EARLY}."),
    };
    let first_page = cuts()
        .filter_map(|cut| cut.first_page_not_included)
        .chain(unread_from)
        .min();
    if let Some(page) = first_page {
        message.push_str(&format!(" Page {page} is the first page not fully included."));
    }
    if cuts().any(|cut| cut.dropped_structure) {
        message.push_str(&format!(" {STRUCTURE_OMITTED}: they cannot be cut."));
    }
    message
}

/// How a cut treats `pages`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PageCut {
    /// The page text is the text the boundaries index: cut the boundary page's text to what is kept.
    TruncateText,
    /// The boundaries index a rendering of the page text: only drop the pages the cut removed.
    DropOnly,
}

/// Byte offset of the `max_chars`-th character of `text`: where a cut keeping `max_chars`
/// characters falls. `None` when `text` holds no more than `max_chars` characters.
fn cut_offset(text: &str, max_chars: usize) -> Option<usize> {
    text.char_indices().nth(max_chars).map(|(offset, _)| offset)
}

/// `offset` moved back out of an HTML tag or character reference it falls inside, so the kept
/// HTML never ends in a partial `<tag` or `&entity`. The cut then keeps fewer characters.
fn outside_markup(html: &str, offset: usize) -> usize {
    let mut offset = offset;
    let kept = &html[..offset];
    if let Some(open) = kept.rfind('<')
        && !kept[open..].contains('>')
    {
        offset = open;
    }
    let kept = &html[..offset];
    if let Some(amp) = kept.rfind('&')
        && kept[amp + 1..]
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '#')
    {
        offset = amp;
    }
    offset
}

/// `text` cut to its first `max_chars` characters.
fn truncate_chars(text: &mut String, max_chars: usize) {
    if let Some(offset) = cut_offset(text, max_chars) {
        text.truncate(offset);
    }
}

/// Keep the items whose text, in order, fits in `max_chars` whole; drop the rest.
fn keep_whole_within<T>(items: &mut Vec<T>, max_chars: usize, text: impl Fn(&T) -> &str) {
    let mut remaining = max_chars;
    items.retain(|item| {
        let chars = text(item).chars().count();
        let fits = chars <= remaining;
        remaining = if fits { remaining - chars } else { 0 };
        fits
    });
}

/// Keep the items whose text, in order, fits in `max_chars`, cutting the one that crosses it with
/// `cut_text` to the characters left; drop the rest. Returns the index (before the cut) of the
/// first item cut or dropped.
fn cut_within<T>(
    items: &mut Vec<T>,
    max_chars: usize,
    text: impl Fn(&T) -> &str,
    cut_text: impl Fn(&mut T, usize),
) -> Option<usize> {
    let mut remaining = max_chars;
    let mut first_not_whole = None;
    let mut index = 0;
    items.retain_mut(|item| {
        let current = index;
        index += 1;
        if remaining == 0 {
            first_not_whole.get_or_insert(current);
            return false;
        }
        let chars = text(item).chars().count();
        if chars > remaining {
            first_not_whole.get_or_insert(current);
            cut_text(item, remaining);
        }
        remaining = remaining.saturating_sub(chars);
        true
    });
    first_not_whole
}

/// Cut `result.content` at `offset` and bring every surface that carries its text in line.
fn cut(result: &mut ExtractedDocument, offset: usize, page_cut: PageCut) -> Cut {
    let original_chars = result.content.chars().count();
    let page_allowance = cut_page_boundaries(result, offset);
    result.content.truncate(offset);
    let kept_chars = result.content.chars().count();

    let mut first_page_not_included = page_allowance.as_ref().and_then(|allowance| allowance.first_cut_page);
    if let Some(pages) = result.pages.as_mut() {
        match &page_allowance {
            Some(allowance) => cut_pages_by_boundaries(pages, allowance, page_cut),
            None => first_page_not_included = cut_pages_by_budget(pages, kept_chars),
        }
    }
    if let Some(elements) = result.elements.as_mut() {
        cut_within(
            elements,
            kept_chars,
            |element| &element.text,
            |element, chars| truncate_chars(&mut element.text, chars),
        );
    }
    drop_text_surfaces_past_the_cut(result, first_page_not_included, kept_chars);

    Cut {
        original_chars,
        first_page_not_included,
        dropped_structure: drop_uncuttable_structure(result),
    }
}

/// What a cut leaves of each page, from the page byte ranges of `content`.
struct PageAllowance {
    /// Characters each kept page still holds, by page number.
    kept_chars: Vec<(u32, usize)>,
    /// The lowest page whose range the cut reaches.
    first_cut_page: Option<u32>,
}

/// Drop the page ranges a cut at `offset` removes and end the one it crosses there. `None` without
/// page ranges.
fn cut_page_boundaries(result: &mut ExtractedDocument, offset: usize) -> Option<PageAllowance> {
    let boundaries = result
        .metadata
        .pages
        .as_mut()
        .and_then(|structure| structure.boundaries.as_mut())?;
    let first_cut_page = boundaries
        .iter()
        .filter(|boundary| boundary.byte_end > offset)
        .map(|boundary| boundary.page_number)
        .min();
    boundaries.retain(|boundary| boundary.byte_start < offset);
    let kept_chars = boundaries
        .iter_mut()
        .map(|boundary| {
            boundary.byte_end = boundary.byte_end.min(offset);
            let kept = result
                .content
                .get(boundary.byte_start..boundary.byte_end)
                .map_or(0, |text| text.chars().count());
            (boundary.page_number, kept)
        })
        .collect();
    Some(PageAllowance {
        kept_chars,
        first_cut_page,
    })
}

/// Keep the pages the page ranges keep. A page with no range (an empty page has none) stays when
/// it comes before the first page the cut reached.
fn cut_pages_by_boundaries(pages: &mut Vec<PageContent>, allowance: &PageAllowance, page_cut: PageCut) {
    pages.retain_mut(|page| {
        match allowance
            .kept_chars
            .iter()
            .find(|(number, _)| *number == page.page_number)
        {
            Some(&(_, kept)) => {
                if page_cut == PageCut::TruncateText {
                    truncate_chars(&mut page.content, kept);
                }
                true
            }
            None => allowance.first_cut_page.is_none_or(|first| page.page_number < first),
        }
    });
}

/// Without page ranges, let the pages share the kept characters in order. Returns the first page
/// cut or dropped.
fn cut_pages_by_budget(pages: &mut Vec<PageContent>, kept_chars: usize) -> Option<u32> {
    let numbers: Vec<u32> = pages.iter().map(|page| page.page_number).collect();
    cut_within(
        pages,
        kept_chars,
        |page| &page.content,
        |page, chars| truncate_chars(&mut page.content, chars),
    )
    .map(|index| numbers[index])
}

/// Drop the tables and OCR elements past the cut: by page when the cut is known by page (page 0
/// means unknown), else by the kept characters.
fn drop_text_surfaces_past_the_cut(result: &mut ExtractedDocument, first_page: Option<u32>, kept_chars: usize) {
    match first_page {
        Some(first) => {
            result
                .tables
                .retain(|table| table.page_number == 0 || table.page_number < first);
            if let Some(ocr_elements) = result.ocr_elements.as_mut() {
                ocr_elements.retain(|element| element.page_number == 0 || element.page_number < first);
            }
        }
        None => {
            keep_whole_within(&mut result.tables, kept_chars, |table| &table.markdown);
            if let Some(ocr_elements) = result.ocr_elements.as_mut() {
                keep_whole_within(ocr_elements, kept_chars, |element| &element.text);
            }
        }
    }
}

/// Drop the document structure and the Djot content: a tree of nodes cannot be cut at a character
/// offset, and returning it whole would hand back the text the limit removed. Returns whether
/// either was there.
fn drop_uncuttable_structure(result: &mut ExtractedDocument) -> bool {
    let dropped_document = result.document.take().is_some();
    let dropped_djot = result.djot_content.take().is_some();
    dropped_document || dropped_djot
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::PageContent;
    use crate::types::page::page_boundary;

    fn page(number: u32, content: &str) -> PageContent {
        serde_json::from_value(serde_json::json!({ "page_number": number, "content": content }))
            .expect("a page with a number and content must deserialize")
    }

    fn config(max: Option<usize>) -> ExtractionConfig {
        ExtractionConfig {
            max_content_chars: max,
            ..Default::default()
        }
    }

    fn run(result: &mut ExtractedDocument, config: &ExtractionConfig) {
        let early = cut_before_processing(result, config);
        apply(result, config, early);
    }

    fn warnings(result: &ExtractedDocument) -> Vec<&str> {
        result
            .processing_warnings
            .iter()
            .filter(|warning| warning.source == WARNING_SOURCE)
            .map(|warning| warning.message.as_ref())
            .collect()
    }

    /// Page 1 "あいう", separator "\n\n", page 2 "えおか", separator, page 3 "きく".
    fn three_pages() -> ExtractedDocument {
        let mut result = ExtractedDocument {
            content: "あいう\n\nえおか\n\nきく".to_string(),
            pages: Some(vec![page(1, "あいう"), page(2, "えおか"), page(3, "きく")]),
            ..Default::default()
        };
        result.metadata.pages = Some(crate::types::PageStructure {
            total_count: 3,
            unit_type: crate::types::PageUnitType::Page,
            boundaries: Some(vec![
                page_boundary(0, 9, 1),
                page_boundary(11, 20, 2),
                page_boundary(22, 28, 3),
            ]),
            pages: None,
        });
        result
    }

    #[test]
    fn cuts_on_a_character_boundary_of_multibyte_text() {
        let mut result = ExtractedDocument {
            content: "日本語のテキスト😀です".to_string(),
            ..Default::default()
        };

        run(&mut result, &config(Some(9)));

        assert_eq!(result.content, "日本語のテキスト😀");
        assert_eq!(result.content.chars().count(), 9);
        let messages = warnings(&result);
        assert_eq!(messages.len(), 1, "one warning for both stages");
        assert!(messages[0].contains("max_content_chars (9 characters) from 11 characters"));
    }

    #[test]
    fn the_rendering_is_cut_after_the_plain_text_with_one_warning() {
        let mut result = ExtractedDocument {
            content: "Heading body text".to_string(),
            ..Default::default()
        };
        let config = ExtractionConfig {
            max_content_chars: Some(10),
            output_format: OutputFormat::Markdown,
            ..Default::default()
        };

        let early = cut_before_processing(&mut result, &config);
        assert_eq!(result.content, "Heading bo");
        // `apply_output_format` swaps the rendering in.
        result.content = "# Heading\n\nbody text".to_string();
        apply(&mut result, &config, early);

        assert_eq!(result.content, "# Heading\n");
        let messages = warnings(&result);
        assert_eq!(messages.len(), 1);
        assert!(messages[0].contains("from 17 characters"), "{}", messages[0]);
    }

    #[test]
    fn an_html_cut_never_ends_inside_a_tag_or_entity() {
        assert_eq!(outside_markup("<p>ab</p><p>c", 13), 13);
        assert_eq!(outside_markup("<p>ab</p><p class", 17), 9);
        assert_eq!(outside_markup("<p>a &amp", 9), 5);
        assert_eq!(outside_markup("<p>a & b", 8), 8);

        let mut result = ExtractedDocument {
            content: "<p>abc</p><p class=\"x\">def</p>".to_string(),
            ..Default::default()
        };
        let config = ExtractionConfig {
            max_content_chars: Some(15),
            output_format: OutputFormat::Html,
            ..Default::default()
        };
        apply(&mut result, &config, None);
        assert_eq!(result.content, "<p>abc</p>");
    }

    #[test]
    fn leaves_content_at_the_limit_untouched_and_silent() {
        let mut result = ExtractedDocument {
            content: "abcde".to_string(),
            ..Default::default()
        };

        run(&mut result, &config(Some(5)));

        assert_eq!(result.content, "abcde");
        assert!(result.processing_warnings.is_empty());
    }

    #[test]
    fn does_nothing_without_a_limit() {
        let mut result = ExtractedDocument {
            content: "x".repeat(10_000),
            ..Default::default()
        };

        run(&mut result, &config(None));

        assert_eq!(result.content.len(), 10_000);
        assert!(result.processing_warnings.is_empty());
    }

    #[test]
    fn page_boundaries_and_pages_follow_the_cut() {
        let mut result = three_pages();

        run(&mut result, &config(Some(6)));

        assert_eq!(result.content, "あいう\n\nえ");
        let pages = result.pages.as_ref().unwrap();
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].content, "あいう");
        assert_eq!(pages[1].content, "え");
        let boundaries = result.metadata.pages.as_ref().unwrap().boundaries.as_ref().unwrap();
        assert_eq!(boundaries.len(), 2);
        assert_eq!(boundaries[1].byte_end, result.content.len());
        assert!(warnings(&result)[0].contains("Page 2 is the first page not fully included"));
    }

    #[test]
    fn a_page_missing_from_the_boundaries_stays_when_it_comes_before_the_cut() {
        // Page 2 is empty and has no range; pages 1, 3 and 4 carry the text of `three_pages`.
        let mut result = three_pages();
        result.pages = Some(vec![page(1, "あいう"), page(2, ""), page(3, "えおか"), page(4, "きく")]);
        result.metadata.pages.as_mut().unwrap().boundaries = Some(vec![
            page_boundary(0, 9, 1),
            page_boundary(11, 20, 3),
            page_boundary(22, 28, 4),
        ]);

        run(&mut result, &config(Some(6)));

        let numbers: Vec<u32> = result
            .pages
            .as_ref()
            .unwrap()
            .iter()
            .map(|page| page.page_number)
            .collect();
        assert_eq!(numbers, [1, 2, 3], "the empty page 2 comes before the cut on page 3");
    }

    #[test]
    fn pages_without_boundaries_share_the_budget_in_order() {
        let mut result = ExtractedDocument {
            content: "aaaa bbbb cccc".to_string(),
            pages: Some(vec![page(1, "aaaa"), page(2, "bbbb"), page(3, "cccc")]),
            ..Default::default()
        };

        run(&mut result, &config(Some(6)));

        let pages = result.pages.as_ref().unwrap();
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[1].content, "bb");
        assert!(warnings(&result)[0].contains("Page 2 is the first page not fully included"));
    }

    #[test]
    fn elements_share_the_budget_in_order() {
        let element = |text: &str| crate::types::Element {
            element_id: String::new(),
            element_type: crate::types::ElementType::NarrativeText,
            text: text.to_string(),
            metadata: crate::types::ElementMetadata {
                page_number: None,
                filename: None,
                coordinates: None,
                element_index: None,
                additional: Default::default(),
            },
        };
        let mut result = ExtractedDocument {
            content: "first second third".to_string(),
            elements: Some(vec![element("first"), element("second"), element("third")]),
            ..Default::default()
        };

        run(&mut result, &config(Some(8)));

        let elements = result.elements.as_ref().unwrap();
        assert_eq!(elements.len(), 2);
        assert_eq!(elements[0].text, "first");
        assert_eq!(elements[1].text, "sec");
    }

    #[test]
    fn tables_and_structure_past_the_cut_are_dropped() {
        let table = |page_number: u32, markdown: &str| crate::types::Table {
            markdown: markdown.to_string(),
            page_number,
            ..Default::default()
        };
        let mut result = three_pages();
        result.tables = vec![table(1, "| a |"), table(2, "| b |"), table(3, "| c |")];
        result.document = Some(
            serde_json::from_value(serde_json::json!({ "nodes": [] })).expect("an empty structure must deserialize"),
        );

        run(&mut result, &config(Some(6)));

        let pages: Vec<u32> = result.tables.iter().map(|table| table.page_number).collect();
        assert_eq!(pages, [1], "tables on the cut page and after it are dropped");
        assert!(result.document.is_none());
        assert!(warnings(&result)[0].contains(STRUCTURE_OMITTED));
    }

    #[test]
    fn a_page_loop_stopped_by_the_budget_is_reported_even_when_nothing_is_cut() {
        let token = crate::cancellation::CancellationToken::new();
        token.note_page_past_char_budget(4);
        let config = ExtractionConfig {
            max_content_chars: Some(100),
            cancel_token: Some(token),
            ..Default::default()
        };
        let mut result = ExtractedDocument {
            content: "short".to_string(),
            ..Default::default()
        };

        run(&mut result, &config);

        assert_eq!(result.content, "short");
        let messages = warnings(&result);
        assert_eq!(messages.len(), 1);
        assert!(messages[0].contains(STOPPED_EARLY));
        assert!(messages[0].contains("Page 4 is the first page not fully included"));
    }
}
