//! `ExtractionConfig::max_content_chars` as the page loops count it.
//!
//! Every page loop that can stop early for the character budget (the native PDF text loop and
//! the OCR page loops) counts the same way through [`PageOrderBudget`]: the characters of the
//! pages before the next page or batch, in page order, so the text of a later page never fills
//! the budget of an earlier one. A loop that stops records the first page it left unread on the
//! extraction's token (`CancellationToken::note_page_past_char_budget`), and the pipeline's
//! `char_budget_cut` stage cuts the result at exactly the budget and reports it.

/// The characters of the pages a loop has read, counted in page order against the budget.
#[derive(Debug)]
pub(crate) struct PageOrderBudget {
    max_chars: usize,
    /// Pages (0-based) before this one are counted in `chars`.
    counted_pages: usize,
    chars: usize,
}

impl PageOrderBudget {
    /// The budget of `max_content_chars`, or `None` without a limit.
    pub(crate) fn new(max_content_chars: Option<usize>) -> Option<Self> {
        max_content_chars.map(|max_chars| Self {
            max_chars,
            counted_pages: 0,
            chars: 0,
        })
    }

    /// Count the pages not yet counted before `page_idx` (0-based), with `page_chars` giving the
    /// characters of one page, and report whether they fill the budget, so that `page_idx` and
    /// every page after it cannot reach the result. The pages before `page_idx` must be final.
    pub(crate) fn is_full_before(&mut self, page_idx: usize, page_chars: impl Fn(usize) -> usize) -> bool {
        if page_idx > self.counted_pages {
            self.chars += (self.counted_pages..page_idx).map(page_chars).sum::<usize>();
            self.counted_pages = page_idx;
        }
        self.chars >= self.max_chars
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_limit_means_no_budget() {
        assert!(PageOrderBudget::new(None).is_none());
    }

    #[test]
    fn pages_are_counted_once_in_page_order() {
        let pages = [4, 3, 5, 100];
        let mut budget = PageOrderBudget::new(Some(10)).unwrap();

        assert!(!budget.is_full_before(0, |idx| pages[idx]));
        assert!(
            !budget.is_full_before(2, |idx| pages[idx]),
            "pages 1-2 hold 7 characters"
        );
        // Asking again for an earlier page counts nothing twice.
        assert!(!budget.is_full_before(1, |idx| pages[idx]));
        assert!(
            budget.is_full_before(3, |idx| pages[idx]),
            "pages 1-3 hold 12 characters"
        );
    }

    #[test]
    fn a_later_page_never_fills_the_budget_of_an_earlier_one() {
        let pages = [0, 0, 1_000];
        let mut budget = PageOrderBudget::new(Some(10)).unwrap();

        assert!(!budget.is_full_before(2, |idx| pages[idx]));
    }
}
