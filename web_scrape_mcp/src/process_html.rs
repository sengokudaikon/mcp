use html5ever::parse_document;
use html5ever::tendril::TendrilSink;
use markup5ever_rcdom::{Handle, NodeData, RcDom};

pub fn extract_text_from_html(html: &str) -> String {
    let dom = parse_document(RcDom::default(), Default::default())
        .from_utf8()
        .read_from(&mut html.as_bytes())
        .unwrap();

    let mut text = String::new();
    extract_text(&dom.document, &mut text);
    text
}

fn extract_text(handle: &Handle, text: &mut String) {
    let node = handle;
    match node.data {
        NodeData::Text { ref contents } => {
            text.push_str(&contents.borrow());
            text.push(' ');
        }
        _ => {}
    }

    for child in node.children.borrow().iter() {
        extract_text(child, text);
    }
}
