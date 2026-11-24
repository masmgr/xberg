//! Minimal PDFs for the OCR tests: page objects with a MediaBox and no content, which render to
//! blank rasters, so a test OCR backend decides what each page reads.

// Builds without the `ocr` feature compile only the tests that use the N-page builder. ~keep
#![cfg_attr(not(feature = "ocr"), allow(dead_code))]

/// A minimal N-page PDF (no content stream, just a MediaBox per page), built the
/// same way `crate::pdf::render::build_minimal_pdf_with_mediabox` builds a one-page
/// version: a hand-written Catalog/Pages/Page object graph with its own xref table.
pub(crate) fn build_minimal_multi_page_pdf(page_count: usize) -> Vec<u8> {
    // 200pt squares: small and fast to render.
    build_minimal_multi_page_pdf_with_media_box(page_count, 200, 200)
}

pub(crate) fn build_minimal_multi_page_pdf_with_media_box(page_count: usize, width_pt: u32, height_pt: u32) -> Vec<u8> {
    build_minimal_pdf_with_media_boxes(&vec![(width_pt, height_pt); page_count])
}

pub(crate) fn build_minimal_pdf_with_media_boxes(media_boxes: &[(u32, u32)]) -> Vec<u8> {
    let page_count = media_boxes.len();
    let mut buf = Vec::<u8>::new();
    buf.extend_from_slice(b"%PDF-1.4\n");
    let mut offsets = Vec::new();

    offsets.push(buf.len());
    buf.extend_from_slice(b"1 0 obj\n<</Type /Catalog /Pages 2 0 R>>\nendobj\n");

    offsets.push(buf.len());
    let kids: String = (0..page_count).map(|i| format!("{} 0 R ", 3 + i)).collect();
    buf.extend_from_slice(
        format!(
            "2 0 obj\n<</Type /Pages /Kids [{}] /Count {}>>\nendobj\n",
            kids.trim_end(),
            page_count
        )
        .as_bytes(),
    );

    for (i, (width_pt, height_pt)) in media_boxes.iter().enumerate() {
        offsets.push(buf.len());
        let obj_num = 3 + i;
        buf.extend_from_slice(
            format!(
                "{} 0 obj\n<</Type /Page /MediaBox [0 0 {} {}] /Parent 2 0 R>>\nendobj\n",
                obj_num, width_pt, height_pt
            )
            .as_bytes(),
        );
    }

    let xref_offset = buf.len();
    let total_objs = 2 + page_count + 1;
    buf.extend_from_slice(b"xref\n");
    buf.extend_from_slice(format!("0 {}\n", total_objs).as_bytes());
    buf.extend_from_slice(b"0000000000 65535 f \n");
    for off in &offsets {
        buf.extend_from_slice(format!("{:010} 00000 n \n", off).as_bytes());
    }
    buf.extend_from_slice(format!("trailer\n<</Size {} /Root 1 0 R>>\n", total_objs).as_bytes());
    buf.extend_from_slice(format!("startxref\n{}\n%%EOF\n", xref_offset).as_bytes());
    buf
}
