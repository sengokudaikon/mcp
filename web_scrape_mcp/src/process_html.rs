use html2text::from_read;

pub fn extract_text_from_html(html: &str) -> String {
    from_read(html.as_bytes(), 80)
}
