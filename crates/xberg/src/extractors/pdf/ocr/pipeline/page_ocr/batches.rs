use super::super::*;
use super::*;

impl PageOcrContext<'_> {
    /// Render and read each batch of pages, post-processing every page it read, then hand the
    /// accumulated state to the assembly phase.
    pub(super) async fn run_batches(self) -> OcrRunResult<Self> {
        let total_pages = self.total_pages;
        let batch_size = self.batch_size;
        let mut this = self;
        // `max_content_chars`, counted over the pages in page order.
        let mut budget = crate::core::char_budget::PageOrderBudget::new(this.config.max_content_chars);
        for batch_start in (0..total_pages).step_by(batch_size) {
            if this.char_budget_full(&mut budget, batch_start) {
                break;
            }
            let (next, rendered) = this.render_batch(batch_start).await?;
            let (next, mut reads) = next.read_batch(batch_start, &rendered).await?;
            this = next.process_batch(batch_start, &rendered, &mut reads).await?;
        }
        #[cfg(feature = "layout-detection")]
        if let Some(model) = this.tatr_model.take() {
            crate::layout::return_tatr(model);
        }
        Ok(this)
    }

    /// Whether the pages before the batch at `batch_start` fill `max_content_chars`, so nothing
    /// that batch or a later one reads can reach the result. Records the batch's first page as
    /// the first page left unread when they do. ~keep
    fn char_budget_full(
        &self,
        budget: &mut Option<crate::core::char_budget::PageOrderBudget>,
        batch_start: usize,
    ) -> bool {
        let page_texts = &self.page_texts;
        let full = budget
            .as_mut()
            .is_some_and(|budget| budget.is_full_before(batch_start, |idx| page_texts[idx].chars().count()));
        if full && let Some(token) = self.config.cancel_token.as_ref() {
            token.note_page_past_char_budget((batch_start + 1) as u32);
        }
        full
    }
}
